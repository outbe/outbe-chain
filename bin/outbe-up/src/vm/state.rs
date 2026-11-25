//! Persisted multi-founder VM deployment state (`vm-state.json`).

use std::{
    collections::BTreeSet,
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use outbe_operator_vm::vmm::validate_vm_id;
use serde::{Deserialize, Serialize};

use crate::network::{
    config::{default_vm_dhcp_leases_file, Ssh},
    remote::NetworkArtifacts,
};

pub(crate) const VM_STATE_VERSION: u32 = 1;
pub(crate) const VM_STATE_FILENAME: &str = "vm-state.json";

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
}
