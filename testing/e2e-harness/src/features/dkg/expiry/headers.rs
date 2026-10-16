//! Canonical header rows read from a stopped validator's durable storage.

use std::process::Output;

use alloy_primitives::B256;
use eyre::{ensure, eyre, Result};
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_primitives::OutbeHeader;
use serde::{Deserialize, Serialize};

use super::telemetry::Telemetry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HeaderWitness {
    pub height: u64,
    pub block_hash: B256,
    pub state_root: B256,
    pub parent_hash: B256,
}

/// The CLI prints tracing lines, followed by the two labelled JSON values.
/// A zero exit code alone is insufficient: missing static rows can still exit
/// zero, and storage-consistency warnings cannot be treated as valid evidence.
pub(super) fn header_output(output: Output, expected_height: u64) -> Result<HeaderWitness> {
    use eyre::WrapErr as _;

    header_output_inner(&output, expected_height)
        .and_then(|header| header_witness(&header))
        .wrap_err_with(|| {
            format!(
            "canonical header h{expected_height} rejected: exit={}; stdout_hex={}; stderr_hex={}",
            output.status,
            hex::encode(&output.stdout),
            hex::encode(&output.stderr)
        )
        })
}

pub(super) fn header_output_inner(output: &Output, expected_height: u64) -> Result<OutbeHeader> {
    ensure!(
        output.status.success(),
        "read-only header command failed: {}",
        output.status
    );
    let stdout =
        std::str::from_utf8(&output.stdout).map_err(|_| eyre!("header stdout is not UTF-8"))?;
    let stderr =
        std::str::from_utf8(&output.stderr).map_err(|_| eyre!("header stderr is not UTF-8"))?;
    let (prefix, body) = if let Some(body) = stdout.strip_prefix("Header\n") {
        ("", body)
    } else {
        stdout
            .split_once("\nHeader\n")
            .ok_or_else(|| eyre!("missing canonical Header output"))?
    };
    for line in prefix
        .lines()
        .chain(stderr.lines())
        .filter(|line| !line.trim().is_empty())
    {
        ensure!(
            line.contains(" INFO ")
                && !line.contains(" WARN ")
                && !line.contains(" ERROR ")
                && !line.contains("Inconsistent storage")
                && !line.contains('\u{1b}'),
            "unexpected read-only header command diagnostic"
        );
    }
    let (header, hash) = body
        .split_once("\nBlockHash\n")
        .ok_or_else(|| eyre!("missing canonical BlockHash output"))?;
    let header: OutbeHeader =
        serde_json::from_str(header).map_err(|_| eyre!("invalid canonical header JSON"))?;
    let hash: B256 =
        serde_json::from_str(hash).map_err(|_| eyre!("invalid canonical block hash JSON"))?;
    ensure!(
        header.inner.number == expected_height,
        "canonical header row has wrong height"
    );
    ensure!(
        header.inner.hash_slow() == hash,
        "canonical header hash does not recompute"
    );
    Ok(header)
}

pub(super) fn header_witness(header: &OutbeHeader) -> Result<HeaderWitness> {
    let artifacts = decode_outbe_block_artifacts(header.inner.extra_data.as_ref())
        .map_err(|_| eyre!("invalid canonical header artifact envelope"))?;
    ensure!(
        !matches!(
            artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        ),
        "DKG boundary activated during the frozen-target halt interval"
    );
    Ok(HeaderWitness {
        height: header.inner.number,
        block_hash: header.inner.hash_slow(),
        state_root: header.inner.state_root,
        parent_hash: header.inner.parent_hash,
    })
}

pub(super) fn check_chain(
    anchor: &HeaderWitness,
    headers: &[HeaderWitness],
    expiry: u64,
    records: &Telemetry,
) -> Result<()> {
    let expected_len = expiry
        .checked_sub(anchor.height)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| eyre!("invalid terminal header interval"))?;
    ensure!(
        u64::try_from(headers.len())? == expected_len && headers.first() == Some(anchor),
        "missing canonical rows or changed live checkpoint"
    );
    for pair in headers.windows(2) {
        ensure!(
            pair[0].height.checked_add(1) == Some(pair[1].height)
                && pair[1].parent_hash == pair[0].block_hash,
            "canonical header gap or parent-link mismatch"
        );
    }
    for header in headers {
        ensure!(
            records.finalized.get(&header.height) == Some(&header.block_hash),
            "canonical row is not bound to this process's finalization record"
        );
        if let Some(hash) = records.forkchoice.get(&header.height) {
            ensure!(
                *hash == header.block_hash,
                "canonical row disagrees with successful forkchoice"
            );
        }
    }
    Ok(())
}
