//! Audit retained canonical headers without reconstructing pruned history.

use std::{ops::RangeInclusive, path::PathBuf};

use alloy_consensus::Sealable;
use alloy_primitives::B256;
use eyre::WrapErr;
use outbe_compressed_entities::{sealed_root, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME};
use outbe_primitives::{
    reshare_artifact::{decode_outbe_block_artifacts, CompressedEntitiesRootArtifact},
    OutbeHeader,
};
use reth_ethereum::provider::db::{cursor::DbCursorRO, tables, transaction::DbTx};
use reth_provider::{BlockHashReader, HeaderProvider, StaticFileSegment};

use super::super::native::RethReadOnlyView;
use crate::errors::{require, SnapshotError};

#[derive(Debug)]
pub(crate) struct HeaderAudit {
    /// Maximal contiguous intervals actually present in the native stores.
    pub intervals: Vec<RangeInclusive<u64>>,
    pub verified_headers: u64,
    /// Requested anchors outside retained intervals. No identity is fabricated.
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
        require(
            entry.file_type()?.is_file(),
            SnapshotError::HeaderSegmentNotFile,
        )?;
        require(
            name == std::ffi::OsStr::new(&segment.filename(&expected)),
            SnapshotError::AmbiguousHeaderSegmentFilename,
        )?;
        let path = entry.path();
        let jar = view
            .static_files
            .get_segment_provider_for_path(&path)?
            .ok_or_else(|| SnapshotError::MissingNativeHeaderSegment {
                path: path.display().to_string(),
            })?;
        let metadata = jar.user_header();
        require(
            metadata.segment() == segment && metadata.expected_block_range() == expected,
            SnapshotError::HeaderSegmentMetadataMismatch,
        )?;
        let Some(range) = metadata.block_range() else {
            require(
                jar.rows() == 0,
                SnapshotError::HeaderSegmentRowsWithoutRange,
            )?;
            continue;
        };
        let (start, end) = (range.start(), range.end());
        require(
            start <= end && start >= expected.start() && end <= expected.end(),
            SnapshotError::InvalidRetainedHeaderRange { start, end },
        )?;
        let count = end
            .checked_sub(start)
            .and_then(|span| span.checked_add(1))
            .ok_or(SnapshotError::HeaderRangeLengthOverflow)?;
        require(
            u64::try_from(jar.rows())? == count,
            SnapshotError::HeaderSegmentRowsDisagreeWithRange { start, end },
        )?;
        segments.push(HeaderSegment {
            path,
            range: start..=end,
        });
    }
    segments.sort_unstable_by_key(|segment| *segment.range.start());
    for pair in segments.windows(2) {
        require(
            pair[0].range.end() < pair[1].range.start(),
            SnapshotError::OverlappingHeaderSegmentRanges,
        )?;
    }
    Ok(segments)
}

fn read_static_header_row(
    view: &RethReadOnlyView,
    segments: &[HeaderSegment],
    segment_index: &mut usize,
    static_number: &mut Option<u64>,
    number: u64,
) -> eyre::Result<Option<(OutbeHeader, B256)>> {
    if *static_number != Some(number) {
        return Ok(None);
    }
    let segment = &segments[*segment_index];
    let jar = view
        .static_files
        .get_segment_provider_for_path(&segment.path)?
        .ok_or(SnapshotError::MissingHeaderSegmentForBlock { number })?;
    let header = jar
        .header_by_number(number)?
        .ok_or(SnapshotError::MissingHeaderInsideStaticRange { number })?;
    let hash = jar
        .block_hash(number)?
        .ok_or(SnapshotError::MissingCanonicalHashInsideStaticRange { number })?;
    if number == *segment.range.end() {
        *segment_index += 1;
        *static_number = segments
            .get(*segment_index)
            .map(|segment| *segment.range.start());
    } else {
        *static_number = Some(number + 1);
    }
    Ok(Some((header, hash)))
}

fn verify_merged_header_row<'a>(
    view: &RethReadOnlyView,
    number: u64,
    static_row: Option<&'a (OutbeHeader, B256)>,
    stored_header: Option<&'a OutbeHeader>,
    stored_hash: Option<B256>,
) -> eyre::Result<(&'a OutbeHeader, B256)> {
    let header = static_row
        .map(|(header, _)| header)
        .or(stored_header)
        .ok_or(SnapshotError::MissingRetainedHeaderForCanonicalRow { number })?;
    let canonical_hash = static_row
        .map(|(_, hash)| *hash)
        .or(stored_hash)
        .ok_or(SnapshotError::MissingCanonicalHashForRetainedHeader { number })?;
    require(
        header.inner.number == number,
        SnapshotError::HeaderNumberMismatch { number },
    )?;
    let hash = header.hash_slow();
    require(
        hash == canonical_hash,
        SnapshotError::CanonicalHeaderHashMismatch { number },
    )?;
    if let Some(stored) = stored_header {
        require(
            stored.inner.number == number,
            SnapshotError::MdbxHeaderNumberMismatch { number },
        )?;
        require(
            stored.hash_slow() == hash,
            SnapshotError::ConflictingMdbxStaticHeaderHash { number },
        )?;
    }
    if let Some(stored) = stored_hash {
        require(
            stored == hash,
            SnapshotError::MdbxCanonicalHashMismatch { number },
        )?;
    }
    if number == 0 {
        require(
            hash == view.chain.genesis_hash(),
            SnapshotError::RetainedGenesisMismatch,
        )?;
    }
    Ok((header, hash))
}

fn record_header_interval(
    audit: &mut HeaderAudit,
    previous: &mut Option<(u64, B256)>,
    number: u64,
    hash: B256,
    parent_hash: B256,
) -> eyre::Result<()> {
    if let Some((parent_number, expected_parent_hash)) = *previous {
        if parent_number.checked_add(1) == Some(number) {
            require(
                parent_hash == expected_parent_hash,
                SnapshotError::RetainedHeaderParentMismatch { number },
            )?;
            let last = audit
                .intervals
                .last_mut()
                .expect("previous header interval");
            *last = *last.start()..=number;
        } else {
            audit.intervals.push(number..=number);
        }
    } else {
        audit.intervals.push(number..=number);
    }
    *previous = Some((number, hash));
    audit.verified_headers = audit
        .verified_headers
        .checked_add(1)
        .ok_or(SnapshotError::VerifiedHeaderCountOverflow)?;
    Ok(())
}

fn missing_required_headers(intervals: &[RangeInclusive<u64>], required: &[u64]) -> Vec<u64> {
    required
        .iter()
        .copied()
        .filter(|number| {
            let index = intervals.partition_point(|range| range.end() < number);
            !intervals
                .get(index)
                .is_some_and(|range| range.contains(number))
        })
        .collect()
}

/// Merge the native static intervals and both MDBX tables with at most one row
/// from each source in memory. A row missing inside a static interval is corrupt.
/// A gap in the union is retention, and the merge never scans it from genesis onwards.
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
        let static_row = read_static_header_row(
            view,
            &segments,
            &mut segment_index,
            &mut static_number,
            number,
        )?;
        let stored_header = db_header
            .as_ref()
            .filter(|(key, _)| *key == number)
            .map(|(_, header)| header);
        let stored_hash = db_hash
            .as_ref()
            .filter(|(key, _)| *key == number)
            .map(|(_, hash)| *hash);
        let (header, hash) = verify_merged_header_row(
            view,
            number,
            static_row.as_ref(),
            stored_header,
            stored_hash,
        )?;
        record_header_interval(
            &mut audit,
            &mut previous,
            number,
            hash,
            header.inner.parent_hash,
        )?;
        if stored_header.is_some() {
            db_header = headers.next()?;
        }
        if stored_hash.is_some() {
            db_hash = canonical.next()?;
        }
    }
    audit.required_missing = missing_required_headers(&audit.intervals, required);
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
        require(
            commitment.commitment_scheme_version == ACTIVE_COMMITMENT_SCHEME,
            SnapshotError::UnsupportedHeaderCeScheme,
        )?;
    }
    Ok(artifacts.compressed_entities_root)
}

/// Bind the reconstructed CE marker to its exact retained header, not merely to
/// another header with the same root. Genesis uses the native empty catalog root.
pub(crate) fn verify_header_ce_marker(
    header: &OutbeHeader,
    marker: &FinalizedMarker,
) -> eyre::Result<()> {
    require(
        marker.commitment_scheme_version == ACTIVE_COMMITMENT_SCHEME,
        SnapshotError::UnsupportedCeMarkerScheme,
    )?;
    require(
        header.inner.number == marker.height,
        SnapshotError::CeMarkerHeightMismatch,
    )?;
    require(
        header.hash_slow() == marker.block_hash,
        SnapshotError::CeMarkerBlockHashMismatch,
    )?;
    require(
        header.inner.parent_hash == marker.parent_block_hash,
        SnapshotError::CeMarkerParentHashMismatch,
    )?;
    let root = if marker.height == 0 {
        sealed_root(B256::ZERO)?
    } else {
        read_header_ce_commitment(header)?
            .ok_or(SnapshotError::MissingHeaderCeCommitment {
                height: marker.height,
            })?
            .r_sealed
    };
    require(root == marker.new_root, SnapshotError::CeMarkerRootMismatch)?;
    Ok(())
}
