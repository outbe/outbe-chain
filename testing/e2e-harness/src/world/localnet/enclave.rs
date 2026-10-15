//! Scenario enclave image identity, execution profile, and startup budgets.

use std::process::Command;

use eyre::{bail, Result};

use super::Localnet;
use crate::internal::proc::{self, args, DockerImageId};

impl Localnet {
    pub(super) fn enclave_startup_deadline(&self, nonhardware_seconds: u64) -> std::time::Instant {
        let now = std::time::Instant::now();
        let seconds = if self.cfg.tee_mode.passes_sgx_devices() {
            crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
        } else {
            nonhardware_seconds
        };
        let deadline = now + std::time::Duration::from_secs(seconds);
        // Leave the existing watchdog time for diagnostics and owned teardown.
        self.scenario_deadline.map_or(deadline, |outer| {
            deadline.min(
                outer
                    .checked_sub(std::time::Duration::from_secs(20))
                    .unwrap_or(now),
            )
        })
    }

    pub(super) fn retain_enclave_image_id(&mut self, image_id: DockerImageId) -> Result<()> {
        match &self.enclave_image_id {
            Some(established) if established != &image_id => {
                bail!("Gramine Docker image identity changed during the scenario");
            }
            Some(_) => Ok(()),
            None => {
                self.enclave_image_id = Some(image_id);
                Ok(())
            }
        }
    }

    pub(super) fn ensure_enclave_image_once_with<F>(&mut self, resolve: F) -> Result<()>
    where
        F: FnOnce() -> Result<DockerImageId>,
    {
        if self.enclave_image_id.is_some() {
            return Ok(());
        }
        self.retain_enclave_image_id(resolve()?)
    }

    pub(super) fn ensure_enclave_image_once(&mut self) -> Result<()> {
        // The native profile runs no container, so it must not build, resolve or
        // retain an image identity. `enclave_image_id()` stays `None`, and the
        // evidence records no Gramine image for a run that never used one.
        if self.cfg.tee_mode.runs_native_host_enclave() {
            return Ok(());
        }
        let repo = self.cfg.repo.clone();
        let sudo = self.cfg.sudo;
        let signing_key = self.cfg.dir.join("test-sgx-signing-key.pem");
        self.ensure_enclave_image_once_with(|| {
            proc::ensure_enclave_image(&repo, sudo, &signing_key, None)
        })
    }

    /// The execution profile every enclave in this scenario runs under.
    pub(super) fn enclave_launch(&self) -> Result<proc::EnclaveLaunch> {
        if self.cfg.tee_mode.runs_native_host_enclave() {
            return Ok(proc::EnclaveLaunch::NativeHost);
        }
        Ok(proc::EnclaveLaunch::Gramine {
            image_id: self
                .enclave_image_id
                .clone()
                .ok_or_else(|| eyre::eyre!("Gramine Docker image identity was not resolved"))?,
        })
    }

    pub(crate) fn enclave_image_id(&self) -> Option<&str> {
        self.enclave_image_id.as_ref().map(DockerImageId::as_str)
    }

    /// The owned foreground enclave launcher must remain observable and live.
    pub(crate) fn live_enclave_pid(&mut self, index: usize) -> Result<u32> {
        let enclave = self
            .enclaves
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned enclave"))?;
        eyre::ensure!(
            enclave.exit_status()?.is_none(),
            "validator-{index} enclave exited"
        );
        Ok(enclave.pid())
    }

    /// Co-located hardware enclaves have an E2E-only startup allowance. The
    /// node's production/testnet default remains unchanged. The node's operator
    /// must choose that value for the deployment topology.
    pub(super) fn extend_real_sgx_startup_timeout(&self, args: &mut Vec<String>) {
        if self.cfg.tee_mode.passes_sgx_devices() {
            args.extend(args![
                "--tee-bootstrap-timeout-secs",
                crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS
            ]);
        }
    }
}

/// Co-located real enclaves share one physical EPC. A request can therefore
/// complete in the enclave after the production-oriented 30-second host timeout.
/// The enclave then observes a broken pipe even though it produced and sealed the
/// result. Widen only the hardware E2E lane. Production/testnet retain their
/// explicit operator-selected/default deadline.
pub(super) fn extend_real_sgx_process_environment(mode: crate::env::TeeMode, cmd: &mut Command) {
    if mode.passes_sgx_devices() {
        cmd.env(
            "OUTBE_TEE_IO_TIMEOUT_SECS",
            crate::env::CO_LOCATED_HARDWARE_SGX_TIMEOUT_SECS.to_string(),
        );
    }
}
