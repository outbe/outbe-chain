//! Replacement chain binary for protocol-upgrade restarts.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use eyre::{bail, Result};

use crate::world::localnet::Localnet;

impl Localnet {
    /// Replace the node executable and relaunch the whole committee with the
    /// same datadirs, keys, ports, and enclave seals. This models the normal
    /// operator recovery after an old binary stops at a protocol activation.
    pub fn restart_committee_with_upgraded_binary(&mut self, version: &str) -> Result<()> {
        let upgraded = match self.cfg.bin_chain_upgraded.clone() {
            Some(path) => path,
            None => self.build_upgraded_binary(version)?,
        };
        if !upgraded.is_file() {
            bail!(
                "upgraded chain binary does not exist: {}",
                upgraded.display()
            );
        }
        self.cfg.bin_chain = upgraded;
        self.restart_committee_and_enclaves()
    }

    /// Build the replacement executable from the exact source revision under
    /// test, without modifying the developer's checkout. The temporary detached
    /// worktree receives only the workspace package-version bump. Cargo writes to
    /// a stable version-specific target so repeated update scenarios reuse heavy
    /// third-party artifacts.
    fn build_upgraded_binary(&self, version: &str) -> Result<PathBuf> {
        let cargo_version = cargo_package_version(version)?;
        let worktree = self.cfg.dir.join("upgrade-worktree");
        let target = self
            .cfg
            .repo
            .join("target/e2e-upgrades")
            .join(version.replace('.', "-"));
        let binary = target.join("release/outbe-chain");

        if worktree.exists() {
            let _ = Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["worktree", "remove", "--force"])
                .arg(&worktree)
                .output();
        }
        self.run_setup(
            Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["worktree", "add", "--detach"])
                .arg(&worktree)
                .arg("HEAD"),
            "create protocol-upgrade worktree",
        )?;

        let build_result = (|| -> Result<()> {
            let manifest = worktree.join("Cargo.toml");
            let source = fs::read_to_string(&manifest)?;
            fs::write(
                &manifest,
                rewrite_workspace_version(&source, &cargo_version)?,
            )?;

            self.run_setup(
                Command::new("cargo")
                    .current_dir(&worktree)
                    .args(replacement_chain_build_args())
                    .arg("--target-dir")
                    .arg(&target),
                &format!("build replacement outbe-chain v{version}"),
            )
        })();

        let cleanup = Command::new("git")
            .current_dir(&self.cfg.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&worktree)
            .output();
        build_result?;
        let cleanup = cleanup?;
        if !cleanup.status.success() {
            bail!(
                "remove protocol-upgrade worktree failed: {}",
                String::from_utf8_lossy(&cleanup.stderr)
            );
        }
        Ok(binary)
    }
}

pub(super) fn rewrite_workspace_version(manifest: &str, version: &str) -> Result<String> {
    let section = manifest
        .find("[workspace.package]")
        .ok_or_else(|| eyre::eyre!("root Cargo.toml has no [workspace.package] section"))?;
    let section_end = manifest[section + 1..]
        .find("\n[")
        .map_or(manifest.len(), |offset| section + 1 + offset);
    let version_start = manifest[section..section_end]
        .find("\nversion = \"")
        .map(|offset| section + offset + "\nversion = \"".len())
        .ok_or_else(|| eyre::eyre!("[workspace.package] has no version field"))?;
    let version_end = manifest[version_start..]
        .find('"')
        .map(|offset| version_start + offset)
        .ok_or_else(|| eyre::eyre!("workspace package version is not quoted"))?;

    let mut rewritten = manifest.to_owned();
    rewritten.replace_range(version_start..version_end, version);
    Ok(rewritten)
}

pub(super) fn cargo_package_version(protocol_version: &str) -> Result<String> {
    let mut components = protocol_version.split('.');
    let major = components.next().unwrap_or_default();
    let minor = components.next().unwrap_or_default();
    if major.is_empty() || minor.is_empty() || components.next().is_some() {
        bail!("protocol version must have major.minor form, got {protocol_version}");
    }
    Ok(format!("{major}.{minor}.0"))
}

pub(super) fn replacement_chain_build_args() -> [&'static str; 9] {
    [
        "build",
        "--offline",
        "--release",
        "-p",
        "outbe-chain",
        "--bin",
        "outbe-chain",
        "--features",
        "e2e-test,test-protocol-overrides",
    ]
}
