//! Legacy `newPayload` request-deadline cancellations, matched by exact text.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use alloy_primitives::B256;

use super::discovery::{contains_nonfatal_alarm, is_reth_runtime_log_name, validator_log_index};

/// Body-read failures that a request-deadline cancellation never excuses.
const OTHER_BODY_READ_FAILURES: [&str; 3] = [
    "body read unavailable",
    "BodyReadCorruption",
    "StorageError::Backend",
];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RequestDeadlineIdentity {
    validator: usize,
    block_number: u64,
    block_hash: B256,
}

#[derive(Debug, Default)]
pub(super) struct LegacyRequestDeadlineCancellations {
    pub(super) cancellations: usize,
    pub(super) accepted_fatal_records: BTreeSet<(PathBuf, usize)>,
}

/// Compatibility adapter for the pre-existing `newPayload` cancellation
/// records. New exceptions must use versioned runtime audit events instead.
pub(super) fn legacy_request_deadline_cancellations(
    logs: &[(PathBuf, String)],
    validators: usize,
) -> LegacyRequestDeadlineCancellations {
    let runtime_nodes = validators.saturating_add(1);
    let complete = complete_request_deadlines(logs, runtime_nodes);
    let mut accepted = BTreeSet::new();
    for (path, content) in logs {
        let Some(validator) = validator_log_index(path, runtime_nodes) else {
            continue;
        };
        let lines = content.lines().collect::<Vec<_>>();
        accepted.extend(
            accepted_deadline_fatals(path, &lines, validator, &complete)
                .into_iter()
                .map(|index| (path.clone(), index)),
        );
    }

    LegacyRequestDeadlineCancellations {
        cancellations: complete.len(),
        accepted_fatal_records: accepted,
    }
}

/// Deadlines with both the typed `newPayload` error and the terminal receiver drop.
fn complete_request_deadlines(
    logs: &[(PathBuf, String)],
    runtime_nodes: usize,
) -> BTreeSet<RequestDeadlineIdentity> {
    let mut typed = BTreeSet::new();
    let mut terminal = BTreeSet::new();
    for (path, content) in logs {
        let is_reth_log = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_reth_runtime_log_name);
        let Some(validator) = validator_log_index(path, runtime_nodes).filter(|_| is_reth_log)
        else {
            continue;
        };
        for line in content.lines() {
            if exact_request_deadline_fatal(line) {
                typed.extend(scoped_new_payload_identity(line, validator));
            }
            if exact_request_deadline_receiver_drop(line) {
                terminal.extend(terminal_payload_identity(line, validator));
            }
        }
    }
    typed.intersection(&terminal).copied().collect()
}

fn accepted_deadline_fatals(
    path: &Path,
    lines: &[&str],
    validator: usize,
    complete: &BTreeSet<RequestDeadlineIdentity>,
) -> Vec<usize> {
    let is_node_log = path.file_name().and_then(|name| name.to_str()) == Some("node.log");
    lines
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, line)| exact_request_deadline_fatal(line))
        .filter_map(|(index, line)| {
            let accepted = match scoped_new_payload_identity(line, validator)
                .or_else(|| terminal_payload_identity(line, validator))
            {
                Some(identity) => complete.contains(&identity),
                None => {
                    is_node_log
                        && line.contains("outbe::executor")
                        && receiver_drop_follows(lines, index, validator, complete)
                }
            };
            accepted.then_some(index)
        })
        .collect()
}

fn receiver_drop_follows(
    lines: &[&str],
    index: usize,
    validator: usize,
    complete: &BTreeSet<RequestDeadlineIdentity>,
) -> bool {
    lines
        .iter()
        .take(index.saturating_add(5))
        .skip(index + 1)
        .any(|candidate| {
            exact_request_deadline_receiver_drop(candidate)
                && terminal_payload_identity(candidate, validator)
                    .is_some_and(|identity| complete.contains(&identity))
        })
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

fn excludes_other_body_read_failures(line: &str) -> bool {
    !OTHER_BODY_READ_FAILURES
        .iter()
        .any(|marker| line.contains(marker))
}

fn exact_request_deadline_receiver_drop(line: &str) -> bool {
    line.contains("Failed to deliver newPayload response, receiver dropped (request cancelled)")
        && line.contains("Err(Internal(BlockExecutionError(Other(")
        && line.contains("fatal: body read request deadline exceeded")
        && excludes_other_body_read_failures(line)
}

fn exact_request_deadline_fatal(line: &str) -> bool {
    line.matches("fatal: body read request deadline exceeded")
        .count()
        == 1
        && !contains_nonfatal_alarm(&line.to_ascii_lowercase())
        && excludes_other_body_read_failures(line)
}
