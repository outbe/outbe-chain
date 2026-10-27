//! Runtime-log normalization and policy for LocalNet evidence.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr};
use serde_json::{json, Value};

use super::Localnet;
use crate::world::state::TeeLeaseShutdownV1;

mod deadlines;
mod discovery;
mod lease_full_node;
mod lease_guard;
mod sidecar_outage;
mod typed_events;
mod update_dkg;

use deadlines::legacy_request_deadline_cancellations;
pub(super) use discovery::first_runtime_log_line_containing;
#[cfg(test)]
use discovery::is_runtime_log;
use discovery::{collect_logs, fatal_at_runtime_severity, unexpected_log_line};
use lease_full_node::expected_tee_lease_full_node_shutdown_records;
use lease_guard::expected_tee_lease_guard_shutdown_records;
use sidecar_outage::expected_tee_sidecar_outage_records;
use typed_events::TypedPayloadEvents;
use update_dkg::{exact_expected_update_fatal, unsupported_version_fragment, ExpectationTally};

/// Runtime-log line indices (per log path) that an expectation accounts for.
type LogRecords = BTreeSet<(PathBuf, usize)>;

pub(crate) struct ExpectedTeeFailures<'a> {
    pub(crate) lease_guard_validator: Option<usize>,
    pub(crate) sidecar_outage: Option<(usize, &'a str, u64, alloy_primitives::B256)>,
    pub(crate) lease_guard_full_node: Option<&'a TeeLeaseShutdownV1>,
}

impl Localnet {
    /// At least one current committee process must actually reject the update.
    /// Not every replica executes a rejected local payload.
    pub(crate) fn unsupported_activation_reported(&mut self, version: u64) -> Result<bool> {
        let fragment = unsupported_version_fragment(version);
        let mut found = false;
        for index in 0..self.cfg.validators {
            let child = self
                .validators
                .get_mut(&index)
                .ok_or_else(|| eyre::eyre!("validator-{index} has no owned process"))?;
            eyre::ensure!(
                child.exit_status()?.is_none(),
                "validator-{index} exited before activation rejection check"
            );
            let pid = child.pid();
            let log = self.node_launch_log(index, pid)?;
            found |= log
                .lines()
                .any(|line| exact_expected_update_fatal(line, &fragment));
        }
        Ok(found)
    }

    /// Audits runtime health independently from functional scenario acceptance.
    pub(crate) fn audit_unexpected_logs(
        &self,
        unsupported_version: Option<u64>,
        expected_dkg_reveal: Option<&str>,
        expected_tee_failures: ExpectedTeeFailures<'_>,
    ) -> Result<LogAudit> {
        let result = audit_runtime_logs(
            &self.cfg.dir,
            AuditExpectations {
                validators: self.cfg.validators,
                unsupported_version,
                expected_dkg_reveal,
                expected_tee_lease_guard_shutdown_validator: expected_tee_failures
                    .lease_guard_validator,
                expected_tee_sidecar_outage: expected_tee_failures.sidecar_outage,
                expected_tee_lease_guard_shutdown_full_node: expected_tee_failures
                    .lease_guard_full_node,
            },
        );
        // Keep an unavailable audit as a failed audit result so the after hook
        // can publish already-captured public evidence before failing closed.
        Ok(result.unwrap_or_else(|error| LogAudit {
            findings: vec![format!("runtime log audit unavailable: {error:#}")],
            ..LogAudit::default()
        }))
    }
}

#[derive(Debug, Default)]
struct LogCounts {
    runtime_log_files: usize,
    fatal: usize,
    panic: usize,
    dkg_share_reveal: usize,
    vrf_alarm: usize,
    sgx_resource_exhaustion: usize,
    projection_fatal: usize,
    expected_update_fatal: usize,
    expected_dkg_reveal: usize,
    expected_request_deadline_cancellation: usize,
    expected_tee_lease_guard_shutdown_fatal: usize,
    expected_tee_sidecar_outage_fatal: usize,
}

impl LogCounts {
    fn observe(&mut self, line: &str) {
        let line = line.to_ascii_lowercase();
        self.fatal += usize::from(fatal_at_runtime_severity(&line));
        self.panic += usize::from(line.contains("panic"));
        self.dkg_share_reveal += usize::from(
            line.contains("dkg")
                && line.contains("share")
                && (line.contains("reveal") || line.contains("disclos")),
        );
        self.vrf_alarm += usize::from(line.contains("vrf") && line.contains("alarm"));
        self.sgx_resource_exhaustion += usize::from(
            line.contains("eagain") || line.contains("resource temporarily unavailable"),
        );
        self.projection_fatal += usize::from(line.contains("projection") && line.contains("fatal"));
    }

    fn json(&self) -> Value {
        json!({
            "runtime_log_files": self.runtime_log_files,
            "fatal": self.fatal,
            "panic": self.panic,
            "dkg_share_reveal": self.dkg_share_reveal,
            "vrf_alarm": self.vrf_alarm,
            "sgx_resource_exhaustion": self.sgx_resource_exhaustion,
            "projection_fatal": self.projection_fatal,
            "expected_update_fatal": self.expected_update_fatal,
            "expected_dkg_reveal": self.expected_dkg_reveal,
            "expected_request_deadline_cancellation": self.expected_request_deadline_cancellation,
            "expected_tee_lease_guard_shutdown_fatal": self.expected_tee_lease_guard_shutdown_fatal,
            "expected_tee_sidecar_outage_fatal": self.expected_tee_sidecar_outage_fatal,
        })
    }
}

/// Structured result retained as scenario evidence before logs are removed.
#[derive(Debug, Default)]
pub struct LogAudit {
    counts: LogCounts,
    findings: Vec<String>,
}

impl LogAudit {
    pub(crate) fn record_cleanup_failure(&mut self, error: eyre::Report) {
        self.findings
            .push(format!("scenario cleanup failed: {error:#}"));
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "clean": self.is_clean(),
            "counts": self.counts.json(),
            "findings": self.findings,
        })
    }

    pub(crate) fn ensure_clean(&self) -> eyre::Result<()> {
        if !self.findings.is_empty() {
            eyre::bail!(
                "unexpected fatal/alarm log records:\n{}",
                self.findings.join("\n")
            );
        }
        Ok(())
    }
}

fn audit_runtime_logs(root: &Path, expectations: AuditExpectations<'_>) -> Result<LogAudit> {
    if let Some(proof) = expectations.expected_tee_lease_guard_shutdown_full_node {
        for interval in &proof.sinks {
            let metadata =
                fs::metadata(&interval.path).wrap_err("stat retained lease shutdown sink")?;
            eyre::ensure!(
                metadata.dev() == interval.device && metadata.ino() == interval.inode,
                "retained lease shutdown sink was replaced: {}",
                interval.path.display()
            );
            eyre::ensure!(
                metadata.len() >= interval.end,
                "retained lease shutdown sink was truncated: {}",
                interval.path.display()
            );
        }
    }
    let mut paths = Vec::new();
    collect_logs(root, &mut paths)?;
    paths.sort();
    paths.dedup();
    let logs = paths
        .into_iter()
        .map(|path| {
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
            Ok((path, content))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(audit_loaded_logs_with_all_expectations(&logs, expectations))
}

#[cfg(test)]
fn audit_loaded_logs(logs: &[(PathBuf, String)]) -> LogAudit {
    audit_loaded_logs_with_expectations(logs, usize::MAX, None, None)
}

#[cfg(test)]
fn audit_loaded_logs_with_expectations(
    logs: &[(PathBuf, String)],
    validators: usize,
    unsupported_version: Option<u64>,
    expected_dkg_reveal: Option<&str>,
) -> LogAudit {
    audit_loaded_logs_with_all_expectations(
        logs,
        AuditExpectations {
            validators,
            unsupported_version,
            expected_dkg_reveal,
            expected_tee_lease_guard_shutdown_validator: None,
            expected_tee_sidecar_outage: None,
            expected_tee_lease_guard_shutdown_full_node: None,
        },
    )
}

#[cfg(test)]
fn audit_validator_shutdown(logs: &[(PathBuf, String)], validator: usize) -> LogAudit {
    audit_loaded_logs_with_all_expectations(
        logs,
        AuditExpectations {
            validators: 4,
            unsupported_version: None,
            expected_dkg_reveal: None,
            expected_tee_lease_guard_shutdown_validator: Some(validator),
            expected_tee_sidecar_outage: None,
            expected_tee_lease_guard_shutdown_full_node: None,
        },
    )
}

#[cfg(test)]
fn audit_full_node_shutdown(logs: &[(PathBuf, String)], proof: &TeeLeaseShutdownV1) -> LogAudit {
    audit_loaded_logs_with_all_expectations(
        logs,
        AuditExpectations {
            validators: 4,
            unsupported_version: None,
            expected_dkg_reveal: None,
            expected_tee_lease_guard_shutdown_validator: None,
            expected_tee_sidecar_outage: None,
            expected_tee_lease_guard_shutdown_full_node: Some(proof),
        },
    )
}

struct AuditExpectations<'a> {
    validators: usize,
    unsupported_version: Option<u64>,
    expected_dkg_reveal: Option<&'a str>,
    expected_tee_lease_guard_shutdown_validator: Option<usize>,
    expected_tee_sidecar_outage: Option<(usize, &'a str, u64, alloy_primitives::B256)>,
    expected_tee_lease_guard_shutdown_full_node: Option<&'a TeeLeaseShutdownV1>,
}

/// Exact records each declared expectation accepts, resolved before the scan.
struct ExpectedRecords {
    legacy_deadlines: LogRecords,
    legacy_deadline_cancellations: usize,
    tee_lease_shutdown: LogRecords,
    tee_sidecar_outage: LogRecords,
}

impl ExpectedRecords {
    fn resolve(
        logs: &[(PathBuf, String)],
        expectations: &AuditExpectations<'_>,
        findings: &mut Vec<String>,
    ) -> Self {
        let validators = expectations.validators;
        let legacy = legacy_request_deadline_cancellations(logs, validators);
        let mut tee_lease_shutdown = records_or_finding(
            expectations
                .expected_tee_lease_guard_shutdown_validator
                .map(|validator| {
                    expected_tee_lease_guard_shutdown_records(logs, validators, validator)
                }),
            findings,
        );
        let tee_sidecar_outage = records_or_finding(
            expectations
                .expected_tee_sidecar_outage
                .map(|(validator, tx, height, hash)| {
                    expected_tee_sidecar_outage_records(
                        logs, validators, validator, tx, height, hash,
                    )
                }),
            findings,
        );
        tee_lease_shutdown.extend(records_or_finding(
            expectations
                .expected_tee_lease_guard_shutdown_full_node
                .map(|full_node| {
                    expected_tee_lease_full_node_shutdown_records(logs, validators, full_node)
                }),
            findings,
        ));
        Self {
            legacy_deadlines: legacy.accepted_fatal_records,
            legacy_deadline_cancellations: legacy.cancellations,
            tee_lease_shutdown,
            tee_sidecar_outage,
        }
    }
}

fn records_or_finding(
    records: Option<Result<LogRecords, String>>,
    findings: &mut Vec<String>,
) -> LogRecords {
    records
        .unwrap_or_else(|| Ok(LogRecords::new()))
        .unwrap_or_else(|reason| {
            findings.push(reason);
            LogRecords::new()
        })
}

/// Which declared expectations account for one runtime-log line.
struct LineVerdict {
    legacy_deadline: bool,
    update: bool,
    reveal: bool,
    tee_lease_shutdown: bool,
    tee_sidecar_outage: bool,
}

impl LineVerdict {
    fn is_expected(&self) -> bool {
        [
            self.legacy_deadline,
            self.update,
            self.reveal,
            self.tee_lease_shutdown,
            self.tee_sidecar_outage,
        ]
        .contains(&true)
    }

    fn count(&self, counts: &mut LogCounts) {
        counts.expected_update_fatal += usize::from(self.update);
        counts.expected_dkg_reveal += usize::from(self.reveal);
        counts.expected_tee_lease_guard_shutdown_fatal += usize::from(self.tee_lease_shutdown);
        counts.expected_tee_sidecar_outage_fatal += usize::from(self.tee_sidecar_outage);
    }
}

fn audit_loaded_logs_with_all_expectations(
    logs: &[(PathBuf, String)],
    expectations: AuditExpectations<'_>,
) -> LogAudit {
    let mut findings = Vec::new();
    let mut counts = LogCounts {
        runtime_log_files: logs.len(),
        ..LogCounts::default()
    };
    let expected = ExpectedRecords::resolve(logs, &expectations, &mut findings);
    let mut tally = ExpectationTally::new(&expectations);
    let mut typed = TypedPayloadEvents::default();

    for (path, content) in logs {
        for (index, line) in content.lines().enumerate() {
            let location = format!("{}:{}", path.display(), index + 1);
            let record = (path.clone(), index);
            let verdict = LineVerdict {
                legacy_deadline: expected.legacy_deadlines.contains(&record),
                update: tally.accept_update(path, line),
                reveal: tally.accept_reveal(path, line),
                tee_lease_shutdown: expected.tee_lease_shutdown.contains(&record),
                tee_sidecar_outage: expected.tee_sidecar_outage.contains(&record),
            };
            verdict.count(&mut counts);
            if !verdict.is_expected() {
                counts.observe(line);
                if unexpected_log_line(line) {
                    findings.push(format!("{location}: {line}"));
                }
            }
            typed.observe(path, line, &location, &mut findings);
        }
    }

    let accepted_cancellations = typed.accepted_cancellations(&mut findings);
    tally.report_missing(&mut findings);
    counts.expected_request_deadline_cancellation =
        accepted_cancellations + expected.legacy_deadline_cancellations;
    LogAudit { counts, findings }
}

#[cfg(test)]
mod tests;
