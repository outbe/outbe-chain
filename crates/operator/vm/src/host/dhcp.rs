//! Host DHCP service detection (systemd unit or running dnsmasq) and `dhcp-hosts` validation.

use std::{fs, os::unix::fs::MetadataExt, path::Path};

use eyre::{ensure, Context, Result};

use super::{
    dnsmasq_argv::{parse_dnsmasq_argv, ParsedDnsmasqArgs},
    probe::{extract_systemd_exec_argv, query_systemd_unit, SystemdUnitProps},
    DhcpStatus, HostEnv,
};
use crate::dnsmasq::{count_dnsmasq_leases, validate_dnsmasq_hosts_format};

const VALIDATOR_DHCP_SERVICE: &str = "validator-vm-dhcp.service";

/// Service facts of a detected DHCP server that do not come from its command line.
struct DhcpService {
    unit_name: Option<String>,
    unit_enabled: bool,
    unit_active: bool,
    pid: Option<u32>,
    reload_mode: String,
}

pub(super) fn detect_dhcp(bridge_name: &str, env: &HostEnv) -> DhcpStatus {
    let unit = query_systemd_unit(VALIDATOR_DHCP_SERVICE);
    if let Some(status) = detect_dhcp_unit(&unit, bridge_name) {
        return status;
    }
    // Fallback: scan `<sys_root>/proc/<pid>/cmdline` for a running dnsmasq bound to `bridge_name`.
    if let Some((pid, parsed)) = scan_proc_for_dnsmasq(bridge_name, env) {
        let reservations_count = read_optional(parsed.hosts_file.as_deref())
            .and_then(|c| validate_dnsmasq_hosts_format(&c).ok())
            .unwrap_or(0);
        let service = DhcpService {
            unit_name: None,
            unit_enabled: false,
            unit_active: true,
            pid: Some(pid),
            reload_mode: "unsupported".to_owned(),
        };
        return detected_dhcp_status(service, parsed, reservations_count);
    }
    absent_dhcp_status()
}

/// Build the DHCP status from the validator DHCP systemd unit when that unit serves
/// `bridge_name`.
fn detect_dhcp_unit(unit: &SystemdUnitProps, bridge_name: &str) -> Option<DhcpStatus> {
    if !unit.is_loaded() {
        return None;
    }
    let argv = extract_systemd_exec_argv(&unit.exec_start);
    let parsed = parse_dnsmasq_argv(&argv);
    let matches_bridge = parsed
        .interface
        .as_deref()
        .is_none_or(|iface| iface == bridge_name);
    let serves_bridge = matches_bridge && (!argv.is_empty() || unit.is_active());
    if !serves_bridge {
        return None;
    }
    let reservations_count = read_optional(parsed.hosts_file.as_deref())
        .map(|c| validate_dnsmasq_hosts_format(&c).unwrap_or_else(|_| count_host_lines(&c)))
        .unwrap_or(0);
    let reload_mode = if !unit.exec_reload.trim().is_empty() {
        "systemctl-reload".to_owned()
    } else {
        "unsupported".to_owned()
    };
    let service = DhcpService {
        unit_name: Some(VALIDATOR_DHCP_SERVICE.to_owned()),
        unit_enabled: unit.is_enabled(),
        unit_active: unit.is_active(),
        pid: unit.main_pid,
        reload_mode,
    };
    Some(detected_dhcp_status(service, parsed, reservations_count))
}

/// Count the lines of a hosts file that are not empty and not only a comment.
fn count_host_lines(content: &str) -> usize {
    content
        .lines()
        .filter(|l| {
            let t = l.split('#').next().unwrap_or("").trim();
            !t.is_empty()
        })
        .count()
}

fn read_optional(path: Option<&Path>) -> Option<String> {
    path.and_then(|p| fs::read_to_string(p).ok())
}

fn detected_dhcp_status(
    service: DhcpService,
    parsed: ParsedDnsmasqArgs,
    reservations_count: usize,
) -> DhcpStatus {
    let active_leases_count = read_optional(parsed.leases_file.as_deref())
        .map(|c| count_dnsmasq_leases(&c))
        .unwrap_or(0);
    DhcpStatus {
        detected: true,
        unit_name: service.unit_name,
        unit_enabled: service.unit_enabled,
        unit_active: service.unit_active,
        pid: service.pid,
        hosts_file: parsed.hosts_file,
        leases_file: parsed.leases_file,
        pid_file: parsed.pid_file,
        pool_start: parsed.pool_start,
        pool_end: parsed.pool_end,
        netmask: parsed.netmask,
        lease_duration: parsed.lease_duration,
        reload_mode: service.reload_mode,
        reservations_count,
        active_leases_count,
    }
}

fn absent_dhcp_status() -> DhcpStatus {
    DhcpStatus {
        detected: false,
        unit_name: None,
        unit_enabled: false,
        unit_active: false,
        pid: None,
        hosts_file: None,
        leases_file: None,
        pid_file: None,
        pool_start: None,
        pool_end: None,
        netmask: None,
        lease_duration: None,
        reload_mode: "none".to_owned(),
        reservations_count: 0,
        active_leases_count: 0,
    }
}

fn scan_proc_for_dnsmasq(bridge_name: &str, env: &HostEnv) -> Option<(u32, ParsedDnsmasqArgs)> {
    let entries = fs::read_dir(env.path("/proc")).ok()?;
    entries.flatten().find_map(|entry| {
        let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
        let argv = read_dnsmasq_cmdline(&entry.path())?;
        let parsed = parse_dnsmasq_argv(&argv);
        (parsed.interface.as_deref() == Some(bridge_name)).then_some((pid, parsed))
    })
}

/// Read `<proc_entry>/cmdline` and return its arguments when the executable is dnsmasq.
fn read_dnsmasq_cmdline(proc_entry: &Path) -> Option<Vec<String>> {
    let raw = fs::read(proc_entry.join("cmdline")).ok()?;
    let argv: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let is_dnsmasq = argv
        .first()
        .is_some_and(|exe| Path::new(exe).file_name().and_then(|n| n.to_str()) == Some("dnsmasq"));
    is_dnsmasq.then_some(argv)
}

pub(super) fn read_and_validate_hosts_file(
    hosts_file: &Path,
    expected_owner: (u32, u32),
) -> Result<String> {
    let meta = fs::symlink_metadata(hosts_file).wrap_err_with(|| {
        format!(
            "discovered DHCP hosts file does not exist: {}",
            hosts_file.display()
        )
    })?;
    ensure!(
        !meta.file_type().is_symlink() && meta.file_type().is_file(),
        "refusing DHCP hosts file {}: must be a regular file (not a symlink)",
        hosts_file.display()
    );
    let actual_owner = (meta.uid(), meta.gid());
    ensure!(
        actual_owner == expected_owner,
        "refusing DHCP hosts file {}: expected owner {}:{}, found {}:{}",
        hosts_file.display(),
        expected_owner.0,
        expected_owner.1,
        actual_owner.0,
        actual_owner.1
    );
    let content = fs::read_to_string(hosts_file)
        .wrap_err_with(|| format!("read DHCP hosts file {}", hosts_file.display()))?;
    validate_dnsmasq_hosts_format(&content).wrap_err_with(|| {
        format!(
            "refusing DHCP hosts file {}: unrecognized file format",
            hosts_file.display()
        )
    })?;
    Ok(content)
}
