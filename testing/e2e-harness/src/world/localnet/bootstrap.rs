//! Bootstrap glue (ported `bootstrap-testnet.sh`): keys/DKG + genesis. The heavy
//! lifting stays one-shot subprocesses (`outbe-chain dkg bootstrap` and
//! `python3 seed_genesis.py`); the genesis skeleton, port rewrite, and dev felony
//! patch are native Rust.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt as _};

use eyre::{bail, eyre, Result, WrapErr};
use outbe_primitives::chain::{DEVNET_CHAIN_ID, TESTNET_CHAIN_ID};
use serde_json::json;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{env::TeeMode, internal::proc};

#[cfg(feature = "ocomp-integration")]
use super::ymd_utc;
use super::{worldwide_day, Localnet};

/// 10000 COEN (`10000 * 10^18`) as hex — the per-validator liquid balance.
const VALIDATOR_BALANCE_HEX: &str = "0x21E19E0C9BAB2400000";
/// Dev felony threshold (blocks) so downtime slashing is observable on the short
/// localnet epoch; must stay `<` the epoch length (`bootstrap-testnet.sh:234`).
const DEV_FELONY_THRESHOLD: u64 = 30;
const PROPOSER_FELONY_SLOT: u64 = 1;
const VOTER_FELONY_SLOT: u64 = 12;
/// A lifecycle E2E may opt into a short delay; testnet seed defaults remain
/// untouched. The value is supplied through `TESTNET_UNBONDING_PERIOD_SECS`.
const STAKING_SUFFIX: &str = "ee02";
const OCOMP_FINAL_FIXTURE_ROOT: &str = "testing/e2e-harness/fixtures/ocomp-final-v1";
const OCOMP_FINAL_ROOT_FILES: &[(&str, u32)] = &[
    ("dkg-output.hex", 0o600),
    ("polynomial.hex", 0o600),
    ("reth-bootnodes.txt", 0o640),
    ("validators.json", 0o640),
];
const OCOMP_FINAL_VALIDATOR_FILES: &[&str] = &[
    "evm-key.hex",
    "reth-p2p-secret.hex",
    "signing-key.hex",
    "signing-share.hex",
];

impl Localnet {
    /// Add the canonical block-1 TEE manifest after every scenario has finished
    /// mutating genesis. Real SGX binds the exact test SIGSTRUCT into a
    /// `DcapRequired` policy; non-hardware lanes remain separate
    /// `GramineDirectDev` chains. The product binary constructs and validates the
    /// policy bytes; the harness never reimplements its codec.
    pub(crate) fn bind_tee_genesis(&self) -> Result<()> {
        self.ensure_ocomp_genesis()?;
        let genesis = self.cfg.dir.join("genesis.json");
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)?;
        if value
            .get("config")
            .and_then(|config| config.get("teeAttestationV1"))
            .is_some()
        {
            return Ok(());
        }
        let seeded = self.cfg.dir.join("genesis.seeded.json");
        if seeded.exists() {
            bail!(
                "refusing to overwrite existing mutable genesis input {}",
                seeded.display()
            );
        }
        fs::rename(&genesis, &seeded)?;
        let mut command = Command::new(&self.cfg.bin_chain);
        command
            .arg("tee")
            .arg("genesis")
            .arg("--input")
            .arg(&seeded)
            .arg("--output")
            .arg(&genesis)
            .arg("--mode");
        match self.cfg.tee_mode {
            TeeMode::Real => {
                let signing_key = self.cfg.dir.join("test-sgx-signing-key.pem");
                let image_id =
                    proc::ensure_enclave_image(&self.cfg.repo, self.cfg.sudo, &signing_key)?;
                let measurement = proc::inspect_test_sgx_measurement(
                    &self.cfg.repo,
                    &self.real_enclave_bin()?,
                    &signing_key,
                    &image_id,
                    self.cfg.sudo,
                )?;
                command
                    .arg("dcap-required")
                    .arg("--mrenclave")
                    .arg(measurement.mrenclave)
                    .arg("--mrsigner")
                    .arg(measurement.mrsigner)
                    .arg("--isv-prod-id")
                    .arg(measurement.isv_prod_id.to_string())
                    .arg("--minimum-isv-svn")
                    .arg(measurement.isv_svn.to_string())
                    .arg("--minimum-tcb-evaluation-data-number")
                    .arg("1");
            }
            TeeMode::SgxNoAttest | TeeMode::GramineDirect | TeeMode::Mock => {
                command.arg("gramine-direct-dev");
            }
        }
        if let Err(error) = self.run_setup(&mut command, "outbe-chain tee genesis") {
            if !genesis.exists() {
                let _ = fs::rename(&seeded, &genesis);
            }
            return Err(error);
        }
        Ok(())
    }

    /// The current node binary requires a genesis-active OCOMP install for
    /// every network. Build that independent prerequisite with the product
    /// tooling before binding the hardware TEE policy; the DCAP harness does
    /// not reproduce either OCOMP's canonical codec or its signatures.
    fn ensure_ocomp_genesis(&self) -> Result<()> {
        eyre::ensure!(
            self.committee_size() > 0,
            "LocalNet OCOMP genesis requires validators"
        );

        let genesis = self.cfg.dir.join("genesis.json");
        let current: serde_json::Value = serde_json::from_slice(&fs::read(&genesis)?)?;
        if current
            .get("config")
            .and_then(|config| config.get("ocompForkInstallV1"))
            .is_some()
        {
            return Ok(());
        }

        let validators = self.cfg.dir.join("validators.json");
        let bindings_path = self.cfg.dir.join("ocomp-bindings-v1.json");
        let mut bindings_command = Command::new(&self.cfg.bin_chain);
        bindings_command.args([
            "ocomp",
            "bindings",
            "--input",
            genesis
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 genesis path"))?,
            "--validators",
            validators
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 validators path"))?,
            "--output",
            bindings_path
                .to_str()
                .ok_or_else(|| eyre!("non-UTF8 OCOMP bindings path"))?,
        ]);
        self.run_setup(&mut bindings_command, "outbe-chain ocomp bindings")?;

        let bindings: serde_json::Value = serde_json::from_slice(&fs::read(&bindings_path)?)?;
        let required_string = |name: &str| -> Result<&str> {
            bindings
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("OCOMP bindings missing string {name}"))
        };
        let required_u64 = |name: &str| -> Result<u64> {
            bindings
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| eyre!("OCOMP bindings missing u64 {name}"))
        };
        let identities = bindings
            .get("validatorIdentityHashes")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| eyre!("OCOMP bindings missing validator identities"))?;
        let validator_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&validators)?)?;
        let validator_manifest = validator_manifest
            .as_array()
            .ok_or_else(|| eyre!("validators manifest is not an array"))?;
        eyre::ensure!(
            identities.len() == self.committee_size()
                && validator_manifest.len() == self.committee_size(),
            "OCOMP founder registrations must cover the complete genesis ValidatorSet"
        );
        for (index, validator) in validator_manifest.iter().enumerate() {
            let validator_address = validator
                .get("address")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("validator-{index} manifest address is missing"))?;
            let consensus_bls = validator
                .get("public_key")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("validator-{index} manifest public key is missing"))?;
            let output_dir = self.cfg.validator_dir(index);
            let mut keygen = Command::new(&self.cfg.bin_keygen);
            keygen
                .arg("ocomp")
                .arg("--output-dir")
                .arg(&output_dir)
                .arg("--chain-id")
                .arg(required_u64("chainId")?.to_string())
                .arg("--genesis-hash")
                .arg(required_string("genesisHash")?)
                .arg("--validator-address")
                .arg(validator_address)
                .arg("--consensus-bls-min-pk")
                .arg(consensus_bls);
            self.run_setup(&mut keygen, "outbe-keygen ocomp")?;
        }

        let generated = self.cfg.dir.join("genesis.ocomp.json");
        let protocol_bundle = self.cfg.dir.join("protocol-bundle-v1.ocb1");
        let mut genesis_command = Command::new(&self.cfg.bin_chain);
        genesis_command
            .arg("ocomp")
            .arg("genesis")
            .arg("--input")
            .arg(&genesis)
            .arg("--validators")
            .arg(&validators)
            .arg("--registrations-dir")
            .arg(&self.cfg.dir)
            .arg("--output")
            .arg(&generated)
            .arg("--protocol-bundle-output")
            .arg(&protocol_bundle);
        self.run_setup(&mut genesis_command, "outbe-chain ocomp genesis")?;

        let original = self.cfg.dir.join("genesis.pre-ocomp.json");
        fs::rename(&genesis, &original)?;
        fs::rename(&generated, &genesis)?;
        Ok(())
    }

    /// Offset the debug-node wall clock to the immutable timestamp baked into
    /// the canonical OCOMP fixture without rewriting that fixture.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn ocomp_final_clock_offset(&self, now_secs: u64) -> Result<i64> {
        let target = self.ocomp_final_genesis_timestamp()?;
        let offset = i128::from(target) - i128::from(now_secs);
        i64::try_from(offset).map_err(|_| eyre!("canonical OCOMP clock offset leaves i64 range"))
    }

    /// Worldwide-day key authored by the immutable Final fixture's logical
    /// genesis time, independent of the host's current wall clock.
    #[cfg(feature = "ocomp-integration")]
    pub(crate) fn ocomp_final_worldwide_day(&self) -> Result<String> {
        let target = self.ocomp_final_genesis_timestamp()?;
        Ok(ymd_utc(target.saturating_add(50_400)))
    }

    #[cfg(feature = "ocomp-integration")]
    fn ocomp_final_genesis_timestamp(&self) -> Result<u64> {
        let path = self.cfg.dir.join("genesis.json");
        let genesis: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let raw = genesis
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre!("canonical OCOMP genesis timestamp is not a string"))?;
        u64::from_str_radix(raw.trim_start_matches("0x"), 16).map_err(Into::into)
    }

    /// Materialize the checked-in `Final` OCOMP devnet.
    ///
    /// Committee/DKG identities and the armed chain manifest are immutable
    /// fixture inputs. Only consensus and reth endpoint ports are rewritten for
    /// the scenario's allocated loopback port blocks.
    pub fn bootstrap_ocomp_final(&self) -> Result<()> {
        if self.cfg.dir.exists()
            && fs::read_dir(&self.cfg.dir)
                .wrap_err_with(|| format!("read scenario root {}", self.cfg.dir.display()))?
                .next()
                .is_some()
        {
            bail!(
                "canonical OCOMP fixture target is not empty: {}",
                self.cfg.dir.display()
            );
        }

        let fixture = self.cfg.repo.join(OCOMP_FINAL_FIXTURE_ROOT);
        let base = fixture.join("base");
        let artifacts = fixture.join("artifacts");
        let validator_manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(base.join("validators.json"))?)?;
        let fixture_validators = validator_manifest
            .as_array()
            .ok_or_else(|| eyre!("canonical OCOMP validators manifest is not an array"))?
            .len();
        eyre::ensure!(
            fixture_validators == self.committee_size(),
            "canonical OCOMP fixture contains {fixture_validators} validators, harness requested {}",
            self.committee_size()
        );
        fs::create_dir_all(&self.cfg.dir)?;
        for (relative, mode) in OCOMP_FINAL_ROOT_FILES {
            copy_fixture_file(&base.join(relative), &self.cfg.dir.join(relative), *mode)?;
        }
        for validator_index in 0..fixture_validators {
            for relative in OCOMP_FINAL_VALIDATOR_FILES {
                copy_fixture_file(
                    &base
                        .join(format!("validator-{validator_index}"))
                        .join(relative),
                    &self.cfg.validator_dir(validator_index).join(relative),
                    0o600,
                )?;
            }
        }
        copy_fixture_file(
            &artifacts.join("genesis-final.json"),
            &self.cfg.dir.join("genesis.json"),
            0o640,
        )?;
        self.rewrite_ports()
    }

    /// Keep a debug-only logical-clock E2E internally consistent by shifting the
    /// genesis header by the same signed number of seconds passed to every node.
    /// Without this, block 1 is correctly rejected by the testnet max-drift
    /// validator before a day-boundary scenario can exercise ZeroFee.
    pub(crate) fn shift_genesis_timestamp(&self, offset_secs: i64) -> Result<()> {
        let path = self.cfg.dir.join("genesis.json");
        let bytes = fs::read(&path)?;
        let mut genesis: serde_json::Value = serde_json::from_slice(&bytes)?;
        let raw = genesis
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("genesis timestamp is not a string"))?;
        let seconds = u64::from_str_radix(raw.trim_start_matches("0x"), 16)?;
        let shifted = i128::from(seconds) + i128::from(offset_secs);
        let shifted = u64::try_from(shifted)
            .map_err(|_| eyre::eyre!("genesis timestamp offset leaves u64 range"))?;
        genesis["timestamp"] = serde_json::Value::String(format!("0x{shifted:x}"));
        fs::write(path, serde_json::to_vec_pretty(&genesis)?)?;
        Ok(())
    }

    /// Bootstrap an N-validator set (keys, DKG, genesis). Runs unprivileged.
    /// `outbe-chain dkg bootstrap` and `seed_genesis.py` stay one-shot
    /// subprocesses; the genesis skeleton, port rewrite, and felony patch are
    /// native. `tuning` forwards `TESTNET_*` knobs (epoch length, DKG grace) some
    /// flows override; pass `&[]` for the defaults.
    pub fn bootstrap(&self, n: usize, tuning: &[(&str, String)]) -> Result<()> {
        fs::create_dir_all(&self.cfg.dir)?;

        // Step 1: DKG bootstrap — keys, polynomial, dkg-output, validators.json,
        // reth-bootnodes.txt.
        let mut cmd = Command::new(&self.cfg.bin_chain);
        cmd.args([
            "dkg",
            "bootstrap",
            "--output-dir",
            &self.dir(),
            "--validators",
            &n.to_string(),
        ]);
        for (k, v) in tuning {
            cmd.env(k, v);
        }
        self.run_setup(&mut cmd, "outbe-chain dkg bootstrap")?;

        // Step 1b: point the baked consensus/reth p2p endpoints at the resolved
        // ports (a no-op under the default layout).
        self.rewrite_ports()?;

        // Step 2: genesis skeleton (chain config + validator balances).
        self.write_genesis(tuning)?;

        // Step 2b: seed precompile storage (validator set/staking/etc.).
        self.seed_genesis(&worldwide_day())?;

        // Step 2c: dev felony thresholds for observable localnet slashing.
        self.patch_felony(tuning)?;
        // Step 2d: opt-in lifecycle timing for claim/accounting E2E scenarios.
        self.patch_staking_timing(tuning)?;
        Ok(())
    }

    /// Rewrite `validators.json` `p2p_address` to the resolved consensus port and
    /// each `reth-bootnodes.txt` enode's port to the resolved p2p port
    /// (`bootstrap-testnet.sh:105-128`).
    fn rewrite_ports(&self) -> Result<()> {
        let vpath = self.cfg.dir.join("validators.json");
        let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&vpath)?)?;
        let arr = v
            .as_array_mut()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        for (i, e) in arr.iter_mut().enumerate() {
            if let Some(obj) = e.as_object_mut() {
                obj.insert(
                    "p2p_address".into(),
                    json!(format!("127.0.0.1:{}", self.cfg.consensus_port(i))),
                );
            }
        }
        fs::write(&vpath, serde_json::to_string_pretty(&v)? + "\n")?;

        let bpath = self.cfg.dir.join("reth-bootnodes.txt");
        if let Ok(raw) = fs::read_to_string(&bpath) {
            let mut out = String::new();
            for (i, line) in raw.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                // enode://<id>@<host>:<port> — swap the trailing port for p2p[i].
                match line.trim().rsplit_once(':') {
                    Some((head, _)) => {
                        out.push_str(head);
                        out.push_str(&format!(":{}\n", self.cfg.p2p_port(i)));
                    }
                    None => {
                        out.push_str(line.trim());
                        out.push('\n');
                    }
                }
            }
            fs::write(&bpath, out)?;
        }
        Ok(())
    }

    /// Write the devnet genesis skeleton: static chain config (chain id 424242, epoch /
    /// DKG params from `tuning`) plus a pre-funded `alloc` of each validator
    /// address (`bootstrap-testnet.sh:133-203`).
    fn write_genesis(&self, tuning: &[(&str, String)]) -> Result<()> {
        // OCOMP retains committee snapshots for a bounded number of epochs.
        // The default must cover one complete result-vote window; 120 blocks
        // was accepted before OCOMP became mandatory but is now invalid.
        let epoch = tuned(tuning, "TESTNET_EPOCH_LENGTH_BLOCKS", 300);
        let dkg_prepare = tuned(tuning, "TESTNET_DKG_PREPARE_WINDOW_BLOCKS", 30);
        let dkg_grace = tuned(tuning, "TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", 30);
        let validator_balance = validator_balance_hex(tuning);

        let vjson: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.cfg.dir.join("validators.json"))?)?;
        let arr = vjson
            .as_array()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        let mut alloc = serde_json::Map::new();
        for e in arr {
            let addr = e
                .get("address")
                .and_then(|a| a.as_str())
                .ok_or_else(|| eyre!("validator entry missing address"))?;
            // Genesis alloc keys are the address without the `0x` prefix.
            let key = addr.trim_start_matches("0x").to_string();
            alloc.insert(key, json!({ "balance": validator_balance.clone() }));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // genesisTime is one day in the past so the chain can produce immediately.
        let genesis_time = OffsetDateTime::from_unix_timestamp(now as i64 - 86_400)
            .wrap_err("genesis time")?
            .format(&Rfc3339)
            .wrap_err("format genesis time")?;

        let chain_id = localnet_chain_id(self.cfg.tee_mode);
        let mut genesis = json!({
            "config": {
                "chainId": chain_id,
                "homesteadBlock": 0,
                "eip150Block": 0,
                "eip155Block": 0,
                "eip158Block": 0,
                "byzantiumBlock": 0,
                "constantinopleBlock": 0,
                "petersburgBlock": 0,
                "istanbulBlock": 0,
                "berlinBlock": 0,
                "londonBlock": 0,
                "mergeNetsplitBlock": 0,
                "terminalTotalDifficulty": 0,
                "terminalTotalDifficultyPassed": true,
                "shanghaiTime": 0,
                "cancunTime": 0,
                "pragueTime": 0,
                "epochLengthBlocks": epoch,
                "dkgPrepareWindowBlocks": dkg_prepare,
                "dkgActivationGraceBlocks": dkg_grace,
                "genesisTime": genesis_time,
            },
            "nonce": "0x0",
            "timestamp": format!("0x{now:x}"),
            "extraData": "0x",
            "gasLimit": "0x1c9c380",
            "difficulty": "0x0",
            "mixHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "coinbase": "0x0000000000000000000000000000000000000000",
            "alloc": alloc,
        });
        // Four real enclaves intentionally share one SGX host in this lane.
        // EPC scheduling can make one validator's otherwise ~100-200 ms offer
        // re-execution take just over five seconds. The testnet defaults
        // assume one enclave per validator host; widen only this co-located
        // hardware test network so a local resource stall does not cancel the
        // execution-read budget before the deterministic retry completes.
        apply_co_located_sgx_timing(&mut genesis, self.cfg.tee_mode)?;
        fs::write(
            self.cfg.dir.join("genesis.json"),
            serde_json::to_string_pretty(&genesis)? + "\n",
        )?;
        Ok(())
    }

    /// Seed precompile storage into genesis (`bootstrap-testnet.sh:209-226`) via
    /// the kept `scripts/seed_genesis.py`.
    fn seed_genesis(&self, worldwide_day: &str) -> Result<()> {
        let genesis = self.cfg.dir.join("genesis.json");
        let mut cmd = Command::new("python3");
        cmd.arg(self.cfg.repo.join("scripts/seed_genesis.py"))
            .arg("--genesis")
            .arg(&genesis)
            .arg("--seed")
            .arg(&self.cfg.seed)
            .arg("--validators")
            .arg(self.cfg.dir.join("validators.json"))
            .arg("--worldwide-day")
            .arg(worldwide_day)
            .arg("--output")
            .arg(&genesis);
        self.run_setup(&mut cmd, "seed_genesis.py")
    }

    /// Lower the SlashIndicator felony thresholds so downtime slashing triggers
    /// within the short dev epoch (`bootstrap-testnet.sh:228-253`).
    fn patch_felony(&self, tuning: &[(&str, String)]) -> Result<()> {
        let epoch = tuned(tuning, "TESTNET_EPOCH_LENGTH_BLOCKS", 300);
        let felony_threshold = tuned(tuning, "TESTNET_DEV_FELONY_THRESHOLD", DEV_FELONY_THRESHOLD);
        if felony_threshold >= epoch {
            bail!("dev felony threshold {felony_threshold} must be < epoch length {epoch}");
        }
        let path = self.cfg.dir.join("genesis.json");
        let mut g: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        let alloc = g
            .get_mut("alloc")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| eyre!("genesis has no alloc object"))?;

        // SlashIndicator lives at 0x…ee01 (config slot 1 = proposer felony, slot
        // 12 = voter felony). Match the address however it's spelled in alloc.
        let key = alloc.keys().find(|k| ends_with_ee01(k)).cloned();
        let key = key.unwrap_or_else(|| {
            let k = "0x000000000000000000000000000000000000ee01".to_string();
            alloc.insert(k.clone(), json!({ "balance": "0x0", "code": "0xef0000" }));
            k
        });
        let entry = alloc
            .get_mut(&key)
            .and_then(|e| e.as_object_mut())
            .ok_or_else(|| eyre!("felony alloc entry is not an object"))?;
        let storage = entry
            .entry("storage")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| eyre!("felony storage is not an object"))?;
        patch_felony_storage(storage, felony_threshold);

        fs::write(&path, serde_json::to_string_pretty(&g)? + "\n")?;
        Ok(())
    }

    /// Apply opt-in staking lifecycle timings to the already-seeded genesis.
    /// No slot is changed unless its corresponding `TESTNET_*` knob is present.
    fn patch_staking_timing(&self, tuning: &[(&str, String)]) -> Result<()> {
        let unbonding = tuned_optional(tuning, "TESTNET_UNBONDING_PERIOD_SECS");
        let slashed = tuned_optional(tuning, "TESTNET_SLASHED_WITHDRAWAL_DELAY_SECS");
        if unbonding.is_none() && slashed.is_none() {
            return Ok(());
        }

        let path = self.cfg.dir.join("genesis.json");
        let mut genesis: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        patch_staking_storage(&mut genesis, unbonding, slashed)?;
        fs::write(&path, serde_json::to_string_pretty(&genesis)? + "\n")?;
        Ok(())
    }
}

fn copy_fixture_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .wrap_err_with(|| format!("inspect OCOMP fixture file {}", source.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "OCOMP fixture input is not a regular non-symlink file: {}",
            source.display()
        );
    }
    let parent = destination
        .parent()
        .ok_or_else(|| eyre!("OCOMP fixture destination has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::copy(source, destination).wrap_err_with(|| {
        format!(
            "copy OCOMP fixture {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    #[cfg(unix)]
    fs::set_permissions(destination, Permissions::from_mode(mode))?;
    Ok(())
}

fn apply_co_located_sgx_timing(
    genesis: &mut serde_json::Value,
    tee_mode: crate::env::TeeMode,
) -> Result<()> {
    if !matches!(
        tee_mode,
        crate::env::TeeMode::Real | crate::env::TeeMode::SgxNoAttest
    ) {
        return Ok(());
    }
    let config = genesis["config"]
        .as_object_mut()
        .ok_or_else(|| eyre!("generated genesis config is not an object"))?;
    config.insert("leaderTimeoutMs".to_owned(), json!(15_000));
    config.insert("certificationTimeoutMs".to_owned(), json!(30_000));
    Ok(())
}

/// DCAP hardware E2E exercises the same DcapRequired network class as testnet.
/// SGX-no-attest uses real hardware but deliberately keeps the separately
/// identified GramineDirectDev chain.
const fn localnet_chain_id(tee_mode: TeeMode) -> u64 {
    match tee_mode {
        TeeMode::Real => TESTNET_CHAIN_ID,
        TeeMode::SgxNoAttest | TeeMode::GramineDirect | TeeMode::Mock => DEVNET_CHAIN_ID,
    }
}

fn patch_felony_storage(
    storage: &mut serde_json::Map<String, serde_json::Value>,
    felony_threshold: u64,
) {
    let threshold = json!(format!("0x{felony_threshold:064x}"));
    storage.insert(format!("0x{PROPOSER_FELONY_SLOT:064x}"), threshold.clone());
    storage.insert(format!("0x{VOTER_FELONY_SLOT:064x}"), threshold);
}

/// A `TESTNET_*` tuning override parsed as `u64`, or `default`.
fn tuned(tuning: &[(&str, String)], key: &str, default: u64) -> u64 {
    tuning
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(default)
}

fn tuned_optional(tuning: &[(&str, String)], key: &str) -> Option<u64> {
    tuning
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .and_then(|(_, value)| value.parse().ok())
}

fn validator_balance_hex(tuning: &[(&str, String)]) -> String {
    tuned_optional(tuning, "TESTNET_VALIDATOR_BALANCE_COEN").map_or_else(
        || VALIDATOR_BALANCE_HEX.to_owned(),
        |coen| format!("0x{:x}", u128::from(coen) * 10u128.pow(18)),
    )
}

fn patch_staking_storage(
    genesis: &mut serde_json::Value,
    unbonding: Option<u64>,
    slashed: Option<u64>,
) -> Result<()> {
    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre!("genesis has no alloc object"))?;
    let key = alloc
        .keys()
        .find(|key| address_has_suffix(key, STAKING_SUFFIX))
        .cloned()
        .ok_or_else(|| eyre!("seeded genesis has no Staking alloc entry"))?;
    let storage = alloc
        .get_mut(&key)
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|entry| entry.get_mut("storage"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre!("Staking alloc entry has no storage object"))?;
    if let Some(value) = unbonding {
        storage.insert(format!("0x{:064x}", 1u64), json!(format!("0x{value:064x}")));
    }
    if let Some(value) = slashed {
        storage.insert(
            format!("0x{:064x}", 11u64),
            json!(format!("0x{value:064x}")),
        );
    }
    Ok(())
}

/// Whether an alloc key normalizes (lowercase, `0x`-stripped, left-padded to 40)
/// to a SlashIndicator address ending in `ee01` (`bootstrap-testnet.sh:244`).
fn ends_with_ee01(key: &str) -> bool {
    address_has_suffix(key, "ee01")
}

fn address_has_suffix(key: &str, suffix: &str) -> bool {
    let k = key.to_lowercase();
    let k = k.strip_prefix("0x").unwrap_or(&k);
    format!("{k:0>40}").ends_with(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_genesis() -> serde_json::Value {
        json!({
            "alloc": {
                "000000000000000000000000000000000000ee02": {
                    "storage": {
                        format!("0x{:064x}", 1u64): format!("0x{:064x}", 1_814_400u64),
                        format!("0x{:064x}", 11u64): format!("0x{:064x}", 3_628_800u64)
                    }
                }
            }
        })
    }

    #[test]
    fn lifecycle_timing_patch_updates_only_requested_staking_slots() {
        let mut genesis = seeded_genesis();
        patch_staking_storage(&mut genesis, Some(8), None).unwrap();
        let storage = genesis["alloc"]["000000000000000000000000000000000000ee02"]["storage"]
            .as_object()
            .unwrap();
        assert_eq!(
            storage[&format!("0x{:064x}", 1u64)],
            json!(format!("0x{:064x}", 8u64))
        );
        assert_eq!(
            storage[&format!("0x{:064x}", 11u64)],
            json!(format!("0x{:064x}", 3_628_800u64))
        );
    }

    #[test]
    fn lifecycle_timing_patch_rejects_unseeded_staking_entry() {
        let mut genesis = json!({ "alloc": {} });
        assert!(patch_staking_storage(&mut genesis, Some(8), None).is_err());
    }

    #[test]
    fn felony_patch_uses_current_slashindicator_config_slots() {
        let mut storage = serde_json::Map::new();
        patch_felony_storage(&mut storage, DEV_FELONY_THRESHOLD);
        let expected = json!(format!("0x{DEV_FELONY_THRESHOLD:064x}"));
        assert_eq!(
            storage.get(&format!("0x{PROPOSER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
        assert_eq!(
            storage.get(&format!("0x{VOTER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
        assert!(storage.get(&format!("0x{:064x}", 13u64)).is_none());
    }

    #[test]
    fn felony_patch_accepts_a_scenario_specific_threshold() {
        let mut storage = serde_json::Map::new();
        patch_felony_storage(&mut storage, 119);
        let expected = json!(format!("0x{:064x}", 119u64));
        assert_eq!(
            storage.get(&format!("0x{PROPOSER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
        assert_eq!(
            storage.get(&format!("0x{VOTER_FELONY_SLOT:064x}")),
            Some(&expected)
        );
    }

    #[test]
    fn co_located_real_sgx_gets_wider_consensus_windows_only_in_hardware_lane() {
        let mut real = json!({ "config": {} });
        apply_co_located_sgx_timing(&mut real, crate::env::TeeMode::Real).unwrap();
        assert_eq!(real["config"]["leaderTimeoutMs"], json!(15_000));
        assert_eq!(real["config"]["certificationTimeoutMs"], json!(30_000));

        let mut mock = json!({ "config": {} });
        apply_co_located_sgx_timing(&mut mock, crate::env::TeeMode::Mock).unwrap();
        assert!(mock["config"].as_object().unwrap().is_empty());
    }

    #[test]
    fn hardware_and_dev_tee_lanes_have_distinct_network_identities() {
        assert_eq!(localnet_chain_id(TeeMode::Real), TESTNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::SgxNoAttest), DEVNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::GramineDirect), DEVNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::Mock), DEVNET_CHAIN_ID);
    }

    #[test]
    fn validator_liquid_balance_can_be_tuned_per_scenario() {
        assert_eq!(validator_balance_hex(&[]), VALIDATOR_BALANCE_HEX);

        let tuned =
            validator_balance_hex(&[("TESTNET_VALIDATOR_BALANCE_COEN", "2100000".to_owned())]);
        assert_eq!(
            u128::from_str_radix(tuned.trim_start_matches("0x"), 16).unwrap(),
            2_100_000u128 * 10u128.pow(18)
        );
    }
}
