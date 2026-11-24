//! Persisted VM deployment state (`vm-state.json`) and idempotent dnsmasq reservation helpers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    network::{
        config::{default_vm_dhcp_leases_file, Ssh},
        remote::NetworkArtifacts,
    },
    vm::vmm::validate_vm_id,
};

pub(crate) const VM_STATE_VERSION: u32 = 1;
pub(crate) const VM_STATE_FILENAME: &str = "vm-state.json";
pub(crate) const DNSMASQ_RESERVATIONS_FILENAME: &str = "dnsmasq-reservations.txt";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum VmFounderPhase {
    /// `CreateVm` succeeded and the VM ID is journaled; MAC/disk not yet staged.
    Created,
    /// `GetInfo` MAC recorded and data disk converted to `hda.img`.
    DiskStaged,
    /// Founder VM provisioning finished.
    #[default]
    Ready,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VmState {
    pub(crate) version: u32,
    pub(crate) network_name: String,
    pub(crate) image_name: String,
    pub(crate) os_image_hash: String,
    pub(crate) founders: Vec<VmFounderState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VmFounderState {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) vm_name: String,
    pub(crate) vm_id: String,
    pub(crate) vmm_host: Ssh,
    pub(crate) vmm_home: String,
    pub(crate) bridge: String,
    pub(crate) bridge_ip: Ipv4Addr,
    #[serde(default)]
    pub(crate) mac: String,
    pub(crate) dhcp_hosts_file: PathBuf,
    #[serde(default = "default_vm_dhcp_leases_file")]
    pub(crate) dhcp_leases_file: PathBuf,
    pub(crate) dhcp_pid_file: PathBuf,
    #[serde(default)]
    pub(crate) phase: VmFounderPhase,
}

impl VmState {
    pub(crate) fn path(artifacts_root: &Path) -> PathBuf {
        artifacts_root.join(VM_STATE_FILENAME)
    }

    pub(crate) fn load(artifacts_root: &Path) -> Result<Self> {
        let path = Self::path(artifacts_root);
        ensure!(
            path.is_file(),
            "VM state file does not exist: {} (run `outbe-up vm create` first)",
            path.display()
        );
        let state: Self = serde_json::from_slice(&fs::read(&path)?)
            .wrap_err_with(|| format!("invalid VM state file: {}", path.display()))?;
        ensure!(
            state.version == VM_STATE_VERSION,
            "unsupported vm-state.json version {}",
            state.version
        );
        ensure!(
            !state.founders.is_empty(),
            "vm-state.json contains no recorded VMs"
        );
        for founder in &state.founders {
            validate_vm_id(&founder.vm_id).wrap_err_with(|| {
                format!("vm-state.json has invalid vm_id for {}", founder.name)
            })?;
        }
        Ok(state)
    }

    pub(crate) fn save(&self, artifacts_root: &Path) -> Result<PathBuf> {
        for founder in &self.founders {
            validate_vm_id(&founder.vm_id)?;
        }
        let path = Self::path(artifacts_root);
        let temp = artifacts_root.join(format!(".{VM_STATE_FILENAME}.tmp"));
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        fs::write(&temp, &bytes)
            .wrap_err_with(|| format!("write temporary VM state: {}", temp.display()))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&temp, &path)
            .wrap_err_with(|| format!("persist VM state: {}", path.display()))?;
        Ok(path)
    }

    /// Return true when every expected founder has been created and reached `Ready`.
    pub(crate) fn is_complete(&self, expected_founders: usize) -> bool {
        self.founders.len() == expected_founders
            && self
                .founders
                .iter()
                .all(|founder| founder.phase == VmFounderPhase::Ready && !founder.mac.is_empty())
    }

    /// Verify that a (possibly partial) state belongs to these network artifacts.
    pub(crate) fn ensure_matches_network(&self, artifacts: &NetworkArtifacts) -> Result<()> {
        ensure!(
            self.network_name == artifacts.config.name,
            "vm-state.json belongs to network `{}`, not `{}`",
            self.network_name,
            artifacts.config.name
        );
        let mut seen_indices = BTreeSet::new();
        for recorded in &self.founders {
            ensure!(
                seen_indices.insert(recorded.index),
                "vm-state.json has duplicate founder index {}",
                recorded.index
            );
            let expected = artifacts
                .config
                .founders
                .get(recorded.index)
                .ok_or_else(|| {
                    eyre::eyre!(
                        "vm-state.json founder index {} exceeds network founder count {}",
                        recorded.index,
                        artifacts.config.founders.len()
                    )
                })?;
            ensure!(
                recorded.name == expected.name,
                "vm-state.json founder mismatch at index {}: recorded `{}` vs expected `{}`",
                recorded.index,
                recorded.name,
                expected.name
            );
        }
        Ok(())
    }

    /// Verify that every founder is recorded and `Ready` before `host-reserve`, `start`, or `status`.
    pub(crate) fn ensure_complete_for_artifacts(&self, artifacts: &NetworkArtifacts) -> Result<()> {
        self.ensure_matches_network(artifacts)?;
        ensure!(
            self.is_complete(artifacts.config.founders.len()),
            "vm-state.json is incomplete ({}/{} founders recorded); resume `outbe-up vm create` or clean up with `outbe-up vm remove`",
            self.founders.len(),
            artifacts.config.founders.len()
        );
        Ok(())
    }
}

/// Format one dnsmasq `--dhcp-hostsfile` reservation line (`mac,ip`).
pub(crate) fn dnsmasq_entry(mac: &str, ip: Ipv4Addr) -> String {
    format!("{},{}", mac.to_ascii_lowercase(), ip)
}

/// Refuse when any requested bridge IP appears in a non-comment line outside our managed block.
///
/// Fails closed on any dnsmasq hosts grammar (`mac,ip`, `mac,set:tag,ip`, `mac,ip,host,lease`,
/// etc.): any line outside our managed block containing a requested IP as a comma-separated field
/// is a conflict unless it is an exact match for `our_mac` (when `our_mac` is non-empty).
pub(crate) fn check_dnsmasq_ip_conflicts(
    existing_hosts: &str,
    requested: &[(String, Ipv4Addr)],
    network_name: &str,
) -> Result<()> {
    let begin = format!("# outbe-up:{network_name}:begin");
    let end = format!("# outbe-up:{network_name}:end");
    let mut inside_ours = false;

    for (line_no, line) in existing_hosts.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == begin {
            inside_ours = true;
            continue;
        }
        if trimmed == end {
            inside_ours = false;
            continue;
        }
        if inside_ours {
            continue;
        }
        let stripped = line.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let fields: Vec<&str> = stripped.split(',').map(str::trim).collect();
        let first_field_lc = fields.first().map(|f| f.to_ascii_lowercase());
        for (our_mac, our_ip) in requested {
            let ip_present = fields
                .iter()
                .any(|field| field.parse::<Ipv4Addr>().ok() == Some(*our_ip));
            if !ip_present {
                continue;
            }
            let our_mac_lc = our_mac.to_ascii_lowercase();
            let exact_own_line = !our_mac_lc.is_empty()
                && fields.len() == 2
                && first_field_lc.as_deref() == Some(our_mac_lc.as_str());
            if !exact_own_line {
                bail!(
                    "bridge IP {our_ip} conflicts with existing dnsmasq hosts line {}: `{stripped}`",
                    line_no + 1
                );
            }
        }
    }
    Ok(())
}

/// Refuse when any requested bridge IP has an active lease for a MAC other than the exact
/// requested MAC+IP pair.
///
/// Standard dnsmasq lease format: `<expiry> <mac> <ip> <hostname> <client-id>`.
pub(crate) fn check_dnsmasq_lease_conflicts(
    existing_leases: &str,
    requested: &[(String, Ipv4Addr)],
) -> Result<()> {
    let mut leases: BTreeMap<Ipv4Addr, String> = BTreeMap::new();
    for line in existing_leases.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split_whitespace().collect();
        if cols.len() >= 3 {
            if let Ok(ip) = cols[2].parse::<Ipv4Addr>() {
                leases.insert(ip, cols[1].to_ascii_lowercase());
            }
        }
    }
    for (our_mac, our_ip) in requested {
        if let Some(lease_mac) = leases.get(our_ip) {
            let our_mac_lc = our_mac.to_ascii_lowercase();
            let exact_match = !our_mac_lc.is_empty() && *lease_mac == our_mac_lc;
            if !exact_match {
                bail!(
                    "bridge IP {our_ip} is currently leased to MAC {lease_mac} in dnsmasq leases"
                );
            }
        }
    }
    Ok(())
}

/// Return any `(mac, ip)` reservations not yet present in `existing`.
pub(crate) fn missing_dnsmasq_entries(
    existing: &str,
    expected: &[(String, Ipv4Addr)],
) -> Vec<(String, Ipv4Addr)> {
    let mut present = Vec::new();
    for line in existing.lines() {
        let stripped = line.split('#').next().unwrap_or("").trim();
        if stripped.is_empty() {
            continue;
        }
        let mut parts = stripped.split(',').map(str::trim);
        if let (Some(mac), Some(ip_str)) = (parts.next(), parts.next()) {
            if let Ok(ip) = ip_str.parse::<Ipv4Addr>() {
                present.push((mac.to_ascii_lowercase(), ip));
            }
        }
    }
    expected
        .iter()
        .filter(|(mac, ip)| {
            let needle = (mac.to_ascii_lowercase(), *ip);
            !present.contains(&needle)
        })
        .cloned()
        .collect()
}

/// Merge our network's `mac,ip` entries into a shared dnsmasq hosts file idempotently,
/// refusing any IP conflict with foreign reservations.
///
/// Foreign lines outside our managed block are preserved verbatim so co-located test
/// networks on the same VMM host are never disturbed.
pub(crate) fn merge_dnsmasq_hosts(
    existing: &str,
    our_entries: &[(String, Ipv4Addr)],
    network_name: &str,
) -> Result<String> {
    check_dnsmasq_ip_conflicts(existing, our_entries, network_name)?;

    let begin = format!("# outbe-up:{network_name}:begin");
    let end = format!("# outbe-up:{network_name}:end");
    let our_macs: Vec<String> = our_entries
        .iter()
        .map(|(mac, _)| mac.to_ascii_lowercase())
        .collect();

    let mut kept = Vec::new();
    let mut inside_ours = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == begin {
            inside_ours = true;
            continue;
        }
        if trimmed == end {
            inside_ours = false;
            continue;
        }
        if inside_ours {
            continue;
        }
        // Drop any pre-existing unmanaged line for one of our own MACs to avoid duplicate leases.
        let stripped = line.split('#').next().unwrap_or("").trim();
        if let Some((mac, _)) = stripped.split_once(',') {
            if our_macs
                .iter()
                .any(|ours| ours == &mac.trim().to_ascii_lowercase())
            {
                continue;
            }
        }
        kept.push(line.to_owned());
    }

    // Trim trailing blank lines before appending our managed block.
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }

    if !our_entries.is_empty() {
        if !kept.is_empty() {
            kept.push(String::new());
        }
        kept.push(begin);
        for (mac, ip) in our_entries {
            kept.push(dnsmasq_entry(mac, *ip));
        }
        kept.push(end);
    }

    if kept.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("{}\n", kept.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn sample_state() -> VmState {
        VmState {
            version: VM_STATE_VERSION,
            network_name: "vm-devnet".into(),
            image_name: "outbe-vm-dev-aaaaaaaaaaaaaaaa".into(),
            os_image_hash: "a".repeat(64),
            founders: vec![VmFounderState {
                index: 0,
                name: "validator-0".into(),
                vm_name: "vm-devnet-validator-0".into(),
                vm_id: SAMPLE_UUID.into(),
                vmm_host: Ssh {
                    host: "192.0.2.50".into(),
                    user: "ubuntu".into(),
                    port: 22,
                },
                vmm_home: "~/.dstack-vmm".into(),
                bridge: "obr0".into(),
                bridge_ip: Ipv4Addr::new(10, 77, 0, 10),
                mac: "52:54:00:aa:bb:01".into(),
                dhcp_hosts_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.hosts"),
                dhcp_leases_file: PathBuf::from("/var/lib/misc/dnsmasq.outbe-mvp.leases"),
                dhcp_pid_file: PathBuf::from("/run/outbe-mvp-dnsmasq.pid"),
                phase: VmFounderPhase::Ready,
            }],
        }
    }

    #[test]
    fn vm_state_round_trips_and_rejects_missing_or_non_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let err = VmState::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");

        let state = sample_state();
        state.save(dir.path()).unwrap();
        let loaded = VmState::load(dir.path()).unwrap();
        assert_eq!(loaded, state);

        // Tamper vm_id with a shell-injection payload and confirm VmState::load refuses it.
        let mut tampered = state;
        tampered.founders[0].vm_id = "x'; printf REVIEW_INJECTION_OK >&2; fi; #".into();
        fs::write(
            VmState::path(dir.path()),
            serde_json::to_vec_pretty(&tampered).unwrap(),
        )
        .unwrap();
        let err = VmState::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains("invalid vm_id"), "{err}");
    }

    #[test]
    fn merge_dnsmasq_hosts_preserves_foreign_entries_and_is_idempotent() {
        let foreign = "\
# existing other test network
52:54:00:de:ad:01,10.77.0.2
52:54:00:de:ad:02,10.77.0.3
";
        let ours = vec![
            ("52:54:00:AA:BB:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10)),
            ("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11)),
        ];

        let merged = merge_dnsmasq_hosts(foreign, &ours, "vm-devnet").unwrap();
        assert!(merged.contains("52:54:00:de:ad:01,10.77.0.2"));
        assert!(merged.contains("52:54:00:de:ad:02,10.77.0.3"));
        assert!(merged.contains("52:54:00:aa:bb:01,10.77.0.10"));
        assert!(merged.contains("52:54:00:aa:bb:02,10.77.0.11"));
        assert!(missing_dnsmasq_entries(&merged, &ours).is_empty());

        // Re-running with the same entries is byte-identical.
        let merged_again = merge_dnsmasq_hosts(&merged, &ours, "vm-devnet").unwrap();
        assert_eq!(merged_again, merged);

        // Updating our entries replaces only our managed block and keeps foreign lines.
        let updated = vec![("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 20))];
        let merged_updated = merge_dnsmasq_hosts(&merged, &updated, "vm-devnet").unwrap();
        assert!(merged_updated.contains("52:54:00:de:ad:01,10.77.0.2"));
        assert!(merged_updated.contains("52:54:00:aa:bb:01,10.77.0.20"));
        assert!(!merged_updated.contains("52:54:00:aa:bb:02,10.77.0.11"));
    }

    #[test]
    fn refuses_bridge_ip_reserved_to_foreign_mac_or_active_lease() {
        let foreign_hosts = "\
52:54:00:de:ad:01,10.77.0.11
52:54:00:de:ad:02,set:tag0,10.77.0.13
52:54:00:de:ad:03,10.77.0.14,vm-other,infinite
";
        let ours = vec![("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11))];
        let err = merge_dnsmasq_hosts(foreign_hosts, &ours, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.11"), "{err}");

        // Non-2-field dnsmasq grammar (`mac,set:tag,ip` and `mac,ip,hostname,lease`) fails closed.
        let tagged = vec![("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 13))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &tagged, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.13"), "{err}");

        let four_col = vec![("52:54:00:aa:bb:04".to_owned(), Ipv4Addr::new(10, 77, 0, 14))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &four_col, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.14"), "{err}");

        // Pre-CreateVm check (empty MAC) also refuses an occupied IP.
        let pre_create = vec![(String::new(), Ipv4Addr::new(10, 77, 0, 11))];
        let err = check_dnsmasq_ip_conflicts(foreign_hosts, &pre_create, "vm-devnet").unwrap_err();
        assert!(err.to_string().contains("10.77.0.11"), "{err}");

        // Active lease for any MAC other than the exact requested MAC+IP pair is refused.
        let leases = "0 52:54:00:fe:ed:01 10.77.0.12 other-vm *\n";
        let mismatch = vec![("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 12))];
        let err = check_dnsmasq_lease_conflicts(leases, &mismatch).unwrap_err();
        assert!(err.to_string().contains("10.77.0.12"), "{err}");
        assert!(err.to_string().contains("52:54:00:fe:ed:01"), "{err}");

        // Exact requested MAC+IP pair is allowed.
        let exact = vec![("52:54:00:fe:ed:01".to_owned(), Ipv4Addr::new(10, 77, 0, 12))];
        check_dnsmasq_lease_conflicts(leases, &exact).unwrap();
    }

    #[test]
    fn missing_dnsmasq_entries_detects_absent_or_mismatched_ips() {
        let existing = "52:54:00:aa:bb:01,10.77.0.10\n52:54:00:aa:bb:02,10.77.0.99\n";
        let expected = vec![
            ("52:54:00:aa:bb:01".to_owned(), Ipv4Addr::new(10, 77, 0, 10)),
            ("52:54:00:aa:bb:02".to_owned(), Ipv4Addr::new(10, 77, 0, 11)),
            ("52:54:00:aa:bb:03".to_owned(), Ipv4Addr::new(10, 77, 0, 12)),
        ];
        let missing = missing_dnsmasq_entries(existing, &expected);
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[0].1, Ipv4Addr::new(10, 77, 0, 11));
        assert_eq!(missing[1].1, Ipv4Addr::new(10, 77, 0, 12));
    }
}
