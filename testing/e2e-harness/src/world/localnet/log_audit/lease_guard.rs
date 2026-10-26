//! The exact two-sink shutdown bundle of a jailed validator's TEE lease guard.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::discovery::{contains_nonfatal_alarm, process_start_boundary, runtime_sink};
use super::LogRecords;

/// How far after the guard record its consensus-stack shutdown may appear.
const STACK_SHUTDOWN_WINDOW: usize = 64;

pub(super) fn expected_tee_lease_guard_shutdown_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    expected_validator: usize,
) -> Result<LogRecords, String> {
    if expected_validator >= validators {
        return Err(format!(
            "expected TEE lease guard shutdown validator-{expected_validator} is outside the {validators}-validator committee"
        ));
    }

    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    for (path, content) in logs {
        let Some(sink) = runtime_sink(path, validators, expected_validator) else {
            continue;
        };
        let lines = content.lines().collect::<Vec<_>>();
        let Some(records) = guard_shutdown_bundle(path, &lines)? else {
            continue;
        };
        if !sinks.insert(sink) {
            return Err(format!(
                "duplicate expected TEE lease guard shutdown sink {sink} for validator-{expected_validator}"
            ));
        }
        accepted.extend(records.into_iter().map(|index| (path.clone(), index)));
    }

    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err(format!(
            "expected one node.log and one reth.log TEE lease guard shutdown bundle for validator-{expected_validator}, observed {sinks:?}"
        ));
    }
    Ok(accepted)
}

/// The guard and its stack-shutdown line indices, or `None` without a guard.
fn guard_shutdown_bundle(path: &Path, lines: &[&str]) -> Result<Option<[usize; 2]>, String> {
    let guard_indices = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| exact_tee_lease_jailed_guard_shutdown(line).then_some(index))
        .collect::<Vec<_>>();
    match guard_indices.as_slice() {
        [] => Ok(None),
        [guard] => stack_shutdown_after(path, lines, *guard).map(|stack| Some([*guard, stack])),
        many => Err(format!(
            "expected exactly one TEE lease guard in {}, observed {}",
            path.display(),
            many.len()
        )),
    }
}

fn stack_shutdown_after(path: &Path, lines: &[&str], guard: usize) -> Result<usize, String> {
    let mut cursor = guard.saturating_add(1);
    loop {
        let Some(line) = lines.get(cursor).copied() else {
            return Err(format!(
                "incomplete expected TEE lease guard shutdown bundle in {}: missing StackShutdown",
                path.display()
            ));
        };
        if cursor.saturating_sub(guard) > STACK_SHUTDOWN_WINDOW || process_start_boundary(line) {
            return Err(format!(
                "expected TEE lease guard shutdown crossed its line or process-epoch bound in {} before StackShutdown",
                path.display()
            ));
        }
        if exact_tee_lease_jailed_guard_shutdown(line) {
            return Err(format!(
                "malformed expected TEE lease guard shutdown bundle in {}: expected StackShutdown, observed Guard",
                path.display()
            ));
        }
        if exact_consensus_stack_shutdown(line) {
            return Ok(cursor);
        }
        cursor = cursor.saturating_add(1);
    }
}

pub(super) fn exact_tee_lease_jailed_guard_shutdown(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.trim_end().ends_with(
        "outbe_chain::launch::shutdown: finalized tee lease guard requested node shutdown reason=validator is jailed; complete ordinary unjail and then run tee join",
    ) && line.contains("error")
        && !contains_nonfatal_alarm(&line)
}

pub(super) fn exact_tee_lease_expired_full_node_guard_shutdown(line: &str) -> bool {
    const PREFIX: &str = "outbe_chain::launch::shutdown: finalized tee lease guard requested node shutdown reason=finalized tee lease expired at ";
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

pub(super) fn exact_consensus_stack_shutdown(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.trim_end()
        .ends_with("outbe_chain::launch::node::consensus: consensus stack shutting down")
        && line.contains("info")
        && !contains_nonfatal_alarm(&line)
}
