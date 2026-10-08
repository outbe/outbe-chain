//! Real offline snapshot CLI and ordinary lifecycle acceptance evidence.
use crate::world::state::{
    OfflineSnapshotEvidence, SnapshotBlock, SnapshotCommandObservation, SnapshotFileRead,
    SnapshotLaunchObservation, SnapshotLogSlice, SnapshotManifestObservation,
    SnapshotNativeProgress, SnapshotNewJobObservation, SnapshotPriorFileObservation,
    SnapshotResultObservation, SnapshotValidationObservation, SnapshotWorkerAttribution,
    SnapshotWorkerOwner,
};
use eyre::{ensure, eyre};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SnapshotCheckStatus {
    Passed,
    Failed,
    Incomplete,
    NotRequested,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotCheckReport {
    pub selected: bool,
    pub status: SnapshotCheckStatus,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotObservedFrontiers {
    pub h: Option<SnapshotBlock>,
    pub e: Option<SnapshotBlock>,
    pub q: Option<SnapshotBlock>,
    pub p: Option<SnapshotBlock>,
    pub c_baseline: Option<SnapshotBlock>,
    pub c_previous: Option<SnapshotBlock>,
    pub c_current: Option<SnapshotBlock>,
}

/// Subset of actual public JSON, with the remaining report retained as raw bytes.
#[derive(Debug, Deserialize)]
pub(crate) struct SnapshotValidationReport {
    pub checks: BTreeMap<String, SnapshotCheckReport>,
    pub observed: SnapshotObservedFrontiers,
}

pub(crate) fn parse_snapshot_validation_report(
    bytes: &[u8],
) -> eyre::Result<SnapshotValidationReport> {
    let report: SnapshotValidationReport = serde_json::from_slice(bytes)?;
    for name in [
        "files",
        "provenance",
        "headers",
        "evm",
        "ce",
        "bodies",
        "ocomp",
    ] {
        let check = report
            .checks
            .get(name)
            .ok_or_else(|| eyre!("missing check {name}"))?;
        ensure!(
            check.selected != (check.status == SnapshotCheckStatus::NotRequested),
            "selection/status mismatch for {name}"
        );
    }
    ensure!(report.checks.len() == 7, "unexpected check names");
    if report.checks["bodies"].status == SnapshotCheckStatus::Passed {
        let q = report
            .observed
            .q
            .as_ref()
            .ok_or_else(|| eyre!("passed bodies without Q"))?;
        let p = report
            .observed
            .p
            .as_ref()
            .ok_or_else(|| eyre!("passed bodies without P"))?;
        ensure!(q == p, "body equality claimed for different Q/P identities");
    }
    // Incomplete is intentionally preserved. Parsing is not validation success.
    Ok(report)
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Narrow relational assertion for Task09 Tests-first 1, not full acceptance.
/// A passed assertion cannot replace signature/placement/role/public-action checks.
mod evidence;
pub(crate) use evidence::assert_snapshot_workflow;

pub(super) fn snapshot_option(argv: &[String], flag: &str) -> eyre::Result<String> {
    for (index, arg) in argv.iter().enumerate() {
        if arg == flag {
            return argv
                .get(index + 1)
                .cloned()
                .ok_or_else(|| eyre!("missing value for {flag}"));
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Ok(value.to_owned());
        }
    }
    Err(eyre!("ordinary node command has no {flag}"))
}

pub(super) fn canonical_snapshot_block(
    world: &crate::world::World,
    number: u64,
) -> eyre::Result<SnapshotBlock> {
    let hash = world
        .rpc
        .block_hash(world.validators.primary_port(), number)
        .ok_or_else(|| eyre!("missing upstream canonical block {number}"))?
        .parse::<alloy_primitives::B256>()?;
    Ok(SnapshotBlock {
        number,
        hash: hex::encode(hash),
    })
}

fn snapshot_file_sha256(path: &std::path::Path) -> eyre::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
    }
    Ok(hex::encode(hash.finalize()))
}

mod protocol;
use protocol::{
    actual_worker, native, newly_present, ordinary_launch, snapshot_command, worker_counters,
};

#[cfg(test)]
mod unit_tests;

mod commands;
use commands::{observe_snapshot_directory, resume_snapshot_prices, snapshot_now_millis};

mod native;
use native::*;

mod donor;

mod recipient;
use recipient::*;

mod workers;
pub(crate) use workers::SnapshotWorkerBeforeRequest;
use workers::*;

mod followup;
use followup::*;

mod owner;
use owner::*;

mod public;

mod damaged;
use damaged::*;

pub(super) use commands::{parse_recovery_record, run_snapshot_command};
pub(super) use protocol::successful_command;
pub(super) use recipient::{assert_local_committee_anchor, place_snapshot_payload};

mod followup_schedule;
use followup_schedule::*;
