//! IPv4 forwarding sysctl scan (runtime and persistent) and managed NAT file ownership probes.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::HostEnv;
use crate::nat::{
    parse_persistent_sysctl, FileOwnership, HostNatPaths, ManagedFileStates, PersistentSysctlState,
    OWNERSHIP_MARKER,
};

pub(super) fn read_runtime_ip_forward(env: &HostEnv) -> Option<u8> {
    let raw = fs::read_to_string(env.path("/proc/sys/net/ipv4/ip_forward")).ok()?;
    let val = raw.trim().parse::<u8>().ok()?;
    (val == 0 || val == 1).then_some(val)
}

pub(super) fn scan_persistent_ip_forward(paths: &HostNatPaths) -> PersistentSysctlState {
    let mut lines = Vec::new();
    for (dir_idx, dir) in paths.sysctl_scan_dirs.iter().enumerate() {
        for path in sorted_conf_files(dir) {
            lines.push(format!("FILE {dir_idx} {}", path.display()));
            push_ip_forward_settings(&path, &mut lines);
        }
    }
    if paths.sysctl_conf_path.is_file() {
        lines.push(format!("CONF {}", paths.sysctl_conf_path.display()));
        push_ip_forward_settings(&paths.sysctl_conf_path, &mut lines);
    }
    parse_persistent_sysctl(&lines.join("\n"), "90-outbe-up-nat.conf")
}

/// List the `*.conf` files of a sysctl directory in sorted order. A missing or unreadable
/// directory gives an empty list.
fn sorted_conf_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut conf_paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("conf"))
        .collect();
    conf_paths.sort();
    conf_paths
}

/// Append one `SET <path>:<line>` entry for each IPv4 forwarding line of a readable sysctl file.
fn push_ip_forward_settings(path: &Path, lines: &mut Vec<String>) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    lines.extend(
        content
            .lines()
            .filter(|line| is_ip_forward_sysctl_line(line))
            .map(|line| format!("SET {}:{}", path.display(), line.trim())),
    );
}

fn is_ip_forward_sysctl_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
        return false;
    }
    let Some((lhs, _)) = trimmed.split_once('=') else {
        return false;
    };
    let key = lhs.trim();
    key == "net.ipv4.ip_forward" || key == "net/ipv4/ip_forward"
}

pub(super) fn probe_managed_nat_files(paths: &HostNatPaths) -> ManagedFileStates {
    ManagedFileStates {
        rules: probe_single_owned_file(&paths.rules_path),
        service: probe_single_owned_file(&paths.service_path),
        sysctl: probe_single_owned_file(&paths.sysctl_path),
    }
}

fn probe_single_owned_file(path: &Path) -> FileOwnership {
    match fs::read_to_string(path) {
        Ok(content) if content.contains(OWNERSHIP_MARKER) => FileOwnership::Owned,
        Ok(_) => FileOwnership::Foreign,
        Err(_) if fs::symlink_metadata(path).is_ok() => FileOwnership::Foreign,
        Err(_) => FileOwnership::Absent,
    }
}
