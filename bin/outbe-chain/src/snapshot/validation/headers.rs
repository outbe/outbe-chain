//! Audit retained canonical headers without reconstructing pruned history.

use std::{ops::RangeInclusive, path::PathBuf};

use alloy_consensus::Sealable;
use alloy_primitives::B256;
use eyre::{ensure, WrapErr};
use outbe_compressed_entities::{sealed_root, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME};
use outbe_primitives::{
    reshare_artifact::{decode_outbe_block_artifacts, CompressedEntitiesRootArtifact},
    OutbeHeader,
};
use reth_ethereum::provider::db::{cursor::DbCursorRO, tables, transaction::DbTx};
use reth_provider::{BlockHashReader, HeaderProvider, StaticFileSegment};

use super::super::native::RethReadOnlyView;

#[derive(Debug)]
pub(crate) struct HeaderAudit {
    /// Maximal contiguous intervals actually present in the native stores.
    pub intervals: Vec<RangeInclusive<u64>>,
    pub verified_headers: u64,
    /// Requested anchors outside retained intervals; no identity is fabricated.
    pub required_missing: Vec<u64>,
}

struct HeaderSegment {
    path: PathBuf,
    range: RangeInclusive<u64>,
}

/// Inspect every native header jar, including its physical row count. The provider's
/// range index alone cannot expose overlapping metadata or duplicate range keys.
fn retained_segments(view: &RethReadOnlyView) -> eyre::Result<Vec<HeaderSegment>> {
    let mut segments = Vec::new();
    for entry in std::fs::read_dir(view.static_files.directory())? {
        let entry = entry?;
        let name = entry.file_name();
        let Some((segment, expected)) = StaticFileSegment::parse_filename(&name.to_string_lossy())
        else {
            continue;
        };
        if segment != StaticFileSegment::Headers {
            continue;
        }
        ensure!(entry.file_type()?.is_file(), "header segment is not a file");
        ensure!(
            name == std::ffi::OsStr::new(&segment.filename(&expected)),
            "ambiguous header segment filename"
        );
        let path = entry.path();
        let jar = view
            .static_files
            .get_segment_provider_for_path(&path)?
            .ok_or_else(|| eyre::eyre!("missing native header segment {}", path.display()))?;
        let metadata = jar.user_header();
        ensure!(
            metadata.segment() == segment && metadata.expected_block_range() == expected,
            "header segment range metadata disagrees with filename"
        );
        let Some(range) = metadata.block_range() else {
            ensure!(
                jar.rows() == 0,
                "header segment has rows but no retained range"
            );
            continue;
        };
        let (start, end) = (range.start(), range.end());
        ensure!(
            start <= end && start >= expected.start() && end <= expected.end(),
            "invalid retained header range {start}..={end}"
        );
        let count = end
            .checked_sub(start)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(|| eyre::eyre!("header range length overflow"))?;
        ensure!(
            u64::try_from(jar.rows())? == count,
            "header segment rows disagree with retained range {start}..={end}"
        );
        segments.push(HeaderSegment {
            path,
            range: start..=end,
        });
    }
    segments.sort_unstable_by_key(|segment| *segment.range.start());
    for pair in segments.windows(2) {
        ensure!(
            pair[0].range.end() < pair[1].range.start(),
            "overlapping retained header segment ranges"
        );
    }
    Ok(segments)
}

/// Merge the native static intervals and both MDBX tables with at most one row
/// from each source in memory. A row missing inside a static interval is corrupt;
/// a gap in the union is retention, and is never scanned from genesis onwards.
pub(crate) fn verify_retained_headers(
    view: &RethReadOnlyView,
    required: &[u64],
) -> eyre::Result<HeaderAudit> {
    let segments = retained_segments(view)?;
    let mut segment_index = 0;
    let mut static_number = segments.first().map(|segment| *segment.range.start());
    let tx = view.read_transaction()?;
    let mut headers = tx.cursor_read::<tables::Headers<OutbeHeader>>()?;
    let mut canonical = tx.cursor_read::<tables::CanonicalHeaders>()?;
    let mut db_header = headers.first()?;
    let mut db_hash = canonical.first()?;
    let mut previous: Option<(u64, B256)> = None;
    let mut audit = HeaderAudit {
        intervals: Vec::new(),
        verified_headers: 0,
        required_missing: Vec::new(),
    };

    while let Some(number) = [
        static_number,
        db_header.as_ref().map(|(number, _)| *number),
        db_hash.as_ref().map(|(number, _)| *number),
    ]
    .into_iter()
    .flatten()
    .min()
    {
        let static_row = if static_number == Some(number) {
            let segment = &segments[segment_index];
            let jar = view
                .static_files
                .get_segment_provider_for_path(&segment.path)?
                .ok_or_else(|| eyre::eyre!("missing header segment for {number}"))?;
            let header = jar
                .header_by_number(number)?
                .ok_or_else(|| eyre::eyre!("missing header {number} inside static range"))?;
            let hash = jar.block_hash(number)?.ok_or_else(|| {
                eyre::eyre!("missing canonical hash {number} inside static range")
            })?;
            if number == *segment.range.end() {
                segment_index += 1;
                static_number = segments
                    .get(segment_index)
                    .map(|segment| *segment.range.start());
            } else {
                static_number = Some(number + 1);
            }
            Some((header, hash))
        } else {
            None
        };

        let stored_header = db_header
            .as_ref()
            .filter(|(key, _)| *key == number)
            .map(|(_, header)| header);
        let stored_hash = db_hash
            .as_ref()
            .filter(|(key, _)| *key == number)
            .map(|(_, hash)| *hash);
        let header = static_row
            .as_ref()
            .map(|(header, _)| header)
            .or(stored_header)
            .ok_or_else(|| eyre::eyre!("missing retained header {number} for canonical row"))?;
        let canonical_hash = static_row
            .as_ref()
            .map(|(_, hash)| *hash)
            .or(stored_hash)
            .ok_or_else(|| eyre::eyre!("missing canonical hash for retained header {number}"))?;
        ensure!(
            header.inner.number == number,
            "header number mismatch at {number}"
        );
        let hash = header.hash_slow();
        ensure!(
            hash == canonical_hash,
            "canonical header hash mismatch at {number}"
        );
        if let Some(stored) = stored_header {
            ensure!(
                stored.inner.number == number,
                "MDBX header number mismatch at {number}"
            );
            ensure!(
                stored.hash_slow() == hash,
                "conflicting MDBX/static header hash at {number}"
            );
        }
        if let Some(stored) = stored_hash {
            ensure!(stored == hash, "MDBX canonical hash mismatch at {number}");
        }
        if number == 0 {
            ensure!(
                hash == view.chain.genesis_hash(),
                "retained genesis differs from configured chain"
            );
        }
        if let Some((parent_number, parent_hash)) = previous {
            if parent_number.checked_add(1) == Some(number) {
                ensure!(
                    header.inner.parent_hash == parent_hash,
                    "retained header parent mismatch at {number}"
                );
                let start = *audit
                    .intervals
                    .last()
                    .expect("previous header interval")
                    .start();
                *audit
                    .intervals
                    .last_mut()
                    .expect("previous header interval") = start..=number;
            } else {
                audit.intervals.push(number..=number);
            }
        } else {
            audit.intervals.push(number..=number);
        }
        previous = Some((number, hash));
        audit.verified_headers = audit
            .verified_headers
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("verified header count overflow"))?;
        if stored_header.is_some() {
            db_header = headers.next()?;
        }
        if stored_hash.is_some() {
            db_hash = canonical.next()?;
        }
    }
    audit.required_missing = required
        .iter()
        .copied()
        .filter(|number| {
            let index = audit
                .intervals
                .partition_point(|range| range.end() < number);
            !audit
                .intervals
                .get(index)
                .is_some_and(|range| range.contains(number))
        })
        .collect();
    Ok(audit)
}

/// Decode the chain-native envelope. Structural absence remains distinguishable
/// from a malformed artifact and from the legitimate genesis empty-catalog seal.
pub(crate) fn read_header_ce_commitment(
    header: &OutbeHeader,
) -> eyre::Result<Option<CompressedEntitiesRootArtifact>> {
    let artifacts = decode_outbe_block_artifacts(&header.inner.extra_data)
        .wrap_err("decode header compressed-entity commitment")?;
    if let Some(commitment) = artifacts.compressed_entities_root {
        ensure!(
            commitment.commitment_scheme_version == ACTIVE_COMMITMENT_SCHEME,
            "unsupported header CE commitment scheme"
        );
    }
    Ok(artifacts.compressed_entities_root)
}

/// Bind the reconstructed CE marker to its exact retained header, not merely to
/// another header with the same root. Genesis uses the native empty catalog root.
pub(crate) fn verify_header_ce_marker(
    header: &OutbeHeader,
    marker: &FinalizedMarker,
) -> eyre::Result<()> {
    ensure!(
        marker.commitment_scheme_version == ACTIVE_COMMITMENT_SCHEME,
        "unsupported CE marker scheme"
    );
    ensure!(
        header.inner.number == marker.height,
        "CE marker/header height mismatch"
    );
    ensure!(
        header.hash_slow() == marker.block_hash,
        "CE marker/header block hash mismatch"
    );
    ensure!(
        header.inner.parent_hash == marker.parent_block_hash,
        "CE marker/header parent hash mismatch"
    );
    let root = if marker.height == 0 {
        sealed_root(B256::ZERO)?
    } else {
        read_header_ce_commitment(header)?
            .ok_or_else(|| eyre::eyre!("missing header CE commitment at {}", marker.height))?
            .r_sealed
    };
    ensure!(root == marker.new_root, "CE marker/header root mismatch");
    Ok(())
}
