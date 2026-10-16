//! The owned, controlled shutdown of a FullNode whose TEE lease expired.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::discovery::{process_start_boundary, validator_log_index};
use super::lease_guard::{
    exact_consensus_stack_shutdown, exact_tee_lease_expired_full_node_guard_shutdown,
};
use super::LogRecords;
use crate::world::state::{TeeLeaseLogIntervalV1, TeeLeaseShutdownV1};

/// How far after the guard record the shutdown records may appear.
const SHUTDOWN_WINDOW: usize = 64;

const RETH_TERMINATION_MARKERS: [&str; 4] = [
    "reth::cli: shutting down gracefully",
    "reth::cli: shutdown signal received, terminating engine",
    "engine::tree: received terminate request",
    "engine::tree: persistence complete, signaling termination",
];

pub(super) fn expected_tee_lease_full_node_shutdown_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    proof: &TeeLeaseShutdownV1,
) -> Result<LogRecords, String> {
    validate_owned_exit(proof, validators)?;
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
        let offset = retained_interval_offset(logs, interval)?;
        let lines: Vec<_> = interval.content.lines().collect();
        let (guard, stack) = guard_and_stack(&lines, interval, proof.deadline)?;
        // Only the guard and stack records are exempted. Generic fatal/panic
        // records, including EngineFatal/CliFatal, remain ordinary audit failures.
        accepted.insert((interval.path.clone(), offset + guard));
        accepted.insert((interval.path.clone(), offset + stack));
        if sink == "reth.log" {
            ensure_reth_termination_records(&lines, guard, stack)?;
        }
        ensure_single_process(&lines, guard)?;
    }
    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err("FullNode lease shutdown omitted an expected sink".to_owned());
    }
    Ok(accepted)
}

fn validate_owned_exit(proof: &TeeLeaseShutdownV1, validators: usize) -> Result<(), String> {
    let clean_exit = proof.exit_code == Some(0) && proof.exit_signal.is_none();
    if proof.slot != validators || proof.pid == 0 || proof.deadline == 0 || !clean_exit {
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
    Ok(())
}

/// The interval's first line number in its retained sink, once the retained
/// bytes are proven identical and line-aligned.
fn retained_interval_offset(
    logs: &[(PathBuf, String)],
    interval: &TeeLeaseLogIntervalV1,
) -> Result<usize, String> {
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
    Ok(full_content[..start].lines().count())
}

fn guard_and_stack(
    lines: &[&str],
    interval: &TeeLeaseLogIntervalV1,
    deadline: u64,
) -> Result<(usize, usize), String> {
    let guards = matching_lines(lines, exact_tee_lease_expired_full_node_guard_shutdown);
    let stacks = matching_lines(lines, exact_consensus_stack_shutdown);
    let ([guard], [stack]) = (guards.as_slice(), stacks.as_slice()) else {
        return Err(format!(
            "expected exactly one FullNode guard/stack pair in {}",
            interval.path.display()
        ));
    };
    let expected_reason =
        format!("reason=finalized TEE lease expired at {deadline}; stop node and run tee join");
    if !lines[*guard].trim_end().ends_with(&expected_reason)
        || stack <= guard
        || stack - guard > SHUTDOWN_WINDOW
    {
        return Err("FullNode lease guard deadline or shutdown ordering differs".to_owned());
    }
    Ok((*guard, *stack))
}

fn matching_lines(lines: &[&str], matches: fn(&str) -> bool) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| matches(line).then_some(index))
        .collect()
}

fn ensure_reth_termination_records(
    lines: &[&str],
    guard: usize,
    stack: usize,
) -> Result<(), String> {
    let mut last = stack;
    for marker in RETH_TERMINATION_MARKERS {
        let matches = matching_debug_records(lines, marker);
        let [record] = matches.as_slice() else {
            return Err(format!(
                "missing, duplicate or unordered FullNode termination record: {marker}"
            ));
        };
        if *record <= last || *record - guard > SHUTDOWN_WINDOW {
            return Err(format!(
                "missing, duplicate or unordered FullNode termination record: {marker}"
            ));
        }
        last = *record;
    }
    Ok(())
}

fn matching_debug_records(lines: &[&str], marker: &str) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let (prefix, _) = line.split_once(marker)?;
            (prefix.split_whitespace().last() == Some("DEBUG") && line.trim_end().ends_with(marker))
                .then_some(index)
        })
        .collect()
}

fn ensure_single_process(lines: &[&str], guard: usize) -> Result<(), String> {
    if lines[guard + 1..]
        .iter()
        .any(|line| process_start_boundary(line))
    {
        return Err("FullNode lease shutdown evidence crossed a process replacement".to_owned());
    }
    let launches = lines[..guard]
        .iter()
        .filter(|line| line.contains("reth::cli: Starting Reth version="))
        .count();
    if launches > 1 {
        return Err("FullNode lease interval contains multiple process launches".to_owned());
    }
    Ok(())
}
