//! Runtime-log discovery and line classification shared by every expectation.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr};

use super::lease_guard::{
    exact_tee_lease_expired_full_node_guard_shutdown, exact_tee_lease_jailed_guard_shutdown,
};

const UNEXPECTED_RUNTIME_MARKERS: &[&str] = &[
    "radicle integration shutdown deadline exceeded",
    "radicle integration shutdown failed",
    "radicle observer join deadline exceeded",
    "radicle drain failed before transport shutdown",
    "radicle endpoint actor stopped",
    "embedded ocomp requested node shutdown",
];

pub(super) fn collect_logs(dir: &Path, logs: &mut Vec<PathBuf>) -> Result<()> {
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

pub(crate) fn first_runtime_log_line_containing(
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

pub(super) fn is_runtime_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                "node.log" | "enclave.log" | "radicle.log" | "snapshot-exporter.log"
            ) || name.starts_with("node.log.")
                || is_worker_log_name(name)
                || is_reth_runtime_log_name(name)
        })
}

fn is_worker_log_name(name: &str) -> bool {
    name.strip_prefix("worker-")
        .and_then(|suffix| suffix.strip_suffix(".log"))
        .is_some_and(|ordinal| {
            !ordinal.is_empty() && ordinal.bytes().all(|byte| byte.is_ascii_digit())
        })
}

pub(super) fn is_reth_runtime_log_name(name: &str) -> bool {
    name == "reth.log" || name.starts_with("reth.log.")
}

pub(super) fn is_machine_audit_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "node.log" || name.starts_with("node.log."))
}

pub(super) fn unexpected_log_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let lease_guard = exact_tee_lease_jailed_guard_shutdown(line)
        || exact_tee_lease_expired_full_node_guard_shutdown(line);
    lease_guard
        || fatal_at_runtime_severity(line)
        || contains_nonfatal_alarm(&lower)
        || UNEXPECTED_RUNTIME_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
}

pub(super) fn fatal_at_runtime_severity(line: &str) -> bool {
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

pub(super) fn contains_nonfatal_alarm(line: &str) -> bool {
    let has = |needle: &str| line.contains(needle);
    let dkg_share_reveal = has("dkg") && has("share") && (has("reveal") || has("disclos"));
    let paired_alarm =
        dkg_share_reveal || (has("vrf") && has("alarm")) || (has("projection") && has("fatal"));
    let resource_exhaustion = has("eagain") || has("resource temporarily unavailable");
    has("panic") || resource_exhaustion || paired_alarm
}

pub(super) fn process_start_boundary(line: &str) -> bool {
    line.contains("reth::cli: Initialized tracing, debug log directory:")
        || line.contains("reth::cli: Starting Reth version=")
}

pub(super) fn validator_log_index(path: &Path, validators: usize) -> Option<usize> {
    path.ancestors().find_map(|ancestor| {
        let name = ancestor.file_name()?.to_str()?;
        let index = name.strip_prefix("validator-")?.parse().ok()?;
        (index < validators).then_some(index)
    })
}

pub(super) fn validator_node_log_index(path: &Path, validators: usize) -> Option<usize> {
    if path.file_name()?.to_str()? != "node.log" {
        return None;
    }
    let validator = path.parent()?.file_name()?.to_str()?;
    let index = validator.strip_prefix("validator-")?.parse().ok()?;
    (index < validators).then_some(index)
}

/// The `node.log`/`reth.log` sink of runtime slot `slot` (out of `slots`).
pub(super) fn runtime_sink(path: &Path, slots: usize, slot: usize) -> Option<&str> {
    if validator_log_index(path, slots) != Some(slot) {
        return None;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| matches!(*name, "node.log" | "reth.log"))
}
