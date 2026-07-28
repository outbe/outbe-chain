//! Isolated binary build and per-validator launch controls for adversarial E2E.
//!
//! The source patch is never applied to the developer checkout. It is checked
//! and applied in a detached worktree at the exact tested `HEAD`, then built
//! offline into a content-addressed target directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::Address;
use eyre::{bail, eyre, Result, WrapErr as _};
use sha2::{Digest as _, Sha256};

use super::Localnet;

const PATCH_RELATIVE_PATH: &str = "crates/testing/e2e-harness/patches/omit-active-boundary.patch";
const ADVERSARY_ENV: &str = "OUTBE_E2E_ADVERSARY_OMIT_ACTIVE";

impl Localnet {
    /// Build (or reuse) the test-only boundary variant from the exact current
    /// revision. The returned executable is outside the detached worktree, so
    /// removing the worktree cannot invalidate the cache.
    pub fn build_omit_active_boundary_binary(&self) -> Result<PathBuf> {
        fs::create_dir_all(&self.cfg.dir)?;
        let patch_path = self.cfg.repo.join(PATCH_RELATIVE_PATH);
        let patch = fs::read(&patch_path)
            .wrap_err_with(|| format!("read adversarial patch {}", patch_path.display()))?;
        let head = command_stdout(
            Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["rev-parse", "HEAD"]),
            "resolve source revision for adversarial build",
        )?;
        let mut digest = Sha256::new();
        digest.update(head.trim().as_bytes());
        digest.update(&patch);
        let cache_key = hex::encode(digest.finalize());
        let target = self.cfg.repo.join("target/e2e-adversarial").join(cache_key);
        let binary = target.join("debug/outbe-chain");
        if binary.is_file() {
            return Ok(binary);
        }

        let worktree = self.cfg.dir.join("omit-active-boundary-worktree");
        self.remove_detached_worktree(&worktree)?;
        self.run_setup(
            Command::new("git")
                .current_dir(&self.cfg.repo)
                .args(["worktree", "add", "--detach"])
                .arg(&worktree)
                .arg(head.trim()),
            "create adversarial detached worktree",
        )?;

        let build_result = (|| -> Result<()> {
            self.run_setup(
                Command::new("git")
                    .current_dir(&worktree)
                    .args(["apply", "--check"])
                    .arg(&patch_path),
                "check adversarial boundary patch",
            )?;
            self.run_setup(
                Command::new("git")
                    .current_dir(&worktree)
                    .arg("apply")
                    .arg(&patch_path),
                "apply adversarial boundary patch",
            )?;
            self.run_setup(
                Command::new("cargo")
                    .current_dir(&worktree)
                    .args([
                        "build",
                        "--offline",
                        "-p",
                        "outbe-chain",
                        "--bin",
                        "outbe-chain",
                    ])
                    .arg("--target-dir")
                    .arg(&target),
                "build adversarial outbe-chain offline",
            )
        })();
        let cleanup_result = self.remove_detached_worktree(&worktree);
        build_result?;
        cleanup_result?;
        if !binary.is_file() {
            bail!(
                "adversarial build completed without binary {}",
                binary.display()
            );
        }
        Ok(binary)
    }

    /// Relaunch one validator with the isolated binary and a single omitted
    /// active address. No other committee process receives the override.
    pub fn restart_validator_with_omit_active_boundary(
        &mut self,
        validator_index: usize,
        omitted: Address,
    ) -> Result<()> {
        let binary = self.build_omit_active_boundary_binary()?;
        self.validator_binary_overrides
            .insert(validator_index, binary);
        self.validator_environment_overrides.insert(
            validator_index,
            vec![(ADVERSARY_ENV.to_owned(), format!("{omitted:#x}"))],
        );
        self.kill_validator(validator_index)?;
        self.restart()
    }

    /// Restore one validator to the normal executable and environment, keeping
    /// its existing database, key material, and enclave seal.
    pub fn restart_validator_with_normal_binary(&mut self, validator_index: usize) -> Result<()> {
        self.validator_binary_overrides.remove(&validator_index);
        self.validator_environment_overrides
            .remove(&validator_index);
        self.kill_validator(validator_index)?;
        self.restart()
    }

    fn remove_detached_worktree(&self, worktree: &Path) -> Result<()> {
        if !worktree.exists() {
            return Ok(());
        }
        let output = Command::new("git")
            .current_dir(&self.cfg.repo)
            .args(["worktree", "remove", "--force"])
            .arg(worktree)
            .output()
            .wrap_err("remove adversarial detached worktree")?;
        if !output.status.success() {
            return Err(eyre!(
                "remove adversarial detached worktree failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }
}

fn command_stdout(command: &mut Command, label: &str) -> Result<String> {
    let output = command.output().wrap_err_with(|| format!("run {label}"))?;
    if !output.status.success() {
        return Err(eyre!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).wrap_err_with(|| format!("decode output from {label}"))
}
