//! Runtime-log normalization and policy for LocalNet evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use eyre::{Result, WrapErr};
use outbe_primitives::runtime_audit_v1::{
    BODY_READ_REQUEST_DEADLINE, EVENT_FIELD, FAILURE_KIND_FIELD, PAYLOAD_EXECUTION_FAILED,
    PROCESS_INSTANCE_FIELD, PROPOSAL_VIEW_CANCELLED, SCHEMA_FIELD, SCHEMA_VERSION,
};
use serde_json::{json, Value};

use super::Localnet;
use crate::world::state::TeeLeaseShutdownV1;

impl Localnet {
    /// Audits runtime health independently from functional scenario acceptance.
    pub fn audit_unexpected_logs(
        &self,
        unsupported_version: Option<u64>,
        expected_dkg_reveal: Option<&str>,
        expected_ocomp_full_node_mismatch: Option<B256>,
        expected_tee_lease_guard_shutdown_validator: Option<usize>,
        expected_tee_lease_guard_shutdown_full_node: Option<&TeeLeaseShutdownV1>,
    ) -> Result<LogAudit> {
        let result = audit_runtime_logs(
            &self.cfg.dir,
            self.cfg.validators,
            unsupported_version,
            expected_dkg_reveal,
            expected_ocomp_full_node_mismatch,
            expected_tee_lease_guard_shutdown_validator,
            expected_tee_lease_guard_shutdown_full_node,
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
    expected_ocomp_full_node_mismatch: usize,
    expected_tee_lease_guard_shutdown_fatal: usize,
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
            "expected_ocomp_full_node_mismatch": self.expected_ocomp_full_node_mismatch,
            "expected_tee_lease_guard_shutdown_fatal": self.expected_tee_lease_guard_shutdown_fatal,
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

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PayloadIdentity {
    source: PathBuf,
    process_instance: B256,
    payload_id: String,
}

#[derive(Clone, Debug)]
struct PayloadFailure {
    identity: PayloadIdentity,
    tx_hash: B256,
    failure_kind: String,
    location: String,
}

pub(super) fn audit_runtime_logs(
    root: &Path,
    validators: usize,
    unsupported_version: Option<u64>,
    expected_dkg_reveal: Option<&str>,
    expected_ocomp_full_node_mismatch: Option<B256>,
    expected_tee_lease_guard_shutdown_validator: Option<usize>,
    expected_tee_lease_guard_shutdown_full_node: Option<&TeeLeaseShutdownV1>,
) -> Result<LogAudit> {
    if let Some(proof) = expected_tee_lease_guard_shutdown_full_node {
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
    Ok(audit_loaded_logs_with_all_expectations(
        &logs,
        validators,
        unsupported_version,
        expected_dkg_reveal,
        expected_ocomp_full_node_mismatch,
        expected_tee_lease_guard_shutdown_validator,
        expected_tee_lease_guard_shutdown_full_node,
    ))
}

#[cfg(test)]
fn audit_loaded_logs(logs: &[(PathBuf, String)]) -> LogAudit {
    audit_loaded_logs_with_expectations(logs, usize::MAX, None, None, None)
}

#[cfg(test)]
fn audit_loaded_logs_with_expectations(
    logs: &[(PathBuf, String)],
    validators: usize,
    unsupported_version: Option<u64>,
    expected_dkg_reveal: Option<&str>,
    expected_ocomp_full_node_mismatch: Option<B256>,
) -> LogAudit {
    audit_loaded_logs_with_all_expectations(
        logs,
        validators,
        unsupported_version,
        expected_dkg_reveal,
        expected_ocomp_full_node_mismatch,
        None,
        None,
    )
}

fn audit_loaded_logs_with_all_expectations(
    logs: &[(PathBuf, String)],
    validators: usize,
    unsupported_version: Option<u64>,
    expected_dkg_reveal: Option<&str>,
    expected_ocomp_full_node_mismatch: Option<B256>,
    expected_tee_lease_guard_shutdown_validator: Option<usize>,
    expected_tee_lease_guard_shutdown_full_node: Option<&TeeLeaseShutdownV1>,
) -> LogAudit {
    let mut findings = Vec::new();
    let mut counts = LogCounts {
        runtime_log_files: logs.len(),
        ..LogCounts::default()
    };
    let mut cancellations = BTreeSet::new();
    let mut failures = BTreeMap::<PayloadIdentity, BTreeMap<(B256, String), String>>::new();
    let expected_request_deadlines = legacy_request_deadline_cancellations(logs, validators);
    let expected_ocomp_mismatch_records = expected_ocomp_full_node_mismatch
        .map(|job_id| expected_ocomp_full_node_mismatch_records(logs, validators, job_id))
        .transpose();
    let expected_ocomp_mismatch_records = match expected_ocomp_mismatch_records {
        Ok(records) => records.unwrap_or_default(),
        Err(reason) => {
            findings.push(reason);
            BTreeSet::new()
        }
    };
    let expected_tee_lease_shutdown_records = expected_tee_lease_guard_shutdown_validator
        .map(|validator| expected_tee_lease_guard_shutdown_records(logs, validators, validator))
        .transpose();
    let expected_tee_lease_shutdown_records = match expected_tee_lease_shutdown_records {
        Ok(records) => records.unwrap_or_default(),
        Err(reason) => {
            findings.push(reason);
            BTreeSet::new()
        }
    };
    let expected_tee_lease_full_node_shutdown_records = expected_tee_lease_guard_shutdown_full_node
        .map(|full_node| expected_tee_lease_full_node_shutdown_records(logs, validators, full_node))
        .transpose();
    let expected_tee_lease_full_node_shutdown_records =
        match expected_tee_lease_full_node_shutdown_records {
            Ok(records) => records.unwrap_or_default(),
            Err(reason) => {
                findings.push(reason);
                BTreeSet::new()
            }
        };
    let expected_fragment = unsupported_version.map(|version| {
        format!(
            "cannot activate protocol version v{}.{} ({version}): binary supports at most v",
            version >> 24,
            version & 0x00ff_ffff
        )
    });
    let mut expected_by_validator = vec![0_usize; validators.min(1_024)];
    let mut expected_reveal_by_validator = vec![0_usize; validators.min(1_024)];

    for (path, content) in logs {
        let source = path.parent().unwrap_or(path.as_path()).to_path_buf();
        for (index, line) in content.lines().enumerate() {
            let location = format!("{}:{}", path.display(), index + 1);
            let accepted_legacy_deadline = expected_request_deadlines
                .accepted_fatal_records
                .contains(&(path.clone(), index));
            let accepted_update = expected_fragment.as_deref().is_some_and(|fragment| {
                accept_expected_update_fatal(path, line, fragment, &mut expected_by_validator)
            });
            let accepted_reveal = expected_dkg_reveal.is_some_and(|public_key| {
                accept_expected_dkg_reveal(
                    path,
                    line,
                    public_key,
                    &mut expected_reveal_by_validator,
                )
            });
            let accepted_ocomp_mismatch =
                expected_ocomp_mismatch_records.contains(&(path.clone(), index));
            let accepted_tee_lease_shutdown = expected_tee_lease_shutdown_records
                .contains(&(path.clone(), index))
                || expected_tee_lease_full_node_shutdown_records.contains(&(path.clone(), index));
            if accepted_update {
                counts.expected_update_fatal += 1;
            }
            if accepted_reveal {
                counts.expected_dkg_reveal += 1;
            }
            if accepted_ocomp_mismatch {
                counts.expected_ocomp_full_node_mismatch += 1;
            }
            if accepted_tee_lease_shutdown {
                counts.expected_tee_lease_guard_shutdown_fatal += 1;
            }
            if !accepted_legacy_deadline
                && !accepted_update
                && !accepted_reveal
                && !accepted_ocomp_mismatch
                && !accepted_tee_lease_shutdown
            {
                counts.observe(line);
            }
            if !accepted_legacy_deadline
                && !accepted_update
                && !accepted_reveal
                && !accepted_ocomp_mismatch
                && !accepted_tee_lease_shutdown
                && unexpected_log_line(line)
            {
                findings.push(format!("{location}: {line}"));
            }
            let has_schema = structured_field(line, SCHEMA_FIELD).is_some();
            let has_event = structured_field(line, EVENT_FIELD).is_some();
            if !has_schema && !has_event {
                continue;
            }
            if !is_machine_audit_log(path) {
                continue;
            }
            let event = match parse_audit_event(line, source.clone(), &location) {
                Ok(event) => event,
                Err(reason) => {
                    findings.push(format!(
                        "{location}: malformed runtime audit event: {reason}"
                    ));
                    continue;
                }
            };
            match event {
                RuntimeAuditEvent::ProposalViewCancelled(identity) => {
                    cancellations.insert(identity);
                }
                RuntimeAuditEvent::PayloadExecutionFailed(failure) => {
                    failures
                        .entry(failure.identity)
                        .or_default()
                        .entry((failure.tx_hash, failure.failure_kind))
                        .or_insert(failure.location);
                }
            }
        }
    }

    let mut accepted = BTreeSet::new();
    for (identity, outcomes) in failures {
        if outcomes.len() != 1 {
            let variants = outcomes
                .iter()
                .map(|((tx_hash, failure_kind), location)| {
                    format!("{location} tx_hash={tx_hash} failure_kind={failure_kind}")
                })
                .collect::<Vec<_>>()
                .join(", ");
            findings.push(format!(
                "conflicting typed payload failures for payload_id={}: {variants}",
                identity.payload_id
            ));
            continue;
        }
        let ((_, failure_kind), location) = outcomes.into_iter().next().expect("one outcome");
        if failure_kind == BODY_READ_REQUEST_DEADLINE && cancellations.contains(&identity) {
            accepted.insert(identity);
        } else {
            findings.push(format!(
                "{}: unaccepted typed payload failure: {}",
                location, failure_kind
            ));
        }
    }

    if expected_fragment.is_some() {
        for (validator, count) in expected_by_validator.into_iter().enumerate() {
            if count == 0 {
                findings.push(format!(
                    "validator-{validator}/node.log: expected unsupported-version fatal is absent"
                ));
            }
        }
    }
    if expected_dkg_reveal.is_some() {
        for (validator, count) in expected_reveal_by_validator.into_iter().enumerate() {
            if count == 0 {
                findings.push(format!(
                    "validator-{validator}/node.log: expected exact DKG share reveal is absent"
                ));
            }
        }
    }

    LogAudit {
        counts: {
            counts.expected_request_deadline_cancellation =
                accepted.len() + expected_request_deadlines.cancellations;
            counts
        },
        findings,
    }
}

fn collect_logs(dir: &Path, logs: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("scan {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_logs(&path, logs)?;
        } else if is_runtime_log(&path) {
            logs.push(path);
        }
    }
    Ok(())
}

pub(super) fn first_runtime_log_line_containing(
    root: &Path,
    needle: &str,
) -> Result<Option<String>> {
    let mut logs = Vec::new();
    collect_logs(root, &mut logs)?;
    logs.sort();
    for path in logs {
        let content = fs::read_to_string(&path)
            .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
        if let Some((index, line)) = content
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
        {
            return Ok(Some(format!("{}:{}: {}", path.display(), index + 1, line)));
        }
    }
    Ok(None)
}

fn is_runtime_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name == "node.log"
                || name.starts_with("node.log.")
                || name == "enclave.log"
                || is_reth_runtime_log_name(name)
        })
}

fn is_reth_runtime_log_name(name: &str) -> bool {
    name == "reth.log" || name.starts_with("reth.log.")
}

fn is_machine_audit_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "node.log" || name.starts_with("node.log."))
}

fn unexpected_log_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    exact_tee_lease_jailed_guard_shutdown(line)
        || exact_tee_lease_expired_full_node_guard_shutdown(line)
        || fatal_at_runtime_severity(line)
        || lower.contains("panic")
        || (lower.contains("dkg")
            && lower.contains("share")
            && (lower.contains("reveal") || lower.contains("disclos")))
        || (lower.contains("vrf") && lower.contains("alarm"))
        || lower.contains("eagain")
        || lower.contains("resource temporarily unavailable")
        || (lower.contains("projection") && lower.contains("fatal"))
}

fn fatal_at_runtime_severity(line: &str) -> bool {
    line.to_ascii_lowercase().contains("fatal")
        && !matches!(runtime_log_level(line), Some("TRACE" | "DEBUG"))
}

fn runtime_log_level(line: &str) -> Option<&str> {
    let mut fields = line.split_ascii_whitespace();
    let first = fields.next()?;
    if is_runtime_log_level(first) {
        return Some(first);
    }
    let second = fields.next()?;
    (looks_like_runtime_timestamp(first) && is_runtime_log_level(second)).then_some(second)
}

fn is_runtime_log_level(value: &str) -> bool {
    matches!(value, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR")
}

fn looks_like_runtime_timestamp(value: &str) -> bool {
    value.contains('T') && value.contains(':')
}

enum RuntimeAuditEvent {
    ProposalViewCancelled(PayloadIdentity),
    PayloadExecutionFailed(PayloadFailure),
}

fn parse_audit_event(
    line: &str,
    source: PathBuf,
    location: &str,
) -> Result<RuntimeAuditEvent, &'static str> {
    let schema = structured_field(line, SCHEMA_FIELD).ok_or("missing audit_schema")?;
    if schema.parse::<u8>().ok() != Some(SCHEMA_VERSION) {
        return Err("unsupported audit_schema");
    }
    let event = structured_field(line, EVENT_FIELD).ok_or("missing audit_event")?;
    let process_instance = structured_field(line, PROCESS_INSTANCE_FIELD)
        .ok_or("missing or invalid process_instance")?
        .parse::<B256>()
        .map_err(|_| "missing or invalid process_instance")?;
    let payload_id = canonical_payload_id(
        structured_field(line, "payload_id").ok_or("missing or invalid payload_id")?,
    )
    .ok_or("missing or invalid payload_id")?;
    let identity = PayloadIdentity {
        source,
        process_instance,
        payload_id: payload_id.to_owned(),
    };
    match event {
        PROPOSAL_VIEW_CANCELLED => Ok(RuntimeAuditEvent::ProposalViewCancelled(identity)),
        PAYLOAD_EXECUTION_FAILED => {
            let tx_hash = structured_field(line, "tx_hash")
                .ok_or("missing or invalid tx_hash")?
                .parse::<B256>()
                .map_err(|_| "missing or invalid tx_hash")?;
            let failure_kind = structured_field(line, FAILURE_KIND_FIELD)
                .ok_or("missing failure_kind")?
                .to_owned();
            Ok(RuntimeAuditEvent::PayloadExecutionFailed(PayloadFailure {
                identity,
                tx_hash,
                failure_kind,
                location: location.to_owned(),
            }))
        }
        _ => Err("unknown audit_event"),
    }
}

fn structured_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_ascii_whitespace()
        .filter_map(|token| token.split_once('='))
        .find_map(|(field, value)| (field == name).then_some(value))
}

fn canonical_payload_id(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("0x")?;
    (hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequestDeadlineIdentity {
    validator: usize,
    block_number: u64,
    block_hash: B256,
}

#[derive(Debug, Default)]
struct LegacyRequestDeadlineCancellations {
    cancellations: usize,
    accepted_fatal_records: BTreeSet<(PathBuf, usize)>,
}

/// Compatibility adapter for the pre-existing `newPayload` cancellation
/// records. New exceptions must use versioned runtime audit events instead.
fn legacy_request_deadline_cancellations(
    logs: &[(PathBuf, String)],
    validators: usize,
) -> LegacyRequestDeadlineCancellations {
    let runtime_nodes = validators.saturating_add(1);
    let mut typed = BTreeSet::new();
    let mut terminal = BTreeSet::new();

    for (path, content) in logs {
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_reth_runtime_log_name)
        {
            continue;
        }
        let Some(validator) = validator_log_index(path, runtime_nodes) else {
            continue;
        };
        for line in content.lines() {
            if line.contains("error=BodyReadRequestDeadline") {
                if let Some(identity) = scoped_new_payload_identity(line, validator) {
                    typed.insert(identity);
                }
            }
            if exact_request_deadline_receiver_drop(line) {
                if let Some(identity) = terminal_payload_identity(line, validator) {
                    terminal.insert(identity);
                }
            }
        }
    }

    let complete = typed
        .intersection(&terminal)
        .copied()
        .collect::<BTreeSet<_>>();
    let mut accepted = BTreeSet::new();
    for (path, content) in logs {
        let Some(validator) = validator_log_index(path, runtime_nodes) else {
            continue;
        };
        let lines = content.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().copied().enumerate() {
            if !exact_request_deadline_fatal(line) {
                continue;
            }
            if let Some(identity) = scoped_new_payload_identity(line, validator)
                .or_else(|| terminal_payload_identity(line, validator))
            {
                if complete.contains(&identity) {
                    accepted.insert((path.clone(), index));
                }
                continue;
            }

            if path.file_name().and_then(|name| name.to_str()) == Some("node.log")
                && line.contains("outbe::executor")
                && lines
                    .iter()
                    .take(index.saturating_add(5))
                    .skip(index + 1)
                    .any(|candidate| {
                        exact_request_deadline_receiver_drop(candidate)
                            && terminal_payload_identity(candidate, validator)
                                .is_some_and(|identity| complete.contains(&identity))
                    })
            {
                accepted.insert((path.clone(), index));
            }
        }
    }

    LegacyRequestDeadlineCancellations {
        cancellations: complete.len(),
        accepted_fatal_records: accepted,
    }
}

fn validator_log_index(path: &Path, validators: usize) -> Option<usize> {
    path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        let index = name.strip_prefix("validator-")?.parse().ok()?;
        (index < validators).then_some(index)
    })
}

fn validator_node_log_index(path: &Path, validators: usize) -> Option<usize> {
    if path.file_name()?.to_str()? != "node.log" {
        return None;
    }
    let validator = path.parent()?.file_name()?.to_str()?;
    let index = validator.strip_prefix("validator-")?.parse().ok()?;
    (index < validators).then_some(index)
}

fn scoped_new_payload_identity(line: &str, validator: usize) -> Option<RequestDeadlineIdentity> {
    Some(RequestDeadlineIdentity {
        validator,
        block_number: value_after(line, "block_num=")?.parse().ok()?,
        block_hash: value_after(line, "block_hash=")?.parse().ok()?,
    })
}

fn terminal_payload_identity(line: &str, validator: usize) -> Option<RequestDeadlineIdentity> {
    let payload = line.rsplit_once("NumHash {")?.1;
    Some(RequestDeadlineIdentity {
        validator,
        block_number: value_after(payload, "number: ")?.parse().ok()?,
        block_hash: value_after(payload, "hash: ")?.parse().ok()?,
    })
}

fn value_after<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    line.split_once(marker)?
        .1
        .split([',', ' ', '}', ':'])
        .next()
}

fn exact_request_deadline_receiver_drop(line: &str) -> bool {
    line.contains("Failed to deliver newPayload response, receiver dropped (request cancelled)")
        && line.contains("Err(Internal(BlockExecutionError(Other(")
        && line.contains("fatal: body read request deadline exceeded")
        && !line.contains("body read unavailable")
        && !line.contains("BodyReadCorruption")
        && !line.contains("StorageError::Backend")
}

fn exact_request_deadline_fatal(line: &str) -> bool {
    line.matches("fatal: body read request deadline exceeded")
        .count()
        == 1
        && !contains_nonfatal_alarm(&line.to_ascii_lowercase())
        && !line.contains("body read unavailable")
        && !line.contains("BodyReadCorruption")
        && !line.contains("StorageError::Backend")
}

fn accept_expected_update_fatal(
    path: &Path,
    line: &str,
    expected_fragment: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_update_fatal(line, expected_fragment) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn accept_expected_dkg_reveal(
    path: &Path,
    line: &str,
    expected_public_key: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_dkg_reveal(line, expected_public_key) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn expected_ocomp_full_node_mismatch_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    job_id: B256,
) -> Result<BTreeSet<(PathBuf, usize)>, String> {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Record {
        Evidence,
        Engine,
        Cli,
    }

    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    for (path, content) in logs {
        if validator_log_index(path, validators.saturating_add(1)) != Some(validators) {
            continue;
        }
        let Some(sink) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !matches!(sink, "node.log" | "reth.log") {
            continue;
        }

        let records = content
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                if exact_ocomp_mismatch_evidence(line, job_id) {
                    Some((index, Record::Evidence))
                } else if exact_engine_fatal_trailer(line) {
                    Some((index, Record::Engine))
                } else if exact_cli_fatal_trailer(line) {
                    Some((index, Record::Cli))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            continue;
        }
        let kinds = records
            .iter()
            .map(|(_, record)| *record)
            .collect::<Vec<_>>();
        if kinds != [Record::Evidence, Record::Engine, Record::Cli] {
            return Err(format!(
                "malformed expected OCOMP FullNode mismatch shutdown bundle in {}: {kinds:?}",
                path.display()
            ));
        }
        if !sinks.insert(sink) {
            return Err(format!(
                "duplicate expected OCOMP FullNode mismatch shutdown sink {sink}"
            ));
        }
        accepted.extend(records.into_iter().map(|(index, _)| (path.clone(), index)));
    }

    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err(format!(
            "expected one node.log and one reth.log OCOMP FullNode mismatch shutdown bundle for job {job_id:#x}, observed {sinks:?}"
        ));
    }
    Ok(accepted)
}

fn expected_tee_lease_guard_shutdown_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    expected_validator: usize,
) -> Result<BTreeSet<(PathBuf, usize)>, String> {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Record {
        Guard,
        StackShutdown,
    }

    if expected_validator >= validators {
        return Err(format!(
            "expected TEE lease guard shutdown validator-{expected_validator} is outside the {validators}-validator committee"
        ));
    }

    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    for (path, content) in logs {
        if validator_log_index(path, validators) != Some(expected_validator) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let sink = if file_name == "node.log" {
            "node.log"
        } else if file_name == "reth.log" {
            "reth.log"
        } else {
            continue;
        };

        let lines = content.lines().collect::<Vec<_>>();
        let guard_indices = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                exact_tee_lease_jailed_guard_shutdown(line).then_some(index)
            })
            .collect::<Vec<_>>();
        if guard_indices.is_empty() {
            continue;
        }
        if guard_indices.len() != 1 {
            return Err(format!(
                "expected exactly one TEE lease guard in {}, observed {}",
                path.display(),
                guard_indices.len()
            ));
        }

        let guard_index = guard_indices[0];
        let mut records = vec![(guard_index, Record::Guard)];
        let mut cursor = guard_index.saturating_add(1);
        for expected in [Record::StackShutdown] {
            loop {
                let Some(line) = lines.get(cursor).copied() else {
                    return Err(format!(
                        "incomplete expected TEE lease guard shutdown bundle in {}: missing {expected:?}",
                        path.display()
                    ));
                };
                if cursor.saturating_sub(guard_index) > 64 || process_start_boundary(line) {
                    return Err(format!(
                        "expected TEE lease guard shutdown crossed its line or process-epoch bound in {} before {expected:?}",
                        path.display()
                    ));
                }
                let observed = if exact_tee_lease_jailed_guard_shutdown(line) {
                    Some(Record::Guard)
                } else if exact_consensus_stack_shutdown(line) {
                    Some(Record::StackShutdown)
                } else {
                    None
                };
                cursor = cursor.saturating_add(1);
                let Some(observed) = observed else {
                    continue;
                };
                if observed != expected {
                    return Err(format!(
                        "malformed expected TEE lease guard shutdown bundle in {}: expected {expected:?}, observed {observed:?}",
                        path.display()
                    ));
                }
                records.push((cursor - 1, observed));
                break;
            }
        }
        if !sinks.insert(sink) {
            return Err(format!(
                "duplicate expected TEE lease guard shutdown sink {sink} for validator-{expected_validator}"
            ));
        }
        accepted.extend(records.into_iter().map(|(index, _)| (path.clone(), index)));
    }

    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err(format!(
            "expected one node.log and one reth.log TEE lease guard shutdown bundle for validator-{expected_validator}, observed {sinks:?}"
        ));
    }
    Ok(accepted)
}

fn expected_tee_lease_full_node_shutdown_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    proof: &crate::world::state::TeeLeaseShutdownV1,
) -> Result<BTreeSet<(PathBuf, usize)>, String> {
    if proof.slot != validators
        || proof.pid == 0
        || proof.deadline == 0
        || proof.exit_code != Some(0)
        || proof.exit_signal.is_some()
    {
        return Err(
            "TEE lease FullNode proof lacks the exact owned successful exit/slot/deadline"
                .to_owned(),
        );
    }
    if proof.sinks.len() != 2 {
        return Err(
            "TEE lease FullNode proof requires both immutable process sink intervals".to_owned(),
        );
    }
    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    for interval in &proof.sinks {
        if validator_log_index(&interval.path, validators + 1) != Some(proof.slot) {
            return Err("TEE lease FullNode proof sink belongs to another slot".to_owned());
        }
        let sink = interval
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !["node.log", "reth.log"].contains(&sink) || !sinks.insert(sink) {
            return Err("TEE lease FullNode proof has duplicate or unexpected sinks".to_owned());
        }
        let Some((_, full_content)) = logs.iter().find(|(path, _)| path == &interval.path) else {
            return Err(format!(
                "missing retained lease sink {}",
                interval.path.display()
            ));
        };
        let start = usize::try_from(interval.start).map_err(|_| "invalid interval start")?;
        let end = usize::try_from(interval.end).map_err(|_| "invalid interval end")?;
        if start > end
            || full_content.get(start..end) != Some(interval.content.as_str())
            || (start > 0 && full_content.as_bytes().get(start - 1) != Some(&b'\n'))
        {
            return Err(format!(
                "changed or truncated lease process interval in {}",
                interval.path.display()
            ));
        }
        let offset = full_content[..start].lines().count();
        let lines: Vec<_> = interval.content.lines().collect();
        let guards: Vec<_> = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                exact_tee_lease_expired_full_node_guard_shutdown(line).then_some(index)
            })
            .collect();
        let stacks: Vec<_> = lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| exact_consensus_stack_shutdown(line).then_some(index))
            .collect();
        if guards.len() != 1 || stacks.len() != 1 {
            return Err(format!(
                "expected exactly one FullNode guard/stack pair in {}",
                interval.path.display()
            ));
        }
        let guard = guards[0];
        let stack = stacks[0];
        let expected_reason = format!(
            "reason=finalized TEE lease expired at {}; stop node and run tee join",
            proof.deadline
        );
        if !lines[guard].trim_end().ends_with(&expected_reason)
            || stack <= guard
            || stack - guard > 64
        {
            return Err("FullNode lease guard deadline or shutdown ordering differs".to_owned());
        }
        // Only the guard and stack records are exempted. Generic fatal/panic
        // records, including EngineFatal/CliFatal, remain ordinary audit failures.
        accepted.insert((interval.path.clone(), offset + guard));
        accepted.insert((interval.path.clone(), offset + stack));
        let mut last = stack;
        if sink == "reth.log" {
            for marker in [
                "reth::cli: received engine shutdown request",
                "engine::tree: received terminate request",
                "engine::tree: persistence complete, signaling termination",
                "reth::cli: shutting down gracefully",
            ] {
                let matches: Vec<_> = lines
                    .iter()
                    .enumerate()
                    .filter_map(|(index, line)| {
                        let (prefix, _) = line.split_once(marker)?;
                        (prefix.split_whitespace().last() == Some("DEBUG")
                            && line.trim_end().ends_with(marker))
                        .then_some(index)
                    })
                    .collect();
                if matches.len() != 1 || matches[0] <= last || matches[0] - guard > 64 {
                    return Err(format!(
                        "missing, duplicate or unordered FullNode termination record: {marker}"
                    ));
                }
                last = matches[0];
            }
        }
        if lines[guard + 1..]
            .iter()
            .any(|line| process_start_boundary(line))
        {
            return Err(
                "FullNode lease shutdown evidence crossed a process replacement".to_owned(),
            );
        }
        if lines[..guard]
            .iter()
            .filter(|line| line.contains("reth::cli: Starting Reth version="))
            .count()
            > 1
        {
            return Err("FullNode lease interval contains multiple process launches".to_owned());
        }
    }
    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err("FullNode lease shutdown omitted an expected sink".to_owned());
    }
    Ok(accepted)
}

fn exact_tee_lease_jailed_guard_shutdown(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.trim_end().ends_with(
        "outbe_chain: finalized tee lease guard requested node shutdown reason=validator is jailed; complete ordinary unjail and then run tee join",
    ) && line.contains("error")
        && !contains_nonfatal_alarm(&line)
}

fn exact_tee_lease_expired_full_node_guard_shutdown(line: &str) -> bool {
    const PREFIX: &str =
        "outbe_chain: finalized tee lease guard requested node shutdown reason=finalized tee lease expired at ";
    const SUFFIX: &str = "; stop node and run tee join";

    let line = line.to_ascii_lowercase();
    let Some((prefix, deadline_and_suffix)) = line.split_once(PREFIX) else {
        return false;
    };
    let Some(deadline) = deadline_and_suffix.trim_end().strip_suffix(SUFFIX) else {
        return false;
    };
    prefix.split_whitespace().last() == Some("error")
        && !deadline.is_empty()
        && deadline.bytes().all(|byte| byte.is_ascii_digit())
        && !contains_nonfatal_alarm(&line)
}

fn exact_consensus_stack_shutdown(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.trim_end()
        .ends_with("outbe_chain: consensus stack shutting down")
        && line.contains("info")
        && !contains_nonfatal_alarm(&line)
}

fn process_start_boundary(line: &str) -> bool {
    line.contains("reth::cli: Initialized tracing, debug log directory:")
        || line.contains("reth::cli: Starting Reth version=")
}

fn exact_ocomp_mismatch_evidence(line: &str, job_id: B256) -> bool {
    let line = line.to_ascii_lowercase();
    fatal_at_runtime_severity(&line)
        && line.trim_end().ends_with(&format!(
            "embedded ocomp persisted fatal evidence: job_id={job_id:#x}"
        ))
        && !contains_nonfatal_alarm(&line)
}

fn exact_engine_fatal_trailer(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    fatal_at_runtime_severity(&line)
        && line.trim_end().ends_with("engine::tree: fatal error")
        && !contains_nonfatal_alarm(&line)
}

fn exact_cli_fatal_trailer(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    fatal_at_runtime_severity(&line)
        && line
            .trim_end()
            .ends_with("reth::cli: fatal error in consensus engine")
        && !contains_nonfatal_alarm(&line)
}

fn exact_expected_dkg_reveal(line: &str, expected_public_key: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("dkg: a validator's individual share was revealed")
        .count()
        == 1
        && line.contains("revealed_validator")
        && line.contains(&expected_public_key.to_ascii_lowercase())
        && !line.contains("fatal")
        && !line.contains("panic")
        && !(line.contains("vrf") && line.contains("alarm"))
        && !line.contains("eagain")
        && !line.contains("resource temporarily unavailable")
        && !(line.contains("projection") && line.contains("fatal"))
}

fn exact_expected_update_fatal(line: &str, expected_fragment: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("fatal").count() == 1
        && line.matches("cannot activate protocol version").count() == 1
        && line.contains(expected_fragment)
        && !contains_nonfatal_alarm(&line)
}

fn contains_nonfatal_alarm(line: &str) -> bool {
    line.contains("panic")
        || (line.contains("dkg")
            && line.contains("share")
            && (line.contains("reveal") || line.contains("disclos")))
        || (line.contains("vrf") && line.contains("alarm"))
        || line.contains("eagain")
        || line.contains("resource temporarily unavailable")
        || (line.contains("projection") && line.contains("fatal"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        audit_loaded_logs, audit_loaded_logs_with_all_expectations,
        audit_loaded_logs_with_expectations,
    };
    use alloy_primitives::B256;
    use outbe_primitives::runtime_audit_v1::{
        BODY_READ_REQUEST_DEADLINE, OTHER_PAYLOAD_EXECUTION_FAILURE, PAYLOAD_EXECUTION_FAILED,
        PROCESS_INSTANCE_FIELD, PROPOSAL_VIEW_CANCELLED, SCHEMA_VERSION,
    };

    const PROCESS_A: &str = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PROCESS_B: &str = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn event(fields: &str, human_message: &str) -> String {
        event_for(PROCESS_A, fields, human_message)
    }

    fn event_for(process_instance: &str, fields: &str, human_message: &str) -> String {
        format!(
            "2026-08-13T00:00:00Z DEBUG \
             {PROCESS_INSTANCE_FIELD}={process_instance} {fields} {human_message}"
        )
    }

    #[test]
    fn cancelled_deadline_uses_typed_fields_across_reordered_logs() {
        let root = tempfile::TempDir::new().unwrap();
        let first = root.path().join("validator-0/node.log.1");
        let current = root.path().join("validator-0/node.log");
        let payload_id = "0x850344ecbf402d32";
        let tx_hash = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
        let logs = vec![
            (
                first,
                event(
                    &format!(
                        "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                         payload_id={payload_id} tx_hash={tx_hash} \
                         failure_kind={BODY_READ_REQUEST_DEADLINE}"
                    ),
                    "this human message may be rewritten",
                ),
            ),
            (
                current,
                event(
                    &format!(
                        "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                         payload_id={payload_id}"
                    ),
                    "different wording and later file order",
                ),
            ),
        ];

        let audit = audit_loaded_logs(&logs);
        assert!(audit.is_clean(), "{:?}", audit.findings);
        assert_eq!(audit.counts.expected_request_deadline_cancellation, 1);
        assert!(audit.ensure_clean().is_ok());
        assert_eq!(audit.json()["clean"], true);
    }

    #[test]
    fn deadline_for_a_live_payload_fails_closed() {
        let path = tempfile::TempDir::new()
            .unwrap()
            .path()
            .join("validator-0/node.log");
        let logs = vec![(
            path,
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id=0x850344ecbf402d32 \
                     tx_hash=0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                     failure_kind={BODY_READ_REQUEST_DEADLINE}"
                ),
                "message is irrelevant",
            ),
        )];

        let audit = audit_loaded_logs(&logs);
        assert!(!audit.is_clean());
        assert!(audit.ensure_clean().is_err());
        assert_eq!(audit.json()["clean"], false);
    }

    #[test]
    fn cancellation_from_a_prior_process_cannot_authorize_a_current_failure() {
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("validator-0");
        let payload_id = "0x850344ecbf402d32";
        let tx_hash = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
        let logs = vec![
            (
                directory.join("node.log.1"),
                event_for(
                    PROCESS_A,
                    &format!(
                        "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                         payload_id={payload_id}"
                    ),
                    "previous process",
                ),
            ),
            (
                directory.join("node.log"),
                event_for(
                    PROCESS_B,
                    &format!(
                        "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                         payload_id={payload_id} tx_hash={tx_hash} \
                         failure_kind={BODY_READ_REQUEST_DEADLINE}"
                    ),
                    "current process",
                ),
            ),
        ];

        let audit = audit_loaded_logs(&logs);
        assert!(!audit.is_clean());
        assert_eq!(audit.counts.expected_request_deadline_cancellation, 0);
    }

    #[test]
    fn hygiene_uses_record_severity_instead_of_nested_error_words() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("validator-0/node.log");
        let debug = vec![(
            path.clone(),
            "2026-08-13T00:00:00Z DEBUG retry discarded err=fatal: stale speculative work"
                .to_owned(),
        )];
        assert!(audit_loaded_logs(&debug).is_clean());

        let error = vec![(
            path,
            "2026-08-13T00:00:00Z ERROR consensus supervisor fatal exit DEBUG context".to_owned(),
        )];
        assert!(!audit_loaded_logs(&error).is_clean());
    }

    #[test]
    fn duplicate_typed_records_are_one_cancelled_episode() {
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("validator-0");
        let payload = "0x850344ecbf402d32";
        let transaction = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
        let cancellation = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                 payload_id={payload}"
            ),
            "human cancellation text",
        );
        let failure = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id={payload} tx_hash={transaction} \
                 failure_kind={BODY_READ_REQUEST_DEADLINE}"
            ),
            "human failure text",
        );
        let logs = vec![
            (directory.join("node.log.1"), cancellation.clone()),
            (
                directory.join("node.log"),
                format!("{cancellation}\n{failure}\n{failure}"),
            ),
        ];

        let audit = audit_loaded_logs(&logs);
        assert!(audit.is_clean(), "{:?}", audit.findings);
        assert_eq!(audit.counts.expected_request_deadline_cancellation, 1);
    }

    #[test]
    fn conflicting_duplicate_failure_kinds_fail_closed_in_any_order() {
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("validator-0/node.log");
        let payload = "0x850344ecbf402d32";
        let transaction = "0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce";
        let cancellation = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                 payload_id={payload}"
            ),
            "cancelled",
        );
        let deadline = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id={payload} tx_hash={transaction} \
                 failure_kind={BODY_READ_REQUEST_DEADLINE}"
            ),
            "deadline",
        );
        let other = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                 payload_id={payload} tx_hash={transaction} \
                 failure_kind={OTHER_PAYLOAD_EXECUTION_FAILURE}"
            ),
            "other",
        );

        for records in [
            format!("{cancellation}\n{deadline}\n{other}"),
            format!("{cancellation}\n{other}\n{deadline}"),
        ] {
            let audit = audit_loaded_logs(&[(path.clone(), records)]);
            assert!(!audit.is_clean());
            assert_eq!(audit.counts.expected_request_deadline_cancellation, 0);
        }
    }

    #[test]
    fn malformed_unknown_mismatched_and_other_failures_fail_closed() {
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("validator-0");
        let cancellation = event(
            &format!(
                "audit_schema={SCHEMA_VERSION} audit_event={PROPOSAL_VIEW_CANCELLED} \
                 payload_id=0x850344ecbf402d32"
            ),
            "cancelled",
        );
        let records = [
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id=0x950344ecbf402d32 \
                     tx_hash=0xde0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                     failure_kind={BODY_READ_REQUEST_DEADLINE}"
                ),
                "different payload",
            ),
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id=0x850344ecbf402d32 \
                     tx_hash=0xce0edec00c2c9e76f5a142a7a06cf07079ebf0bcc191d570ab87b8a83150d3ce \
                     failure_kind={OTHER_PAYLOAD_EXECUTION_FAILURE}"
                ),
                "wrong failure kind",
            ),
            event(
                &format!(
                    "audit_schema={SCHEMA_VERSION} audit_event={PAYLOAD_EXECUTION_FAILED} \
                     payload_id=0x850344ecbf402d32 \
                     failure_kind={BODY_READ_REQUEST_DEADLINE}"
                ),
                "missing transaction hash",
            ),
            event(
                "audit_schema=2 audit_event=proposal_view_cancelled_v2 \
                 payload_id=0x850344ecbf402d32",
                "unknown schema and event",
            ),
        ];
        let logs = vec![(
            directory.join("node.log"),
            std::iter::once(cancellation)
                .chain(records)
                .collect::<Vec<_>>()
                .join("\n"),
        )];

        let audit = audit_loaded_logs(&logs);
        assert!(!audit.is_clean());
        assert!(audit.findings.len() >= 4, "{:?}", audit.findings);
    }

    fn legacy_deadline_bundle(hash: &str, terminal_hash: &str) -> Vec<(PathBuf, String)> {
        let reth = format!(
            "2026-08-09T10:48:40Z ERROR on_new_payload{{block_hash={hash} block_num=323}}: trigger failed error=BodyReadRequestDeadline\n\
             2026-08-09T10:48:40Z ERROR on_new_payload{{block_hash={hash} block_num=323}}: fatal: body read request deadline exceeded\n\
             2026-08-09T10:48:40Z DEBUG on_new_payload{{block_hash={hash} block_num=323}}: fatal: body read request deadline exceeded block=NumHash {{ number: 323, hash: {hash} }}\n\
             2026-08-09T10:48:40Z WARN Failed to deliver newPayload response, receiver dropped (request cancelled): Err(Internal(BlockExecutionError(Other(\"fatal: body read request deadline exceeded\")))) payload=NumHash {{ number: 323, hash: {terminal_hash} }}"
        );
        let node = format!(
            "2026-08-09T10:48:40Z ERROR outbe::executor: fatal: body read request deadline exceeded\n\
             2026-08-09T10:48:40Z WARN Failed to deliver newPayload response, receiver dropped (request cancelled): Err(Internal(BlockExecutionError(Other(\"fatal: body read request deadline exceeded\")))) payload=NumHash {{ number: 323, hash: {terminal_hash} }}"
        );
        vec![
            (
                PathBuf::from("scenario-1/validator-1/logs/54322345/reth.log"),
                reth,
            ),
            (PathBuf::from("scenario-1/validator-1/node.log"), node),
        ]
    }

    #[test]
    fn legacy_new_payload_adapter_preserves_the_existing_exact_rule() {
        let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
        let accepted = audit_loaded_logs_with_expectations(
            &legacy_deadline_bundle(hash, hash),
            4,
            None,
            None,
            None,
        );
        assert!(accepted.is_clean(), "{:?}", accepted.findings);
        assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);

        let other = "0x46aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
        assert!(!audit_loaded_logs_with_expectations(
            &legacy_deadline_bundle(hash, other),
            4,
            None,
            None,
            None,
        )
        .is_clean());
    }

    #[test]
    fn legacy_new_payload_adapter_accepts_rotated_reth_log() {
        let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
        let mut logs = legacy_deadline_bundle(hash, hash);
        logs[0].0 = PathBuf::from("scenario-1/validator-1/logs/54322345/reth.log.1");

        let accepted = audit_loaded_logs_with_expectations(&logs, 4, None, None, None);
        assert!(accepted.is_clean(), "{:?}", accepted.findings);
        assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);
    }

    #[test]
    fn legacy_new_payload_adapter_accepts_only_the_canonical_joiner() {
        let hash = "0x36aded35254849d7f72af50566e7ef7b799b8d970741319889c63b3ae292ce3a";
        let joiner_logs = legacy_deadline_bundle(hash, hash)
            .into_iter()
            .map(|(path, content)| {
                (
                    PathBuf::from(path.to_string_lossy().replace("validator-1", "validator-4")),
                    content,
                )
            })
            .collect::<Vec<_>>();

        let accepted = audit_loaded_logs_with_expectations(&joiner_logs, 4, None, None, None);
        assert!(accepted.is_clean(), "{:?}", accepted.findings);
        assert_eq!(accepted.counts.expected_request_deadline_cancellation, 1);

        let outside_topology = joiner_logs
            .into_iter()
            .map(|(path, content)| {
                (
                    PathBuf::from(path.to_string_lossy().replace("validator-4", "validator-5")),
                    content,
                )
            })
            .collect::<Vec<_>>();
        let rejected = audit_loaded_logs_with_expectations(&outside_topology, 4, None, None, None);
        assert!(!rejected.is_clean());
        assert_eq!(rejected.counts.expected_request_deadline_cancellation, 0);
    }

    #[test]
    fn legacy_update_and_dkg_expectations_remain_exact_adapters() {
        let update = vec![(
            PathBuf::from("scenario-1/validator-0/node.log"),
            "ERROR fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v0.1 (1)"
                .to_owned(),
        )];
        let update_audit =
            audit_loaded_logs_with_expectations(&update, 1, Some(50_331_648), None, None);
        assert!(update_audit.is_clean(), "{:?}", update_audit.findings);
        assert_eq!(update_audit.counts.expected_update_fatal, 1);

        let key = "91782b96da4ceae23d5adfa62ec55ef41827d43c8b624035972bc0a086f743266168a73e42e5b6c14c119dcf94d39588";
        let reveal = vec![(
            PathBuf::from("scenario-1/validator-0/node.log"),
            format!(
                "WARN DKG: a validator's individual share was REVEALED \
                 revealed_validator={key}"
            ),
        )];
        let reveal_audit = audit_loaded_logs_with_expectations(&reveal, 1, None, Some(key), None);
        assert!(reveal_audit.is_clean(), "{:?}", reveal_audit.findings);
        assert_eq!(reveal_audit.counts.expected_dkg_reveal, 1);
    }

    #[test]
    fn full_node_mismatch_requires_the_exact_two_sink_shutdown_bundle() {
        let job_id = B256::repeat_byte(0x42);
        let anchor =
            format!("ERROR failure=embedded OCOMP persisted fatal evidence: job_id={job_id:#x}");
        let engine = "ERROR poll_next_event: engine::tree: Fatal error";
        let cli = "ERROR reth::cli: Fatal error in consensus engine";
        let expected = vec![
            (
                PathBuf::from("scenario-1/validator-4/node.log"),
                format!("{anchor}\nERROR engine::tree: Fatal error\n{cli}"),
            ),
            (
                PathBuf::from("scenario-1/validator-4/logs/54322345/reth.log"),
                format!("{anchor}\n{engine}\n{cli}"),
            ),
        ];
        let audit = audit_loaded_logs_with_expectations(&expected, 4, None, None, Some(job_id));
        assert!(audit.is_clean(), "{:?}", audit.findings);
        assert_eq!(audit.counts.expected_ocomp_full_node_mismatch, 6);

        let wrong_job = audit_loaded_logs_with_expectations(
            &expected,
            4,
            None,
            None,
            Some(B256::repeat_byte(0x43)),
        );
        assert!(!wrong_job.is_clean());
        let wrong_node = expected
            .iter()
            .map(|(path, content)| {
                (
                    PathBuf::from(path.to_string_lossy().replace("validator-4", "validator-3")),
                    content.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            !audit_loaded_logs_with_expectations(&wrong_node, 4, None, None, Some(job_id),)
                .is_clean()
        );

        let missing_sink = vec![expected[0].clone()];
        assert!(
            !audit_loaded_logs_with_expectations(&missing_sink, 4, None, None, Some(job_id))
                .is_clean()
        );

        let mut unrelated_fatal = expected.clone();
        unrelated_fatal[0]
            .1
            .push_str("\nERROR outbe_chain: unrelated fatal condition");
        assert!(!audit_loaded_logs_with_expectations(
            &unrelated_fatal,
            4,
            None,
            None,
            Some(job_id),
        )
        .is_clean());

        let reordered = expected
            .iter()
            .map(|(path, _)| (path.clone(), format!("{engine}\n{anchor}\n{cli}")))
            .collect::<Vec<_>>();
        assert!(
            !audit_loaded_logs_with_expectations(&reordered, 4, None, None, Some(job_id))
                .is_clean()
        );

        let mut duplicated = expected.clone();
        duplicated[1].1.push_str(&format!("\n{anchor}"));
        assert!(
            !audit_loaded_logs_with_expectations(&duplicated, 4, None, None, Some(job_id))
                .is_clean()
        );

        let mut panic = expected.clone();
        panic[0].1.push_str("\nERROR panic in unrelated teardown");
        assert!(
            !audit_loaded_logs_with_expectations(&panic, 4, None, None, Some(job_id)).is_clean()
        );

        let absent = audit_loaded_logs_with_expectations(&[], 4, None, None, Some(job_id));
        assert!(!absent.is_clean());
    }

    fn tee_lease_guard_shutdown_bundle(validator: usize) -> Vec<(PathBuf, String)> {
        let guard = "ERROR outbe_chain: finalized TEE lease guard requested node shutdown \
            reason=validator is jailed; complete ordinary unjail and then run tee join";
        let shutdown = "INFO outbe_chain: consensus stack shutting down";
        vec![
            (
                PathBuf::from(format!("scenario-1/validator-{validator}/node.log")),
                format!("{guard}\n{shutdown}"),
            ),
            (
                PathBuf::from(format!(
                    "scenario-1/validator-{validator}/logs/54322345/reth.log"
                )),
                format!("{guard}\n{shutdown}"),
            ),
        ]
    }

    fn tee_lease_full_node_shutdown_bundle(full_node: usize) -> Vec<(PathBuf, String)> {
        let guard = "ERROR outbe_chain: finalized TEE lease guard requested node shutdown reason=finalized TEE lease expired at 1789793371; stop node and run tee join";
        let shutdown = "INFO outbe_chain: consensus stack shutting down";
        vec![
            (PathBuf::from(format!("scenario-1/validator-{full_node}/node.log")), format!("{guard}\n{shutdown}\n")),
            (PathBuf::from(format!("scenario-1/validator-{full_node}/logs/54322345/reth.log")),
             format!("{guard}\n{shutdown}\nDEBUG reth::cli: received engine shutdown request\nDEBUG engine::tree: received terminate request\nDEBUG engine::tree: persistence complete, signaling termination\nDEBUG reth::cli: shutting down gracefully\n")),
        ]
    }

    fn full_node_proof(logs: &[(PathBuf, String)]) -> crate::world::state::TeeLeaseShutdownV1 {
        crate::world::state::TeeLeaseShutdownV1 {
            slot: 4,
            pid: 1234,
            deadline: 1789793371,
            exit_code: Some(0),
            exit_signal: None,
            sinks: logs
                .iter()
                .map(
                    |(path, content)| crate::world::state::TeeLeaseLogIntervalV1 {
                        path: path.clone(),
                        device: 1,
                        inode: 2,
                        start: 0,
                        end: content.len() as u64,
                        content: content.clone(),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn tee_lease_guard_accepts_only_the_explicit_exact_two_sink_shutdown_bundle() {
        let expected = tee_lease_guard_shutdown_bundle(3);
        let accepted =
            audit_loaded_logs_with_all_expectations(&expected, 4, None, None, None, Some(3), None);
        assert!(accepted.is_clean(), "{:?}", accepted.findings);
        assert_eq!(accepted.counts.expected_tee_lease_guard_shutdown_fatal, 4);

        let mut later_unrelated_shutdown = expected.clone();
        later_unrelated_shutdown[0]
            .1
            .push_str("\nINFO outbe_chain: consensus stack shutting down");
        later_unrelated_shutdown[1].1.push_str(
            "\nINFO reth::cli: Starting Reth version=1.2.3\n\
             INFO outbe_chain: consensus stack shutting down",
        );
        let accepted_with_later_shutdown = audit_loaded_logs_with_all_expectations(
            &later_unrelated_shutdown,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        );
        assert!(
            accepted_with_later_shutdown.is_clean(),
            "{:?}",
            accepted_with_later_shutdown.findings
        );
        assert_eq!(
            accepted_with_later_shutdown
                .counts
                .expected_tee_lease_guard_shutdown_fatal,
            4
        );

        let unarmed = audit_loaded_logs_with_expectations(&expected, 4, None, None, None);
        assert!(!unarmed.is_clean());

        let wrong_validator =
            audit_loaded_logs_with_all_expectations(&expected, 4, None, None, None, Some(2), None);
        assert!(!wrong_validator.is_clean());

        let missing_sink = vec![expected[0].clone()];
        assert!(!audit_loaded_logs_with_all_expectations(
            &missing_sink,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());

        let mut duplicate_sink = expected.clone();
        duplicate_sink.push(expected[0].clone());
        assert!(!audit_loaded_logs_with_all_expectations(
            &duplicate_sink,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());

        let reordered = expected
            .iter()
            .map(|(path, content)| {
                let lines = content.lines().collect::<Vec<_>>();
                (path.clone(), format!("{}\n{}", lines[1], lines[0]))
            })
            .collect::<Vec<_>>();
        assert!(!audit_loaded_logs_with_all_expectations(
            &reordered,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());

        let process_epoch_crossing = expected
            .iter()
            .map(|(path, content)| {
                (
                    path.clone(),
                    content.replacen(
                        "\nINFO outbe_chain: consensus stack shutting down",
                        "\nINFO reth::cli: Starting Reth version=1.2.3\nINFO outbe_chain: consensus stack shutting down",
                        1,
                    ),
                )
            })
            .collect::<Vec<_>>();
        assert!(!audit_loaded_logs_with_all_expectations(
            &process_epoch_crossing,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());

        let mut extra_fatal = expected.clone();
        extra_fatal[0]
            .1
            .push_str("\nERROR outbe_chain: unrelated fatal condition");
        assert!(!audit_loaded_logs_with_all_expectations(
            &extra_fatal,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());

        let mut duplicate_fatal_trailer = expected.clone();
        duplicate_fatal_trailer[0]
            .1
            .push_str("\nERROR engine::tree: Fatal error");
        assert!(!audit_loaded_logs_with_all_expectations(
            &duplicate_fatal_trailer,
            4,
            None,
            None,
            None,
            Some(3),
            None,
        )
        .is_clean());
    }

    #[test]
    fn tee_lease_full_node_accepts_only_owned_controlled_shutdown() {
        let expected = tee_lease_full_node_shutdown_bundle(4);
        let proof = full_node_proof(&expected);
        let audit = |logs: &[(PathBuf, String)],
                     proof: &crate::world::state::TeeLeaseShutdownV1| {
            audit_loaded_logs_with_all_expectations(logs, 4, None, None, None, None, Some(proof))
        };
        let accepted = audit(&expected, &proof);
        assert!(accepted.is_clean(), "{:?}", accepted.findings);
        assert_eq!(accepted.counts.expected_tee_lease_guard_shutdown_fatal, 4);
        let unarmed = audit_loaded_logs_with_expectations(&expected, 4, None, None, None);
        assert!(!unarmed.is_clean());
        assert_eq!(unarmed.findings.len(), 2);
        let mut later_expiry = expected.clone();
        let guard = expected[0].1.lines().next().unwrap();
        later_expiry[0].1.push_str(guard);
        later_expiry[0].1.push('\n');
        assert!(!audit(&later_expiry, &proof).is_clean());
        for (code, signal, slot, pid, deadline) in [
            (None, None, 4, 1234, 1789793371),
            (Some(1), None, 4, 1234, 1789793371),
            (None, Some(15), 4, 1234, 1789793371),
            (Some(0), Some(9), 4, 1234, 1789793371),
            (Some(0), None, 5, 1234, 1789793371),
            (Some(0), None, 4, 0, 1789793371),
            (Some(0), None, 4, 1234, 1789793372),
        ] {
            let mut bad = proof.clone();
            bad.exit_code = code;
            bad.exit_signal = signal;
            bad.slot = slot;
            bad.pid = pid;
            bad.deadline = deadline;
            assert!(!audit(&expected, &bad).is_clean());
        }
        for trailer in [
            "thread tokio-rt panicked",
            "ERROR engine::tree: Fatal error",
            "ERROR reth::cli: Fatal error in consensus engine",
            "ERROR other: unrelated fatal",
        ] {
            let mut bad = expected.clone();
            bad[0].1.push_str(trailer);
            assert!(!audit(&bad, &proof).is_clean(), "{trailer}");
        }
    }

    #[test]
    fn tee_lease_full_node_rejects_every_incomplete_or_cross_process_bundle() {
        let baseline = tee_lease_full_node_shutdown_bundle(4);
        let check = |logs: &[(PathBuf, String)]| {
            let proof = full_node_proof(logs);
            audit_loaded_logs_with_all_expectations(logs, 4, None, None, None, None, Some(&proof))
                .is_clean()
        };
        assert!(check(&baseline));
        for sink in 0..2 {
            let lines: Vec<_> = baseline[sink].1.lines().collect();
            for removed in 0..lines.len() {
                let mut bad = baseline.clone();
                bad[sink].1 = lines
                    .iter()
                    .enumerate()
                    .filter_map(|(i, line)| (i != removed).then_some(*line))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!check(&bad), "missing sink {sink} line {removed}");
            }
            for (duplicated, line) in lines.iter().enumerate() {
                let mut bad = baseline.clone();
                bad[sink].1.push_str(line);
                bad[sink].1.push('\n');
                assert!(!check(&bad), "duplicate sink {sink} line {duplicated}");
            }
        }
        for replacement in [
            "INFO reth::cli: Starting Reth version=other\nINFO outbe_chain: consensus stack shutting down",
            "INFO outbe_chain: consensus stack shutting down\nINFO reth::cli: Starting Reth version=other",
        ] {
            let mut bad = baseline.clone();
            bad[1].1 = bad[1].1.replace("INFO outbe_chain: consensus stack shutting down", replacement);
            assert!(!check(&bad));
        }
        let mut reordered = baseline.clone();
        reordered[1].1 = reordered[1].1.replace("DEBUG reth::cli: received engine shutdown request\nDEBUG engine::tree: received terminate request",
            "DEBUG engine::tree: received terminate request\nDEBUG reth::cli: received engine shutdown request");
        assert!(!check(&reordered));
        for replacement in [
            "WARN outbe_chain:",
            "ERROR other:",
            "ERROR outbe_chain: unrelated reason ",
        ] {
            let mut bad = baseline.clone();
            bad[0].1 = bad[0].1.replace("ERROR outbe_chain:", replacement);
            assert!(!check(&bad));
        }
        let mut too_far = baseline.clone();
        too_far[1].1 = too_far[1].1.replace(
            "DEBUG reth::cli: shutting down gracefully",
            &format!(
                "{}DEBUG reth::cli: shutting down gracefully",
                "INFO noise\n".repeat(65)
            ),
        );
        assert!(!check(&too_far));
        assert!(!check(&baseline[..1]));
        let mut mismatched = baseline.clone();
        mismatched[1].0 = PathBuf::from("scenario-1/validator-5/logs/54322345/reth.log");
        assert!(!check(&mismatched));
    }

    #[test]
    fn tee_lease_full_node_immutable_intervals_exclude_future_and_detect_changes() {
        let baseline = tee_lease_full_node_shutdown_bundle(4);
        let proof = full_node_proof(&baseline);
        let mut appended = baseline.clone();
        for (_, log) in &mut appended {
            log.push_str("INFO reth::cli: Starting Reth version=new\n");
        }
        assert!(audit_loaded_logs_with_all_expectations(
            &appended,
            4,
            None,
            None,
            None,
            None,
            Some(&proof)
        )
        .is_clean());
        for changed in ["", "INFO replaced process\n"] {
            let mut bad = baseline.clone();
            bad[1].1 = changed.to_owned();
            assert!(!audit_loaded_logs_with_all_expectations(
                &bad,
                4,
                None,
                None,
                None,
                None,
                Some(&proof)
            )
            .is_clean());
        }
        let mut previous = baseline.clone();
        let mut shifted = proof.clone();
        for (index, (_, content)) in previous.iter_mut().enumerate() {
            *content = format!("INFO previous process\n{content}");
            shifted.sinks[index].start += 22;
            shifted.sinks[index].end += 22;
        }
        assert!(audit_loaded_logs_with_all_expectations(
            &previous,
            4,
            None,
            None,
            None,
            None,
            Some(&shifted)
        )
        .is_clean());
    }

    #[test]
    fn runtime_log_discovery_is_explicit_and_rotation_aware() {
        for path in [
            "validator-0/node.log",
            "validator-0/node.log.1",
            "validator-0/enclave.log",
            "validator-0/logs/54322345/reth.log",
            "validator-0/logs/54322345/reth.log.1",
        ] {
            assert!(super::is_runtime_log(Path::new(path)), "missed {path}");
        }
        assert!(!super::is_runtime_log(Path::new(
            "validator-0/data/rocksdb/000011.log"
        )));
    }
}
