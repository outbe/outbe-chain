//! Versioned runtime audit events: typed payload failures and their cancellations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use alloy_primitives::B256;
use outbe_primitives::runtime_audit_v1::{
    BODY_READ_REQUEST_DEADLINE, EVENT_FIELD, FAILURE_KIND_FIELD, PAYLOAD_EXECUTION_FAILED,
    PROCESS_INSTANCE_FIELD, PROPOSAL_VIEW_CANCELLED, SCHEMA_FIELD, SCHEMA_VERSION,
};

use super::discovery::is_machine_audit_log;

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

/// Typed audit events seen in machine-audit logs, keyed by payload identity.
#[derive(Default)]
pub(super) struct TypedPayloadEvents {
    cancellations: BTreeSet<PayloadIdentity>,
    failures: BTreeMap<PayloadIdentity, BTreeMap<(B256, String), String>>,
}

impl TypedPayloadEvents {
    pub(super) fn observe(
        &mut self,
        path: &Path,
        line: &str,
        location: &str,
        findings: &mut Vec<String>,
    ) {
        let has_schema = structured_field(line, SCHEMA_FIELD).is_some();
        let has_event = structured_field(line, EVENT_FIELD).is_some();
        if (!has_schema && !has_event) || !is_machine_audit_log(path) {
            return;
        }
        let source = path.parent().unwrap_or(path).to_path_buf();
        match parse_audit_event(line, source, location) {
            Ok(RuntimeAuditEvent::ProposalViewCancelled(identity)) => {
                self.cancellations.insert(identity);
            }
            Ok(RuntimeAuditEvent::PayloadExecutionFailed(failure)) => {
                self.failures
                    .entry(failure.identity)
                    .or_default()
                    .entry((failure.tx_hash, failure.failure_kind))
                    .or_insert(failure.location);
            }
            Err(reason) => {
                findings.push(format!(
                    "{location}: malformed runtime audit event: {reason}"
                ));
            }
        }
    }

    /// Counts failures that one cancelled deadline episode accounts for; every
    /// other typed failure becomes a finding.
    pub(super) fn accepted_cancellations(self, findings: &mut Vec<String>) -> usize {
        let mut accepted = BTreeSet::new();
        for (identity, outcomes) in self.failures {
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
            if failure_kind == BODY_READ_REQUEST_DEADLINE && self.cancellations.contains(&identity)
            {
                accepted.insert(identity);
            } else {
                findings.push(format!(
                    "{}: unaccepted typed payload failure: {}",
                    location, failure_kind
                ));
            }
        }
        accepted.len()
    }
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
