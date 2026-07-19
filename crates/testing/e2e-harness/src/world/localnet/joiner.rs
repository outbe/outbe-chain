//! A validator that joins a running localnet: provision (keygen, fund, register,
//! enclave, `tee join`) and launch at index = committee size. Ported
//! `e2e_provision_joiner` / `e2e_launch_joiner`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::{hex, Bytes};
use eyre::{eyre, Result};

use crate::env::TeeMode;
use crate::internal::{
    addresses,
    eth::{self, IValidatorSet},
    proc::{
        self, args, attach_log, first_hex, random_hex_32, read_evm_key, read_trimmed, wait_tcp,
        SealSpec,
    },
    shell::Sh,
};

use super::Localnet;

fn verifier_material_paths(run_dir: &Path) -> (PathBuf, PathBuf) {
    let active_keys = run_dir.join("validator-0/data/keys");
    (
        active_keys.join("dkg_polynomial.hex"),
        active_keys.join("dkg_output.hex"),
    )
}

impl Localnet {
    /// Provision a joiner: keygen, fund, register, p2p, enclave, `tee join`
    /// (port of `e2e_provision_joiner`). Leaves keys under `validator-<index>/`.
    pub fn provision_joiner(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(&vd)?;
        let signing_key = vd.join("signing-key.hex").display().to_string();

        // Fresh hybrid key material for the joiner.
        self.keygen(&["hybrid", "--output-dir", &vd.display().to_string()])?;
        let bls = first_hex(&self.keygen(&["show-pubkey", "--key", &signing_key])?, 96)
            .ok_or_else(|| eyre!("no BLS pubkey from keygen"))?;
        let key = read_evm_key(&vd)?;
        let addr = eth::address_of(&key).ok_or_else(|| eyre!("bad joiner evm key"))?;
        let sig = first_hex(
            &self.keygen(&[
                "sign-registration",
                "--key",
                &signing_key,
                "--validator-address",
                &format!("{addr:#x}"),
            ])?,
            120,
        )
        .ok_or_else(|| eyre!("no registration signature from keygen"))?;
        fs::write(vd.join("reth-p2p-secret.hex"), random_hex_32()?)?;

        // Fund from validator-0, prove that an unrelated EOA cannot register
        // this validator identity, then self-register and publish the P2P
        // address. The rejected call uses the joiner's otherwise-valid BLS
        // binding, isolating caller authorization from proof validation.
        let v0 = read_evm_key(&self.cfg.validator_dir(0))?;
        eth::send_value(&self.cfg.rpc0, addr, &v0, eth::coen(2000))?;
        let registration = IValidatorSet::registerValidatorCall {
            v: addr,
            pubkey: Bytes::from(hex::decode(&bls)?),
            sig: Bytes::from(hex::decode(&sig)?),
        };
        let unrelated = read_evm_key(&self.cfg.validator_dir(1))?;
        let unauthorized = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &unrelated,
            &registration,
            None,
        );
        match unauthorized {
            Err(error) if error.to_string().contains("unauthorized") => {}
            Ok(tx) if eth::receipt_success(&self.cfg.rpc0, &tx) == Some(false) => {}
            Err(error) => {
                return Err(eyre!(
                    "unexpected unrelated-EOA registration error for {addr:#x}: {error}"
                ));
            }
            Ok(tx) => {
                return Err(eyre!(
                    "unrelated EOA unexpectedly registered joiner {addr:#x}: {tx}"
                ));
            }
        }
        let register_tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &key,
            &registration,
            None,
        )?;
        if eth::receipt_success(&self.cfg.rpc0, &register_tx) != Some(true) {
            return Err(eyre!("joiner registration failed: {register_tx}"));
        }
        let p2p_tx = eth::send_call(
            &self.cfg.rpc0,
            addresses::VS_ADDR,
            &key,
            &IValidatorSet::setP2pAddressCall {
                v: addr,
                kind: 1,
                addr: Bytes::from(hex::decode("00047f00000176c4")?),
            },
            None,
        )?;
        if eth::receipt_success(&self.cfg.rpc0, &p2p_tx) != Some(true) {
            return Err(eyre!("joiner P2P registration failed: {p2p_tx}"));
        }

        // Enclave container (owned foreground, no `-d`), then `tee join` once its
        // socket is up.
        self.start_joiner_enclave(index)?;
        let port = self.cfg.tee_port(index);
        let sock = format!("127.0.0.1:{port}");
        let _ = Sh::new(&self.cfg).cli([
            "tee",
            "join",
            "--enclave-socket",
            &sock,
            "--rpc-url",
            self.cfg.rpc0.as_str(),
            "--private-key",
            &key,
            "--timeout-secs",
            "60",
        ]);
        Ok(())
    }

    /// Restart a joiner's enclave from its existing writable TEE directory.
    /// On hardware SGX this exercises EGETKEY unsealing rather than provisioning
    /// fresh join material.
    pub fn restart_joiner_enclave(&mut self, index: usize) -> Result<()> {
        self.enclaves.remove(&index);
        self.start_joiner_enclave(index)
    }

    fn start_joiner_enclave(&mut self, index: usize) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        let port = self.cfg.tee_port(index);
        proc::ensure_enclave_image(&self.cfg.repo, self.cfg.sudo)?;
        let mock = matches!(self.cfg.tee_mode, TeeMode::Mock);
        let enclave_bin = if mock {
            self.cfg.bin_mock.clone()
        } else {
            self.real_enclave_bin()?
        };
        let guard = proc::spawn_enclave(proc::EnclaveSpec {
            name: self.cfg.tee_container(index),
            tee_port: port,
            enclave_bin,
            sudo: self.cfg.sudo,
            mock,
            dkg_seed: mock.then(|| format!("{:064x}", index + 1)),
            seal: Some(SealSpec {
                tee_dir: vd.join("tee"),
                chain_id_hex: self.chain_id_hex()?,
            }),
            log_path: vd.join("enclave.log"),
            debug: self.cfg.debug,
        })?;
        self.enclaves.insert(index, guard);
        if !wait_tcp(port, 100) {
            self.enclaves.remove(&index);
            return Err(eyre!("enclave socket 127.0.0.1:{port} never came up"));
        }
        Ok(())
    }

    /// Launch the joiner node (validator-mode, verifier-join args), passing any
    /// extra node args (e.g. `--consensus.keys-dir ...`). Port of `e2e_launch_joiner`.
    pub fn launch_joiner(&mut self, index: usize, extra: &[&str]) -> Result<()> {
        let vd = self.cfg.validator_dir(index);
        fs::create_dir_all(vd.join("data"))?;
        fs::create_dir_all(vd.join("logs"))?;
        let secret = read_trimmed(&vd.join("reth-p2p-secret.hex"))?;

        let (public_polynomial, dkg_output) = verifier_material_paths(&self.cfg.dir);
        let mut a = self.reth_base_args(&vd, index);
        a.extend(args![
            "--validator",
            "--bootnodes",
            self.bootnodes().unwrap_or_default(),
            "--p2p-secret-key-hex",
            secret,
            "--metrics",
            format!("0.0.0.0:{}", self.cfg.metrics_port(index)),
            "--consensus.signing-key",
            vd.join("signing-key.hex").display(),
            "--validator.evm-key",
            vd.join("evm-key.hex").display(),
            "--consensus.listen-addr",
            format!("127.0.0.1:{}", self.cfg.consensus_port(index)),
            "--consensus.peers",
            self.consensus_peers()?,
            "--consensus.use-local-defaults",
            "--tee-enclave-socket",
            format!("127.0.0.1:{}", self.cfg.tee_port(index)),
            "--consensus.public-polynomial",
            public_polynomial.display(),
            "--consensus.dkg-output",
            dkg_output.display(),
        ]);
        a.extend(extra.iter().map(|s| s.to_string()));

        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.env("RUST_MIN_STACK", "16777216").args(&a);
        attach_log(&mut cmd, &vd)?;
        let guard = self.spawn_node(&format!("validator-{index}"), &vd, cmd)?;
        self.validators.insert(index, guard);
        Ok(())
    }

    /// Stop the joiner node (drop its owned handle → kill + reap). Port of
    /// `e2e_stop_joiner`.
    pub fn stop_joiner(&mut self, index: usize) -> Result<()> {
        self.validators.remove(&index);
        Ok(())
    }

    /// `--consensus.peers` (`<public_key>@<p2p_address>,…`) from `validators.json`.
    fn consensus_peers(&self) -> Result<String> {
        let raw = fs::read_to_string(self.cfg.dir.join("validators.json"))?;
        let v: serde_json::Value = serde_json::from_str(&raw)?;
        let arr = v
            .as_array()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        let peers: Vec<String> = arr
            .iter()
            .filter_map(|e| {
                let pk = e.get("public_key")?.as_str()?;
                let addr = e.get("p2p_address")?.as_str()?;
                Some(format!("{pk}@{addr}"))
            })
            .collect();
        Ok(peers.join(","))
    }

    /// Run `outbe-keygen <args>` and return stdout.
    fn keygen(&self, args: &[&str]) -> Result<String> {
        proc::run_capture(&self.cfg.bin_keygen, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_join_uses_active_committee_material_not_bootstrap_fixture() {
        let root = Path::new("/run/scenario-1");
        let (polynomial, output) = verifier_material_paths(root);

        assert_eq!(
            polynomial,
            root.join("validator-0/data/keys/dkg_polynomial.hex")
        );
        assert_eq!(output, root.join("validator-0/data/keys/dkg_output.hex"));
    }
}
