//! Owned hardware enclave candidates and the runtime selected after promotion.
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eyre::{ensure, eyre, Result, WrapErr};
use sha2::{Digest, Sha256};

use super::Localnet;
use crate::internal::proc::{self, EnclaveGuard, EnclaveSpec, SealSpec, TestSgxMeasurement};

#[derive(Clone, Debug)]
pub(super) struct EnclaveRuntimeProfile {
    pub binary: PathBuf,
    pub signing_key: PathBuf,
    pub tee_dir: PathBuf,
    pub port: u16,
    pub container: String,
}

pub(crate) struct HardwareEnclaveCandidate {
    pub index: usize,
    pub round: u32,
    pub measurement: TestSgxMeasurement,
    profile: EnclaveRuntimeProfile,
    guard: EnclaveGuard,
}

impl HardwareEnclaveCandidate {
    pub fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.profile.port)
    }

    pub fn tee_dir(&self) -> &Path {
        &self.profile.tee_dir
    }
}

impl Localnet {
    pub(crate) fn active_enclave_seal_directory(&self, index: usize) -> Result<PathBuf> {
        Ok(self.active_enclave_profile(index)?.tee_dir)
    }

    pub(super) fn active_enclave_profile(&self, index: usize) -> Result<EnclaveRuntimeProfile> {
        if let Some(profile) = self.enclave_runtime_profiles.get(&index) {
            return Ok(profile.clone());
        }
        Ok(EnclaveRuntimeProfile {
            binary: if self.cfg.tee_mode.uses_mock_binary() {
                self.cfg.bin_mock.clone()
            } else {
                self.real_enclave_bin()?
            },
            signing_key: self.cfg.dir.join("test-sgx-signing-key.pem"),
            tee_dir: self.cfg.validator_dir(index).join("tee"),
            port: self.cfg.tee_port(index),
            container: self.cfg.tee_container(index),
        })
    }

    /// Keep A running while creating B with independent identity and storage.
    /// The supplied binary must be an actual separately built release artifact.
    pub(crate) fn start_hardware_upgrade_candidate(
        &mut self,
        index: usize,
        round: u32,
        binary: &Path,
    ) -> Result<HardwareEnclaveCandidate> {
        ensure!(
            self.cfg.tee_mode.passes_sgx_devices(),
            "upgrade acceptance requires hardware SGX"
        );
        ensure!(
            index < self.committee_size() && round > 0,
            "invalid candidate slot or round"
        );
        self.ensure_enclave_image_once()?;
        let active = self.active_enclave_profile(index)?;
        let image = self
            .enclave_image_id
            .as_ref()
            .ok_or_else(|| eyre!("missing SGX image"))?;
        let descriptor = self.cfg.dir.join("network-descriptor-v1.bin");
        let dcap = self.cfg.tee_mode == crate::env::TeeMode::Real;
        let predecessor = proc::inspect_test_sgx_measurement(
            &self.cfg.repo,
            &active.binary,
            &active.signing_key,
            &descriptor,
            image,
            self.cfg.sudo,
            dcap,
        )?;
        let root = self
            .cfg
            .validator_dir(index)
            .join(format!("enclave-upgrade-{round}"));
        ensure!(
            !root.exists(),
            "candidate directory already exists; resume its owned candidate"
        );
        fs::create_dir(&root)?;
        let candidate_binary = root.join("outbe-tee-enclave");
        fs::copy(binary, &candidate_binary).wrap_err("copy exact successor release artifact")?;
        let binary_hash = hex::encode(Sha256::digest(fs::read(&candidate_binary)?));
        ensure!(
            binary_hash != hex::encode(Sha256::digest(fs::read(&active.binary)?)),
            "successor ELF is identical to predecessor"
        );
        let signing_key = root.join("signing-key.pem");
        let key_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&signing_key)?;
        let output = Command::new("openssl")
            .args(["genrsa", "-3", "3072"])
            .stdout(Stdio::from(key_file))
            .stderr(Stdio::piped())
            .output()?;
        ensure!(
            output.status.success(),
            "generate independent operator SGX key: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let measurement = proc::inspect_test_sgx_measurement(
            &self.cfg.repo,
            &candidate_binary,
            &signing_key,
            &descriptor,
            image,
            self.cfg.sudo,
            dcap,
        )?;
        ensure!(
            measurement.mrenclave != predecessor.mrenclave,
            "successor has unchanged MRENCLAVE"
        );
        ensure!(
            measurement.mrsigner != predecessor.mrsigner,
            "successor reused predecessor signer"
        );
        let slot = 10_000usize
            .checked_add(
                (round as usize)
                    .checked_mul(self.committee_size())
                    .ok_or_else(|| eyre!("round overflow"))?,
            )
            .and_then(|slot| slot.checked_add(index))
            .ok_or_else(|| eyre!("candidate slot overflow"))?;
        let profile = EnclaveRuntimeProfile {
            binary: candidate_binary,
            signing_key,
            tee_dir: root.join("tee"),
            port: self.cfg.tee_port(slot),
            container: format!("{}-upgrade-{round}", self.cfg.tee_container(index)),
        };
        fs::write(
            root.join("artifact.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "hardware_sgx": true, "round": round, "validator": index,
                "binary_sha256": binary_hash, "mrenclave": measurement.mrenclave,
                "mrsigner": measurement.mrsigner, "predecessor_mrenclave": predecessor.mrenclave,
                "predecessor_mrsigner": predecessor.mrsigner,
            }))?,
        )?;
        let guard = self.spawn_upgrade_enclave(&profile, root.join("enclave.log"))?;
        Ok(HardwareEnclaveCandidate {
            index,
            round,
            measurement,
            profile,
            guard,
        })
    }

    fn spawn_upgrade_enclave(
        &self,
        profile: &EnclaveRuntimeProfile,
        log_path: PathBuf,
    ) -> Result<EnclaveGuard> {
        proc::spawn_enclave_ready(
            EnclaveSpec {
                name: profile.container.clone(),
                tee_port: profile.port,
                enclave_bin: profile.binary.clone(),
                signing_key: profile.signing_key.clone(),
                network_descriptor: Some(self.cfg.dir.join("network-descriptor-v1.bin")),
                dev_network_binding: None,
                launch: self.enclave_launch()?,
                sudo: self.cfg.sudo,
                pass_sgx_devices: true,
                remote_attestation: if self.cfg.tee_mode == crate::env::TeeMode::Real {
                    proc::TestRemoteAttestation::Dcap
                } else {
                    proc::TestRemoteAttestation::None
                },
                dkg_seed: None,
                seal: Some(SealSpec {
                    tee_dir: profile.tee_dir.clone(),
                    chain_id_hex: self.chain_id_hex()?,
                }),
                log_path,
                debug: self.cfg.debug,
            },
            self.enclave_startup_deadline(20),
        )
    }

    pub(crate) fn restart_hardware_upgrade_candidate(
        &self,
        candidate: &mut HardwareEnclaveCandidate,
    ) -> Result<()> {
        candidate.guard.stop_and_reap()?;
        candidate.guard = self.spawn_upgrade_enclave(
            &candidate.profile,
            candidate
                .profile
                .tee_dir
                .parent()
                .unwrap()
                .join("enclave.log"),
        )?;
        Ok(())
    }

    pub(crate) fn run_candidate_upgrade_cli(
        &self,
        candidate: &HardwareEnclaveCandidate,
        donor: usize,
        command: &str,
        extra: &[String],
    ) -> Result<String> {
        ensure!(
            donor < self.committee_size() && donor != candidate.index,
            "candidate requires another live donor"
        );
        let directory = self.cfg.validator_dir(candidate.index);
        let mut args = vec![
            "--rpc-url".to_owned(),
            format!("http://127.0.0.1:{}", self.cfg.http_port(donor)),
            "--private-key".to_owned(),
            proc::read_evm_key(&directory)?,
            "tee".to_owned(),
            command.to_owned(),
            "--node-data-dir".to_owned(),
            directory.join("data").display().to_string(),
        ];
        if matches!(
            command,
            "upgrade-prepare" | "upgrade-provision" | "upgrade-submit"
        ) {
            args.extend([
                "--candidate-enclave-socket".to_owned(),
                candidate.endpoint(),
                "--reth-p2p-secret-key".to_owned(),
                directory.join("reth-p2p-secret.hex").display().to_string(),
            ]);
        }
        args.extend_from_slice(extra);
        self.sh().cli_required(args)
    }

    /// Only select B after the production CLI durably promoted its journal.
    pub(crate) fn select_promoted_hardware_candidate(
        &mut self,
        candidate: HardwareEnclaveCandidate,
        donor: usize,
    ) -> Result<()> {
        let snapshot: serde_json::Value = serde_json::from_str(&self.run_candidate_upgrade_cli(
            &candidate,
            donor,
            "upgrade-status",
            &[],
        )?)?;
        ensure!(
            snapshot
                .pointer("/lifecycle/state")
                .and_then(|v| v.as_str())
                == Some("promoted"),
            "candidate journal has not been promoted"
        );
        ensure!(
            !self.validator_running(candidate.index),
            "stop predecessor node before replacing its enclave"
        );
        let context_dir = snapshot
            .pointer("/lifecycle/context/candidateTeeDir")
            .and_then(|value| value.as_str())
            .ok_or_else(|| eyre!("promoted context has no candidate directory"))?;
        ensure!(
            Path::new(context_dir).canonicalize()? == candidate.profile.tee_dir.canonicalize()?,
            "promoted journal belongs to another candidate"
        );
        let index = candidate.index;
        let endpoint = candidate.endpoint();
        let original = self
            .validator_argv
            .get(&index)
            .ok_or_else(|| eyre!("validator has no captured argv"))?;
        let argv = with_enclave_endpoint(original, &endpoint)?;
        let recovery = self
            .validator_recovery_original_argv
            .get(&index)
            .map(|original| with_enclave_endpoint(original, &endpoint))
            .transpose()?;
        if let Some(mut old) = self.enclaves.remove(&index) {
            old.stop_and_reap()?;
        }
        self.validator_argv.insert(index, argv);
        if let Some(recovery) = recovery {
            self.validator_recovery_original_argv
                .insert(index, recovery);
        }
        self.enclave_runtime_profiles
            .insert(index, candidate.profile);
        self.enclaves.insert(index, candidate.guard);
        Ok(())
    }
}

fn with_enclave_endpoint(argv: &[String], endpoint: &str) -> Result<Vec<String>> {
    let positions = argv
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value == "--tee-enclave-socket").then_some(index))
        .collect::<Vec<_>>();
    ensure!(
        positions.len() == 1,
        "validator must have one explicit enclave endpoint"
    );
    let position = positions[0] + 1;
    ensure!(
        argv.get(position)
            .is_some_and(|value| !value.starts_with("--")),
        "validator endpoint value missing"
    );
    let mut updated = argv.to_vec();
    updated[position] = endpoint.to_owned();
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_switch_preserves_authority_and_recovery_arguments() {
        let original = [
            "node",
            "--tee-enclave-socket",
            "127.0.0.1:1",
            "--upstream",
            "http://donor",
            "--datadir",
            "/owned/data",
        ]
        .map(str::to_owned);
        let updated = with_enclave_endpoint(&original, "127.0.0.1:2").unwrap();
        assert_eq!(&updated[..2], &original[..2]);
        assert_eq!(updated[2], "127.0.0.1:2");
        assert_eq!(&updated[3..], &original[3..]);
        assert_eq!(original[2], "127.0.0.1:1");
        for invalid in [
            vec!["node"],
            vec!["--tee-enclave-socket"],
            vec!["--tee-enclave-socket", "--datadir"],
            vec!["--tee-enclave-socket", "a", "--tee-enclave-socket", "b"],
        ] {
            assert!(with_enclave_endpoint(
                &invalid.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                "new"
            )
            .is_err());
        }
    }
}
