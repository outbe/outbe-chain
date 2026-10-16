//! Observations from explicitly selected offline checks.
//!
//! This report is not startup authority or a readiness attestation. Passed
//! checks describe only their requested integrity/relations. They do not attest
//! untested genesis state, DKG freeze state, or upstream epoch-history inputs.
//! Selection parsing, prerequisite expansion, and source validation belong to
//! the orchestrator. This model performs no I/O and validates no native heights.

use std::collections::{BTreeMap, BTreeSet};

use outbe_snapshot::manifest::BlockIdentity;
use serde::Serialize;

pub(crate) const MAX_DIAGNOSTIC_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckName {
    Files,
    Provenance,
    Headers,
    Evm,
}

impl CheckName {
    pub(crate) const ALL: [Self; 4] = [Self::Files, Self::Provenance, Self::Headers, Self::Evm];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckStatus {
    Passed,
    Failed,
    Incomplete,
    NotRequested,
}

#[derive(Debug, Serialize)]
pub(crate) struct CheckReport {
    selected: bool,
    pub(crate) status: CheckStatus,
    /// One bounded observation, replaced by each subsequent record call.
    pub(crate) diagnostic: Option<String>,
}

/// Native frontiers are independent observations, never normalized to one cut.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ObservedFrontiers {
    pub(crate) h: Option<BlockIdentity>,
    pub(crate) e: Option<BlockIdentity>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RetainedRange {
    pub(crate) domain: String,
    pub(crate) start: u64,
    pub(crate) end_inclusive: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RequiredHeight {
    pub(crate) domain: String,
    pub(crate) height: u64,
}

/// The canonical interval is [start, end_exclusive). Visited may be partial.
/// These observations do not independently confer a Passed check status.
#[derive(Debug, Serialize)]
pub(crate) struct InventoryBounds {
    pub(crate) name: String,
    pub(crate) start: u64,
    pub(crate) end_exclusive: u64,
    pub(crate) visited: u64,
}

/// Cryptographic validity and configured signer trust are distinct observations.
/// None means no claim was established, including when no signer was configured.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ProvenanceObservation {
    pub(crate) signature_valid: Option<bool>,
    /// Authenticated compressed public key in the native hexadecimal spelling.
    pub(crate) signer: Option<String>,
    pub(crate) expected_signer_match: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ValidationReport {
    /// Known source/configuration paths for safe optional report publication.
    /// Internal observations only. They are never serialized as validation authority.
    #[serde(skip)]
    pub(crate) protected_paths: outbe_snapshot::layout::ProtectedPaths,
    checks: BTreeMap<CheckName, CheckReport>,
    pub(crate) observed: ObservedFrontiers,
    pub(crate) retained_ranges: Vec<RetainedRange>,
    pub(crate) required_missing: Vec<RequiredHeight>,
    pub(crate) inventory_bounds: Vec<InventoryBounds>,
    pub(crate) provenance: ProvenanceObservation,
}

impl ValidationReport {
    /// `selected` already includes every prerequisite chosen by the orchestrator.
    pub(crate) fn new(selected: impl IntoIterator<Item = CheckName>) -> Self {
        let selected: BTreeSet<_> = selected.into_iter().collect();
        let checks = CheckName::ALL
            .into_iter()
            .map(|check| {
                let selected = selected.contains(&check);
                (
                    check,
                    CheckReport {
                        selected,
                        status: if selected {
                            CheckStatus::Incomplete
                        } else {
                            CheckStatus::NotRequested
                        },
                        diagnostic: None,
                    },
                )
            })
            .collect();
        Self {
            protected_paths: outbe_snapshot::layout::ProtectedPaths::default(),
            checks,
            observed: ObservedFrontiers::default(),
            retained_ranges: Vec::new(),
            required_missing: Vec::new(),
            inventory_bounds: Vec::new(),
            provenance: ProvenanceObservation::default(),
        }
    }

    pub(crate) fn check(&self, check: CheckName) -> &CheckReport {
        self.checks
            .get(&check)
            .expect("all fixed checks are initialized")
    }

    /// Unselected domains remain NotRequested. Their data cannot change the
    /// outcome or be presented as verified by a report for another selection.
    pub(crate) fn record(
        &mut self,
        check: CheckName,
        status: CheckStatus,
        diagnostic: Option<&str>,
    ) {
        let entry = self
            .checks
            .get_mut(&check)
            .expect("all fixed checks are initialized");
        if !entry.selected {
            return;
        }
        entry.status = status;
        entry.diagnostic = diagnostic.map(|text| text.chars().take(MAX_DIAGNOSTIC_CHARS).collect());
    }

    pub(crate) fn success(&self) -> bool {
        let mut selected = self
            .checks
            .values()
            .filter(|entry| entry.selected)
            .peekable();
        selected.peek().is_some() && selected.all(|entry| entry.status == CheckStatus::Passed)
    }
}

pub(super) fn failure_status(error: &eyre::Report) -> CheckStatus {
    if error.downcast_ref::<super::Incomplete>().is_some()
        || error.chain().any(|source| {
            source
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
    {
        CheckStatus::Incomplete
    } else {
        CheckStatus::Failed
    }
}

pub(super) fn record_failure(
    report: &mut ValidationReport,
    check: CheckName,
    error: &eyre::Report,
) {
    report.record(check, failure_status(error), Some(&format!("{error:#}")));
}

pub(super) fn record_outcome<T>(
    report: &mut ValidationReport,
    check: CheckName,
    result: &eyre::Result<T>,
) {
    match result {
        Ok(_) => report.record(check, CheckStatus::Passed, None),
        Err(error) => record_failure(report, check, error),
    }
}

pub(super) fn fail_selected(
    report: &mut ValidationReport,
    checks: &[CheckName],
    error: &eyre::Report,
) {
    for check in checks {
        // Preserve an already reported primary contradiction over secondary errors.
        if report.check(*check).status != CheckStatus::Failed {
            record_failure(report, *check, error);
        }
    }
}

pub(super) fn block_on(report: &mut ValidationReport, check: CheckName, prerequisite: CheckName) {
    if report.check(check).status == CheckStatus::Failed {
        return;
    }
    let status = report.check(prerequisite).status;
    if status != CheckStatus::Passed {
        report.record(
            check,
            if status == CheckStatus::Failed {
                CheckStatus::Failed
            } else {
                CheckStatus::Incomplete
            },
            Some(&format!("required {prerequisite:?} check is {status:?}")),
        );
    }
}
