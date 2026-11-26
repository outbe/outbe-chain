//! Journaled per-VM state record and the identity checks for its fields.

use std::{
    fs,
    io::Read,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::vmm::validate_vm_id;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalVmPhase {
    /// Durable create intent written BEFORE calling `CreateVm`; carries `create_token` for
    /// ownership-safe recovery if the process crashes or the `CreateVm` response is lost.
    Creating,
    /// `CreateVm` succeeded and `vm_id` is journaled; MAC/disk not yet staged.
    Created,
    /// `GetInfo` MAC recorded; data disk substitution pending.
    MacRecorded,
    /// Data disk converted and staged to `hda.img`.
    DiskStaged,
    /// Provisioning completed; VM is ready to start.
    #[default]
    Ready,
    /// Removal requested; waiting for asynchronous VMM removal before deleting state.
    Removing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub timestamp_unix_secs: u64,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalVmState {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub vmm_socket: PathBuf,
    pub vmm_home: PathBuf,
    pub image_name: String,
    pub os_image_hash: String,
    pub disk_source: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_source_sha256: Option<String>,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub mac: String,
    pub app_id: String,
    pub no_tee: bool,
    pub phase: LocalVmPhase,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

impl LocalVmState {
    pub(crate) fn record_step(&mut self, step: impl Into<String>, detail: Option<String>) {
        let timestamp_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.journal.push(JournalEntry {
            timestamp_unix_secs,
            step: step.into(),
            detail,
        });
    }

    /// True when a `Creating` intent has no VMM ID yet (the `CreateVm` response was never
    /// journaled).
    pub(super) fn is_unresolved_creating(&self) -> bool {
        self.phase == LocalVmPhase::Creating && self.vm_id.is_empty()
    }

    pub(super) fn validate_identity(&self, path: &Path) -> Result<()> {
        validate_vm_name(&self.name)?;
        if let Some(tag) = &self.owner_tag {
            validate_owner_tag(tag).wrap_err_with(|| {
                format!("VM state file {} has invalid owner_tag", path.display())
            })?;
        }
        if self.phase != LocalVmPhase::Creating {
            return validate_vm_id(&self.vm_id)
                .wrap_err_with(|| format!("VM state file {} has invalid vm_id", path.display()));
        }
        if !self.vm_id.is_empty() {
            validate_vm_id(&self.vm_id)
                .wrap_err_with(|| format!("VM state file {} has invalid vm_id", path.display()))?;
        }
        ensure!(
            self.create_token
                .as_deref()
                .is_some_and(is_valid_create_token),
            "VM state file {} in `creating` phase is missing a valid create_token",
            path.display()
        );
        Ok(())
    }
}

fn is_valid_create_token(token: &str) -> bool {
    token.len() >= 16 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Collects human-readable `field (recorded vs actual)` differences between two records.
#[derive(Default)]
pub(super) struct Mismatches(Vec<String>);

impl Mismatches {
    /// Record `describe()` when `differs` is true.
    pub(super) fn check(&mut self, differs: bool, describe: impl FnOnce() -> String) {
        if differs {
            self.0.push(describe());
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn join(&self) -> String {
        self.0.join(", ")
    }
}

pub(super) fn generate_create_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .wrap_err("open /dev/urandom for VM create token")?
        .read_exact(&mut bytes)
        .wrap_err("read random bytes from /dev/urandom")?;
    Ok(hex::encode(bytes))
}

/// Validate an optional `--expect-vm-id` value before any lock or RPC.
pub(super) fn validate_expected_vm_id(expect_vm_id: Option<&str>) -> Result<()> {
    if let Some(expected) = expect_vm_id {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    Ok(())
}

/// Check an optional `--expect-vm-id` against the journaled VM ID.
pub(super) fn verify_expected_vm_id(
    state: &LocalVmState,
    expect_vm_id: Option<&str>,
) -> Result<()> {
    let Some(expected) = expect_vm_id else {
        return Ok(());
    };
    validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    ensure!(
        !state.vm_id.is_empty() && state.vm_id == expected,
        "vm_id_mismatch for VM `{}`: expected `{expected}`, recorded `{}`",
        state.name,
        if state.vm_id.is_empty() {
            "<none (creating)>"
        } else {
            &state.vm_id
        }
    );
    Ok(())
}

/// Validate a VM name used both as the VMM name and as the `<name>.json` state file stem.
pub fn validate_vm_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 63,
        "VM name must be 1..=63 characters, got '{name}'"
    );
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "VM name must start with an ASCII letter or digit, got '{name}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "VM name must use only ASCII letters, digits, `-`, or `_`, got '{name}'"
    );
    Ok(())
}

/// Validate an optional VM ownership tag (e.g. `outbe-net:<network>:<genesis8>:<member>`).
pub fn validate_owner_tag(tag: &str) -> Result<()> {
    ensure!(
        !tag.is_empty() && tag.len() <= 128,
        "owner tag must be 1..=128 characters, got '{tag}'"
    );
    let mut chars = tag.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "owner tag must start with an ASCII letter or digit, got '{tag}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')),
        "owner tag must use only ASCII letters, digits, `-`, `_`, `:`, or `.`, got '{tag}'"
    );
    Ok(())
}

pub fn validate_flavor(flavor: &str) -> Result<()> {
    ensure!(
        matches!(flavor, "dev" | "prod"),
        "VM flavor must be `dev` or `prod`, got `{flavor}`"
    );
    Ok(())
}
