//! Bootstrap glue (ported `bootstrap-testnet.sh`): keys/DKG + genesis. The heavy
//! lifting stays one-shot subprocesses (`outbe-chain dkg bootstrap` and
//! `python3 seed_genesis.py`); the genesis skeleton, port rewrite, and dev felony
//! patch are native Rust.

use std::collections::HashSet;
#[cfg(all(test, unix))]
use std::fs::Permissions;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::num::NonZeroU64;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
#[cfg(not(feature = "ocomp-integration"))]
use alloy_primitives::hex;
use eyre::{bail, eyre, Result, WrapErr};
use outbe_primitives::chain::{DEVNET_CHAIN_ID, TESTNET_CHAIN_ID};
use serde_json::json;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{env::TeeMode, internal::proc};

use super::{worldwide_day, Localnet};

/// 10000 COEN (`10000 * 10^6`) as hex — the per-validator liquid balance.
const VALIDATOR_BALANCE_HEX: &str = "0x2540be400";
/// Dev felony threshold (blocks) so downtime slashing is observable on the short
/// localnet epoch; must stay `<` the epoch length (`bootstrap-testnet.sh:234`).
const DEV_FELONY_THRESHOLD: u64 = 30;
const PROPOSER_FELONY_SLOT: u64 = 1;
const VOTER_FELONY_SLOT: u64 = 12;
const LOCALNET_METADOSIS_FORMING_LEAD_SECONDS: u64 = 60;
const LOCALNET_METADOSIS_LOOKBACK_SECONDS: u64 = 0;
const LOCALNET_METADOSIS_OFFERING_SECONDS: u64 = 120;
const LOCALNET_METADOSIS_WAITING_SECONDS: u64 = 30;
const LOCALNET_METADOSIS_BOOTSTRAP_SECONDS: u64 = 300;
const LOCALNET_METADOSIS_ADVANCE_SECONDS: u64 = 10;
const LOCALNET_OCOMP_VOTE_WINDOW_BLOCKS: u64 = 120;
const DEFAULT_EPOCH_LENGTH_BLOCKS: u64 = 300;
const DEFAULT_DKG_PREPARE_WINDOW_BLOCKS: u64 = 30;
const DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS: u64 = 30;
const RADICLE_REGISTRY_ADDRESS: &str = "000000000000000000000000000000000000ee11";
const RADICLE_MAX_REPOSITORIES_SLOT: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000005";

/// Valid, scenario-local genesis choices used by lifecycle E2E tests.
///
/// The fields are deliberately private: callers use the checked builders, and
/// [`Localnet::bootstrap_with_profile`] validates the values again against the
/// actual committee size before generating any files. ValidatorSet and Staking
/// overrides are written to a scenario-local seed copy consumed by the normal
/// genesis seeder; they are never installed through raw storage writes.
#[derive(Debug, Clone)]
pub struct BootstrapProfile {
    chain_id: Option<NonZeroU64>,
    validator_set_owner_index: Option<usize>,
    max_validators: Option<u32>,
    reregistration_cooldown_blocks: Option<u32>,
    epoch_length_blocks: NonZeroU64,
    dkg_prepare_window_blocks: NonZeroU64,
    dkg_activation_grace_blocks: NonZeroU64,
    dev_felony_threshold: u64,
    unbonding_period_secs: Option<NonZeroU64>,
    slashed_withdrawal_delay_secs: Option<NonZeroU64>,
    validator_balance_coen: Option<u64>,
    metadosis_forming_seconds: Option<u64>,
    metadosis_offering_seconds: u64,
    ocomp_vote_window_blocks: u64,
}

impl Default for BootstrapProfile {
    fn default() -> Self {
        Self {
            chain_id: None,
            validator_set_owner_index: None,
            max_validators: None,
            reregistration_cooldown_blocks: None,
            epoch_length_blocks: NonZeroU64::new(DEFAULT_EPOCH_LENGTH_BLOCKS)
                .expect("non-zero default epoch"),
            dkg_prepare_window_blocks: NonZeroU64::new(DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
                .expect("non-zero default prepare window"),
            dkg_activation_grace_blocks: NonZeroU64::new(DEFAULT_DKG_ACTIVATION_GRACE_BLOCKS)
                .expect("non-zero default activation grace"),
            dev_felony_threshold: DEV_FELONY_THRESHOLD,
            unbonding_period_secs: None,
            slashed_withdrawal_delay_secs: None,
            validator_balance_coen: None,
            metadosis_forming_seconds: None,
            metadosis_offering_seconds: LOCALNET_METADOSIS_OFFERING_SECONDS,
            ocomp_vote_window_blocks: LOCALNET_OCOMP_VOTE_WINDOW_BLOCKS,
        }
    }
}

impl BootstrapProfile {
    /// Override the EIP-155 chain identity. Zero is rejected.
    pub fn with_chain_id(mut self, chain_id: u64) -> Result<Self> {
        self.chain_id =
            Some(NonZeroU64::new(chain_id).ok_or_else(|| eyre!("chain id must be > 0"))?);
        Ok(self)
    }

    /// Make an existing genesis validator the ValidatorSet owner.
    ///
    /// The index is checked against the requested committee size at bootstrap.
    pub fn with_validator_set_owner(mut self, validator_index: usize) -> Self {
        self.validator_set_owner_index = Some(validator_index);
        self
    }

    /// Override the registry capacity. It must cover the genesis committee.
    pub fn with_max_validators(mut self, max_validators: u32) -> Result<Self> {
        if max_validators == 0 {
            bail!("max validators must be > 0");
        }
        self.max_validators = Some(max_validators);
        Ok(self)
    }

    /// Override the re-registration cooldown. Zero intentionally means no
    /// cooldown and is a valid acceleration setting.
    pub fn with_reregistration_cooldown_blocks(mut self, blocks: u32) -> Self {
        self.reregistration_cooldown_blocks = Some(blocks);
        self
    }

    /// Set the positive periodic epoch/DKG schedule.
    pub fn with_dkg_timing(
        mut self,
        epoch_length_blocks: u64,
        prepare_window_blocks: u64,
        activation_grace_blocks: u64,
    ) -> Result<Self> {
        self.epoch_length_blocks = positive(epoch_length_blocks, "epoch length")?;
        self.dkg_prepare_window_blocks = positive(prepare_window_blocks, "DKG prepare window")?;
        self.dkg_activation_grace_blocks =
            positive(activation_grace_blocks, "DKG activation grace")?;
        if prepare_window_blocks > epoch_length_blocks {
            bail!(
                "DKG prepare window {prepare_window_blocks} exceeds epoch length \
                 {epoch_length_blocks}"
            );
        }
        Ok(self)
    }

    /// Set the localnet felony threshold. It may be zero, but must remain below
    /// the epoch length so the per-epoch reset cannot hide a test offense.
    pub fn with_dev_felony_threshold(mut self, blocks: u64) -> Result<Self> {
        if blocks >= self.epoch_length_blocks.get() {
            bail!(
                "dev felony threshold {blocks} must be < epoch length {}",
                self.epoch_length_blocks
            );
        }
        self.dev_felony_threshold = blocks;
        Ok(self)
    }

    /// Set positive Staking delays through the public genesis seed schema.
    pub fn with_staking_timing(
        mut self,
        unbonding_period_secs: Option<u64>,
        slashed_withdrawal_delay_secs: Option<u64>,
    ) -> Result<Self> {
        self.unbonding_period_secs = unbonding_period_secs
            .map(|value| positive(value, "unbonding period"))
            .transpose()?;
        self.slashed_withdrawal_delay_secs = slashed_withdrawal_delay_secs
            .map(|value| positive(value, "slashed withdrawal delay"))
            .transpose()?;
        Ok(self)
    }

    /// Parse the legacy `TESTNET_*` slice used by existing feature steps.
    ///
    /// Parsing is strict: malformed or unsupported knobs fail before bootstrap
    /// instead of silently falling back to a different genesis.
    fn from_tuning(tuning: &[(&str, String)]) -> Result<Self> {
        let mut profile = Self::default();
        for (key, value) in tuning {
            let parsed = || {
                value
                    .parse::<u64>()
                    .wrap_err_with(|| format!("{key} must be an unsigned integer"))
            };
            match *key {
                "TESTNET_EPOCH_LENGTH_BLOCKS" => {
                    profile.epoch_length_blocks = positive(parsed()?, "epoch length")?;
                }
                "TESTNET_DKG_PREPARE_WINDOW_BLOCKS" => {
                    profile.dkg_prepare_window_blocks = positive(parsed()?, "DKG prepare window")?;
                }
                "TESTNET_DKG_ACTIVATION_GRACE_BLOCKS" => {
                    profile.dkg_activation_grace_blocks =
                        positive(parsed()?, "DKG activation grace")?;
                }
                "TESTNET_DEV_FELONY_THRESHOLD" => {
                    profile.dev_felony_threshold = parsed()?;
                }
                "TESTNET_UNBONDING_PERIOD_SECS" => {
                    profile.unbonding_period_secs = Some(positive(parsed()?, "unbonding period")?);
                }
                "TESTNET_SLASHED_WITHDRAWAL_DELAY_SECS" => {
                    profile.slashed_withdrawal_delay_secs =
                        Some(positive(parsed()?, "slashed withdrawal delay")?);
                }
                "TESTNET_VALIDATOR_BALANCE_COEN" => {
                    profile.validator_balance_coen = Some(parsed()?);
                }
                "TESTNET_METADOSIS_FORMING_SECONDS" => {
                    profile.metadosis_forming_seconds = Some(parsed()?);
                }
                "TESTNET_METADOSIS_OFFERING_SECONDS" => {
                    profile.metadosis_offering_seconds = parsed()?;
                }
                "TESTNET_OCOMP_VOTE_WINDOW_BLOCKS" => {
                    profile.ocomp_vote_window_blocks = parsed()?;
                }
                other => {
                    bail!("unsupported localnet tuning key {other}");
                }
            }
        }
        profile.validate_timing()?;
        Ok(profile)
    }

    fn validate_timing(&self) -> Result<()> {
        if self.dkg_prepare_window_blocks > self.epoch_length_blocks {
            bail!(
                "DKG prepare window {} exceeds epoch length {}",
                self.dkg_prepare_window_blocks,
                self.epoch_length_blocks
            );
        }
        if self.epoch_length_blocks.get() > u64::from(u32::MAX) {
            bail!("epoch length exceeds the on-chain u32 range");
        }
        if self.dev_felony_threshold >= self.epoch_length_blocks.get() {
            bail!(
                "dev felony threshold {} must be < epoch length {}",
                self.dev_felony_threshold,
                self.epoch_length_blocks
            );
        }
        Ok(())
    }

    fn validate_for_committee(&self, n: usize) -> Result<()> {
        self.validate_timing()?;
        if n == 0 {
            bail!("bootstrap committee must contain at least one validator");
        }
        if n > u32::MAX as usize {
            bail!("bootstrap committee size exceeds u32 range");
        }
        if let Some(index) = self.validator_set_owner_index {
            if index >= n {
                bail!("ValidatorSet owner index {index} is outside committee size {n}");
            }
        }
        if let Some(max) = self.max_validators {
            if max < n as u32 {
                bail!("max validators {max} is smaller than genesis committee size {n}");
            }
        }
        Ok(())
    }
}

fn positive(value: u64, label: &str) -> Result<NonZeroU64> {
    NonZeroU64::new(value).ok_or_else(|| eyre!("{label} must be > 0"))
}

impl Localnet {
    /// Add the canonical block-1 TEE manifest after every scenario has finished
    /// mutating genesis. The DCAP lane binds the exact test SIGSTRUCT into a
    /// `DcapRequired` policy. SGX without DCAP and the non-hardware lanes use an
    /// explicit `GramineDirectDev` policy. The product binary constructs and
    /// validates the policy bytes; the harness never reimplements its codec.
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
                let image_id = proc::ensure_enclave_image(
                    &self.cfg.repo,
                    self.cfg.sudo,
                    &signing_key,
                    self.enclave_image_id.as_ref(),
                )?;
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
            TeeMode::SgxNoAttest | TeeMode::GramineDirect | TeeMode::Mock | TeeMode::MockNative => {
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

    /// Stage the founder material required by each node's mandatory embedded
    /// OCOMP runtime when the harness is built without the external OCOMP
    /// scenario drivers.
    #[cfg(not(feature = "ocomp-integration"))]
    pub(crate) fn ensure_embedded_ocomp_domain_material(&self) -> Result<()> {
        for index in 0..self.committee_size() {
            self.ensure_embedded_ocomp_validator_domain_material(index)?;
        }
        Ok(())
    }

    /// Stage one validator's mandatory embedded OCOMP runtime domain. Founder
    /// startup and late validator promotion share this exact material contract.
    #[cfg(not(feature = "ocomp-integration"))]
    pub(crate) fn ensure_embedded_ocomp_validator_domain_material(
        &self,
        index: usize,
    ) -> Result<()> {
        let bundle = fs::read(self.cfg.dir.join("protocol-bundle-v1.ocb1"))?;
        let validator_dir = self.cfg.validator_dir(index);
        let signing_key = fs::read(validator_dir.join("ocomp-key-v1.hex"))?;
        let domain = validator_dir.join("ocomp").join("domain-v1");
        fs::create_dir_all(&domain)?;
        publish_embedded_ocomp_file(&domain.join("protocol-bundle-v1.ocb1"), &bundle, 0o640)?;
        publish_embedded_ocomp_file(&domain.join("ocomp-key-v1.hex"), &signing_key, 0o600)?;
        let validator_index = u8::try_from(index)?;
        let evm_key = hex::encode([validator_index.saturating_add(0x71); 32]);
        publish_embedded_ocomp_file(
            &domain.join("ocomp-evm-key.hex"),
            format!("{evm_key}\n").as_bytes(),
            0o600,
        )?;
        Ok(())
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
    /// native. `tuning` strictly maps the legacy `TESTNET_*` knobs used by
    /// existing flows into a [`BootstrapProfile`]; pass `&[]` for defaults.
    pub fn bootstrap(&self, n: usize, tuning: &[(&str, String)]) -> Result<()> {
        let profile = BootstrapProfile::from_tuning(tuning)?;
        self.bootstrap_with_profile(n, &profile)
    }

    /// Bootstrap using a checked, typed genesis profile.
    pub fn bootstrap_with_profile(&self, n: usize, profile: &BootstrapProfile) -> Result<()> {
        profile.validate_for_committee(n)?;
        self.validate_effective_seed_capacity(n, profile)?;
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
        self.run_setup(&mut cmd, "outbe-chain dkg bootstrap")?;

        // Registration V2 binds every founder to the native Heartwood identity
        // that its sidecar will load from this validator directory.
        self.materialize_radicle_identities(n)?;

        // Step 1b: point the baked consensus/reth p2p endpoints at the resolved
        // ports (a no-op under the default layout).
        self.rewrite_ports()?;

        // Step 2: genesis skeleton (chain config + validator balances).
        self.write_genesis(profile)?;

        // Step 2b: seed precompile storage (validator set/staking/etc.).
        self.seed_genesis(&worldwide_day(), profile)?;

        // Step 2c: dev felony thresholds for observable localnet slashing.
        self.patch_felony(profile)?;
        Ok(())
    }

    /// Stop and rebuild this scenario directory with a new valid genesis.
    ///
    /// [`crate::world::validators::RegistrationIdentity`] values are owned by
    /// the caller and remain reusable after this method wipes on-disk chain
    /// state. This is the intended way to exercise proof replay across two
    /// otherwise-valid chain IDs.
    pub fn rebootstrap_with_profile(&mut self, n: usize, profile: &BootstrapProfile) -> Result<()> {
        profile.validate_for_committee(n)?;
        let signing_key_path = self.cfg.dir.join("test-sgx-signing-key.pem");
        let signing_key = read_rebootstrap_signing_key(&signing_key_path)?;
        self.stop()?;
        self.wipe()?;
        restore_rebootstrap_signing_key(&signing_key_path, signing_key.as_deref())?;
        self.bootstrap_with_profile(n, profile)
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

    fn materialize_radicle_identities(&self, n: usize) -> Result<()> {
        let validators_path = self.cfg.dir.join("validators.json");
        let mut validators: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&validators_path)?)?;
        let entries = validators
            .as_array_mut()
            .ok_or_else(|| eyre!("validators.json is not an array"))?;
        eyre::ensure!(entries.len() == n, "validators.json founder count mismatch");

        let mut node_ids = HashSet::with_capacity(n);
        for (index, validator) in entries.iter_mut().enumerate() {
            let home = self.cfg.validator_dir(index).join("radicle");
            let output = proc::run_capture(
                &self.cfg.bin_keygen,
                &["radicle", "--output-dir", &home.display().to_string()],
            )?;
            let node_id = output
                .lines()
                .find_map(|line| {
                    line.split_once("node id hex:")
                        .map(|(_, value)| value.trim())
                })
                .ok_or_else(|| eyre!("outbe-keygen did not print validator-{index} NodeId"))?;
            eyre::ensure!(
                node_id.len() == 64 && node_id.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "outbe-keygen printed invalid validator-{index} NodeId"
            );
            eyre::ensure!(
                node_ids.insert(node_id.to_owned()),
                "duplicate founder Radicle NodeId"
            );
            validator
                .as_object_mut()
                .ok_or_else(|| eyre!("validator-{index} manifest entry is not an object"))?
                .insert("radicle_node_id".into(), json!(format!("0x{node_id}")));
        }
        fs::write(
            validators_path,
            serde_json::to_string_pretty(&validators)? + "\n",
        )?;
        Ok(())
    }

    /// Write the genesis skeleton: profiled chain id / epoch / DKG parameters
    /// plus a pre-funded `alloc` of each validator address
    /// (`bootstrap-testnet.sh:133-203`).
    fn write_genesis(&self, profile: &BootstrapProfile) -> Result<()> {
        let ocomp_vote_window = localnet_ocomp_vote_window_blocks(profile);
        let validator_balance = validator_balance_hex(profile);
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
        let localnet_forming_period = localnet_metadosis_forming_period_seconds(now, profile)?;
        // genesisTime is one day in the past so the chain can produce immediately.
        let genesis_time = OffsetDateTime::from_unix_timestamp(now as i64 - 86_400)
            .wrap_err("genesis time")?
            .format(&Rfc3339)
            .wrap_err("format genesis time")?;

        let chain_id = profile
            .chain_id
            .map_or_else(|| localnet_chain_id(self.cfg.tee_mode), NonZeroU64::get);
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
                "epochLengthBlocks": profile.epoch_length_blocks.get(),
                "dkgPrepareWindowBlocks": profile.dkg_prepare_window_blocks.get(),
                "dkgActivationGraceBlocks": profile.dkg_activation_grace_blocks.get(),
                "genesisTime": genesis_time,
                "outbeProtocol": {
                    "schemaVersion": 1,
                    "metadosis": {
                        "formingPeriodSeconds": localnet_forming_period,
                        "lookbackDelaySeconds": LOCALNET_METADOSIS_LOOKBACK_SECONDS,
                        "offeringPeriodSeconds":
                            localnet_metadosis_offering_period_seconds(profile),
                        "waitingPeriodSeconds": LOCALNET_METADOSIS_WAITING_SECONDS,
                        "bootstrapDurationSeconds": LOCALNET_METADOSIS_BOOTSTRAP_SECONDS,
                        "advanceIntervalSeconds": LOCALNET_METADOSIS_ADVANCE_SECONDS,
                    },
                    "ocomp": {
                        "computeVoteWindowBlocks": ocomp_vote_window,
                    },
                },
            },
            "nonce": "0x0",
            "timestamp": format!("0x{now:x}"),
            "extraData": "0x",
            "gasLimit": "0x1c9c380",
            // Start at the EIP-1559 floor. Omitting this takes reth's 1 Gwei
            // Ethereum default, which no account here can pay: COEN carries six
            // decimals, so a validator's 10000 COEN is 10^10 base units and buys
            // 13 gas at a Gwei. Every harness transaction is priced at
            // MIN_PROTOCOL_BASE_FEE for that reason, and the base fee only sheds
            // 12.5% per under-target block — about 140 blocks before the floor
            // is reachable, while the localnet starts issuing work at block 2.
            "baseFeePerGas": format!("0x{MIN_PROTOCOL_BASE_FEE:x}"),
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
    fn seed_genesis(&self, worldwide_day: &str, profile: &BootstrapProfile) -> Result<()> {
        let genesis = self.cfg.dir.join("genesis.json");
        let seed = self.prepare_scenario_seed(profile)?;
        let mut cmd = Command::new("python3");
        cmd.arg(self.cfg.repo.join("scripts/seed_genesis.py"))
            .arg("--genesis")
            .arg(&genesis)
            .arg("--seed")
            .arg(&seed)
            // The typed profile is written to a scenario-local seed copy. Keep
            // external contract artifacts anchored to the repository seed
            // directory instead of letting seed_genesis.py infer a nonexistent
            // `<scenario>/contracts` sibling.
            .arg("--contracts-dir")
            .arg(self.cfg.repo.join("scripts/contracts"))
            .arg("--validators")
            .arg(self.cfg.dir.join("validators.json"))
            .arg("--worldwide-day")
            .arg(worldwide_day)
            .arg("--fresh-metadosis")
            .arg("--output")
            .arg(&genesis);
        self.run_setup(&mut cmd, "seed_genesis.py")?;

        let seed: serde_json::Value = serde_json::from_str(&fs::read_to_string(seed)?)?;
        let genesis: serde_json::Value = serde_json::from_str(&fs::read_to_string(genesis)?)?;
        validate_radicle_registry_genesis(&seed, &genesis)
    }

    /// Lower the SlashIndicator felony thresholds so downtime slashing triggers
    /// within the short dev epoch (`bootstrap-testnet.sh:228-253`).
    fn patch_felony(&self, profile: &BootstrapProfile) -> Result<()> {
        let felony_threshold = profile.dev_felony_threshold;
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

    fn validate_effective_seed_capacity(&self, n: usize, profile: &BootstrapProfile) -> Result<()> {
        let seed: serde_json::Value = serde_json::from_str(&fs::read_to_string(&self.cfg.seed)?)?;
        let configured = profile.max_validators.or_else(|| {
            seed.pointer("/validator_set/max_validators")
                .and_then(json_u32)
        });
        if let Some(max) = configured {
            if max == 0 || max < n as u32 {
                bail!("effective max validators {max} cannot hold genesis committee size {n}");
            }
        }
        Ok(())
    }

    /// Create the seed consumed by `seed_genesis.py` without changing the
    /// caller-supplied seed file.
    fn prepare_scenario_seed(&self, profile: &BootstrapProfile) -> Result<std::path::PathBuf> {
        let mut seed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&self.cfg.seed)?)?;
        let root = seed
            .as_object_mut()
            .ok_or_else(|| eyre!("genesis seed root is not an object"))?;
        let validator_set = root
            .entry("validator_set")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| eyre!("genesis seed validator_set is not an object"))?;

        if let Some(index) = profile.validator_set_owner_index {
            let validators: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(self.cfg.dir.join("validators.json"))?)?;
            let owner = validators
                .as_array()
                .and_then(|entries| entries.get(index))
                .and_then(|entry| entry.get("address"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| eyre!("validator-{index} has no address in validators.json"))?;
            validator_set.insert("owner".into(), json!(owner));
        }
        if let Some(max) = profile.max_validators {
            validator_set.insert("max_validators".into(), json!(max));
        }
        if let Some(cooldown) = profile.reregistration_cooldown_blocks {
            validator_set.insert("reregistration_cooldown_blocks".into(), json!(cooldown));
        }

        if profile.unbonding_period_secs.is_some()
            || profile.slashed_withdrawal_delay_secs.is_some()
        {
            let staking = root
                .entry("staking")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| eyre!("genesis seed staking is not an object"))?;
            if let Some(value) = profile.unbonding_period_secs {
                staking.insert("unbonding_period".into(), json!(value.get()));
            }
            if let Some(value) = profile.slashed_withdrawal_delay_secs {
                staking.insert("slashed_withdrawal_delay".into(), json!(value.get()));
            }
        }

        let path = self.cfg.dir.join("scenario-seed.json");
        fs::write(&path, serde_json::to_string_pretty(&seed)? + "\n")?;
        Ok(path)
    }
}

fn read_rebootstrap_signing_key(path: &Path) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!(
            "unsafe test SGX signing key before localnet rebootstrap: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "test SGX signing key has unsafe permissions before localnet rebootstrap: {}",
            path.display()
        );
    }
    Ok(Some(fs::read(path)?))
}

fn restore_rebootstrap_signing_key(path: &Path, signing_key: Option<&[u8]>) -> Result<()> {
    let Some(signing_key) = signing_key else {
        return Ok(());
    };
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("test SGX signing key has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    file.write_all(signing_key)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(feature = "ocomp-integration"))]
fn publish_embedded_ocomp_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == bytes => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_file() {
                bail!(
                    "existing embedded OCOMP artifact is not a regular file: {}",
                    path.display()
                );
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o777 != mode {
                bail!(
                    "existing embedded OCOMP artifact has unsafe permissions: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Ok(_) => bail!(
            "refusing to replace a different embedded OCOMP artifact at {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(mode);
            #[cfg(not(unix))]
            let _ = mode;
            let mut file = options.open(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
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

/// Hardware E2E uses the testnet chain identity. `SgxNoAttest` keeps LocalNet
/// consensus/timing defaults and an explicit `GramineDirectDev` policy while
/// running the production enclave inside SGX without DCAP.
const fn localnet_chain_id(tee_mode: TeeMode) -> u64 {
    match tee_mode {
        TeeMode::Real | TeeMode::SgxNoAttest => TESTNET_CHAIN_ID,
        TeeMode::GramineDirect | TeeMode::Mock | TeeMode::MockNative => DEVNET_CHAIN_ID,
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

fn localnet_metadosis_offering_period_seconds(profile: &BootstrapProfile) -> u64 {
    profile.metadosis_offering_seconds
}

fn localnet_metadosis_forming_period_seconds(now: u64, profile: &BootstrapProfile) -> Result<u64> {
    match profile.metadosis_forming_seconds {
        Some(period) => Ok(period),
        None => localnet_forming_period_seconds(now),
    }
}

fn localnet_ocomp_vote_window_blocks(profile: &BootstrapProfile) -> u64 {
    profile.ocomp_vote_window_blocks
}

fn validator_balance_hex(profile: &BootstrapProfile) -> String {
    profile.validator_balance_coen.map_or_else(
        || VALIDATOR_BALANCE_HEX.to_owned(),
        |coen| format!("0x{:x}", u128::from(coen) * 1_000_000),
    )
}

fn json_u32(value: &serde_json::Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|raw| u32::try_from(raw).ok())
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u32>().ok()))
}

fn validate_radicle_registry_genesis(
    seed: &serde_json::Value,
    genesis: &serde_json::Value,
) -> Result<()> {
    let Some(configured) = seed.pointer("/radicle_registry/max_repositories") else {
        return Ok(());
    };
    let maximum = configured
        .as_u64()
        .or_else(|| {
            configured.as_str().and_then(|raw| {
                raw.strip_prefix("0x")
                    .map_or_else(|| raw.parse().ok(), |hex| u64::from_str_radix(hex, 16).ok())
            })
        })
        .and_then(|raw| u32::try_from(raw).ok())
        .filter(|maximum| *maximum != 0)
        .ok_or_else(|| eyre!("radicle_registry.max_repositories must be in 1..=4294967295"))?;

    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre!("generated genesis has no alloc object"))?;
    let account = alloc
        .iter()
        .find_map(|(address, account)| {
            let address = address.to_ascii_lowercase();
            let address = address.strip_prefix("0x").unwrap_or(&address);
            (address.len() <= 40
                && address.bytes().all(|byte| byte.is_ascii_hexdigit())
                && format!("{address:0>40}") == RADICLE_REGISTRY_ADDRESS)
                .then_some(account)
        })
        .ok_or_else(|| eyre!("generated genesis has no RadicleRegistry marker account"))?;
    eyre::ensure!(
        account.get("code").and_then(serde_json::Value::as_str) == Some("0xef"),
        "generated genesis has an invalid RadicleRegistry marker"
    );
    let actual = account
        .pointer(&format!("/storage/{RADICLE_MAX_REPOSITORIES_SLOT}"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre!("generated genesis has no RadicleRegistry maxRepositories slot"))?;
    let expected = format!("0x{maximum:064x}");
    eyre::ensure!(
        actual.eq_ignore_ascii_case(&expected),
        "generated genesis RadicleRegistry maxRepositories mismatch: expected {expected}, got {actual}"
    );
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

fn localnet_forming_period_seconds(now: u64) -> Result<u64> {
    let current_wwd = outbe_primitives::time::worldwide_day_from_timestamp(now);
    let current_wwd_start = outbe_primitives::time::date_key_to_utc_timestamp(current_wwd)
        .checked_sub(outbe_primitives::time::UTC_PLUS_14_OFFSET)
        .ok_or_else(|| eyre!("cannot derive LocalNet WorldwideDay start"))?;
    now.checked_sub(current_wwd_start)
        .and_then(|elapsed| elapsed.checked_add(LOCALNET_METADOSIS_FORMING_LEAD_SECONDS))
        .ok_or_else(|| eyre!("cannot derive LocalNet Metadosis forming period"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radicle_registry_genesis() {
        let seed = json!({ "radicle_registry": { "max_repositories": "0x25" } });
        let genesis = json!({
            "alloc": {
                "0x000000000000000000000000000000000000EE11": {
                    "code": "0xef",
                    "storage": {
                        RADICLE_MAX_REPOSITORIES_SLOT:
                            "0x0000000000000000000000000000000000000000000000000000000000000025"
                    }
                }
            }
        });

        validate_radicle_registry_genesis(&seed, &genesis).unwrap();
        validate_radicle_registry_genesis(&json!({}), &json!({})).unwrap();

        let mut wrong = genesis;
        wrong["alloc"]["0x000000000000000000000000000000000000EE11"]["storage"]
            [RADICLE_MAX_REPOSITORIES_SLOT] = json!("0x01");
        assert!(validate_radicle_registry_genesis(&seed, &wrong)
            .unwrap_err()
            .to_string()
            .contains("maxRepositories mismatch"));
    }

    #[test]
    fn rebootstrap_preserves_the_scenario_signing_key_across_wipe() {
        let directory = tempfile::tempdir().unwrap();
        let scenario = directory.path().join("scenario-1");
        let signing_key_path = scenario.join("test-sgx-signing-key.pem");
        fs::create_dir_all(&scenario).unwrap();
        fs::write(&signing_key_path, b"scenario signing key").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&signing_key_path, Permissions::from_mode(0o600)).unwrap();

        let signing_key = read_rebootstrap_signing_key(&signing_key_path).unwrap();
        fs::remove_dir_all(&scenario).unwrap();
        restore_rebootstrap_signing_key(&signing_key_path, signing_key.as_deref()).unwrap();

        assert_eq!(
            fs::read(&signing_key_path).unwrap(),
            b"scenario signing key"
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&signing_key_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(not(feature = "ocomp-integration"))]
    #[test]
    fn default_bootstrap_stages_embedded_ocomp_domain_material() {
        let directory = tempfile::tempdir().unwrap();
        let env = crate::env::Environment {
            data_dir: directory.path().to_path_buf(),
            validators: 2,
            ..crate::env::Environment::default()
        };
        env.ports.start_scenario(env.validators).unwrap();
        let localnet = Localnet::new(crate::internal::config::Config::for_scenario(&env, 1));
        fs::create_dir_all(&localnet.cfg.dir).unwrap();
        fs::write(
            localnet.cfg.dir.join("protocol-bundle-v1.ocb1"),
            b"canonical-bundle",
        )
        .unwrap();
        for index in 0..env.validators {
            let validator_dir = localnet.cfg.validator_dir(index);
            fs::create_dir_all(&validator_dir).unwrap();
            fs::write(
                validator_dir.join("ocomp-key-v1.hex"),
                format!("{}\n", hex::encode([index as u8 + 1; 32])),
            )
            .unwrap();
        }

        localnet.ensure_embedded_ocomp_domain_material().unwrap();
        localnet.ensure_embedded_ocomp_domain_material().unwrap();

        let joiner_index = env.validators;
        let joiner_dir = localnet.cfg.validator_dir(joiner_index);
        fs::create_dir_all(&joiner_dir).unwrap();
        fs::write(
            joiner_dir.join("ocomp-key-v1.hex"),
            b"joiner-registration-secret\n",
        )
        .unwrap();
        localnet
            .ensure_embedded_ocomp_validator_domain_material(joiner_index)
            .unwrap();
        localnet
            .ensure_embedded_ocomp_validator_domain_material(joiner_index)
            .unwrap();

        for index in 0..env.validators {
            let validator_dir = localnet.cfg.validator_dir(index);
            let domain = validator_dir.join("ocomp").join("domain-v1");
            assert_eq!(
                fs::read(domain.join("protocol-bundle-v1.ocb1")).unwrap(),
                b"canonical-bundle"
            );
            assert_eq!(
                fs::read(domain.join("ocomp-key-v1.hex")).unwrap(),
                fs::read(validator_dir.join("ocomp-key-v1.hex")).unwrap()
            );
            assert_eq!(
                fs::read_to_string(domain.join("ocomp-evm-key.hex")).unwrap(),
                format!("{}\n", hex::encode([index as u8 + 0x71; 32]))
            );
            #[cfg(unix)]
            assert_eq!(
                fs::metadata(domain.join("ocomp-evm-key.hex"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let joiner_domain = joiner_dir.join("ocomp").join("domain-v1");
        assert_eq!(
            fs::read(joiner_domain.join("protocol-bundle-v1.ocb1")).unwrap(),
            b"canonical-bundle"
        );
        assert_eq!(
            fs::read(joiner_domain.join("ocomp-key-v1.hex")).unwrap(),
            b"joiner-registration-secret\n"
        );
        assert_eq!(
            fs::read_to_string(joiner_domain.join("ocomp-evm-key.hex")).unwrap(),
            format!("{}\n", hex::encode([joiner_index as u8 + 0x71; 32]))
        );
    }

    #[test]
    fn localnet_forming_edge_tracks_the_canonical_utc_plus_14_day() {
        let utc_midnight = outbe_primitives::time::date_key_to_utc_timestamp(20260302);

        for now in [
            utc_midnight + 9 * 3_600 + 59 * 60,
            utc_midnight + 10 * 3_600,
        ] {
            let wwd = outbe_primitives::time::worldwide_day_from_timestamp(now);
            let wwd_start = outbe_primitives::time::date_key_to_utc_timestamp(wwd)
                - outbe_primitives::time::UTC_PLUS_14_OFFSET;
            let period = localnet_forming_period_seconds(now).unwrap();
            assert_eq!(
                wwd_start + period,
                now + LOCALNET_METADOSIS_FORMING_LEAD_SECONDS
            );
        }
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
    fn co_located_sgx_gets_wider_consensus_windows() {
        let mut real = json!({ "config": {} });
        apply_co_located_sgx_timing(&mut real, crate::env::TeeMode::Real).unwrap();
        assert_eq!(real["config"]["leaderTimeoutMs"], json!(15_000));
        assert_eq!(real["config"]["certificationTimeoutMs"], json!(30_000));

        let mut no_attest = json!({ "config": {} });
        apply_co_located_sgx_timing(&mut no_attest, crate::env::TeeMode::SgxNoAttest).unwrap();
        assert_eq!(no_attest["config"]["leaderTimeoutMs"], json!(15_000));
        assert_eq!(no_attest["config"]["certificationTimeoutMs"], json!(30_000));

        let mut mock = json!({ "config": {} });
        apply_co_located_sgx_timing(&mut mock, crate::env::TeeMode::Mock).unwrap();
        assert!(mock["config"].as_object().unwrap().is_empty());
    }

    #[test]
    fn sgx_no_attest_uses_testnet_identity_while_dev_transports_use_devnet() {
        assert_eq!(localnet_chain_id(TeeMode::Real), TESTNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::SgxNoAttest), TESTNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::GramineDirect), DEVNET_CHAIN_ID);
        assert_eq!(localnet_chain_id(TeeMode::Mock), DEVNET_CHAIN_ID);
    }

    #[test]
    fn validator_liquid_balance_can_be_tuned_per_scenario() {
        assert_eq!(
            validator_balance_hex(&BootstrapProfile::default()),
            VALIDATOR_BALANCE_HEX
        );

        let tuned_profile = BootstrapProfile::from_tuning(&[(
            "TESTNET_VALIDATOR_BALANCE_COEN",
            "2100000".to_owned(),
        )])
        .unwrap();
        let tuned = validator_balance_hex(&tuned_profile);
        assert_eq!(
            u128::from_str_radix(tuned.trim_start_matches("0x"), 16).unwrap(),
            2_100_000u128 * 1_000_000
        );
    }

    #[test]
    fn metadosis_offering_period_can_be_tuned_only_in_generated_genesis() {
        let default = BootstrapProfile::default();
        assert_eq!(
            localnet_metadosis_offering_period_seconds(&default),
            LOCALNET_METADOSIS_OFFERING_SECONDS
        );
        let tuned = BootstrapProfile::from_tuning(&[(
            "TESTNET_METADOSIS_OFFERING_SECONDS",
            "900".to_owned(),
        )])
        .unwrap();
        assert_eq!(localnet_metadosis_offering_period_seconds(&tuned), 900);
    }

    #[test]
    fn metadosis_forming_period_can_be_tuned_only_in_generated_genesis() {
        let now = outbe_primitives::time::date_key_to_utc_timestamp(20260302) + 12 * 3_600;
        let default = BootstrapProfile::default();
        assert_eq!(
            localnet_metadosis_forming_period_seconds(now, &default).unwrap(),
            localnet_forming_period_seconds(now).unwrap()
        );
        let tuned = BootstrapProfile::from_tuning(&[(
            "TESTNET_METADOSIS_FORMING_SECONDS",
            "136860".to_owned(),
        )])
        .unwrap();
        assert_eq!(
            localnet_metadosis_forming_period_seconds(now, &tuned).unwrap(),
            136_860
        );
    }

    #[test]
    fn ocomp_vote_window_can_be_tuned_only_in_generated_genesis() {
        let default = BootstrapProfile::default();
        assert_eq!(
            localnet_ocomp_vote_window_blocks(&default),
            LOCALNET_OCOMP_VOTE_WINDOW_BLOCKS
        );
        let tuned = BootstrapProfile::from_tuning(&[(
            "TESTNET_OCOMP_VOTE_WINDOW_BLOCKS",
            "450".to_owned(),
        )])
        .unwrap();
        assert_eq!(localnet_ocomp_vote_window_blocks(&tuned), 450);
    }

    #[test]
    fn profile_rejects_invalid_values_before_bootstrap() {
        assert!(BootstrapProfile::default().with_chain_id(0).is_err());
        assert!(BootstrapProfile::default()
            .with_dkg_timing(60, 0, 30)
            .is_err());
        assert!(BootstrapProfile::default()
            .with_dkg_timing(60, 61, 30)
            .is_err());
        assert!(BootstrapProfile::default()
            .with_staking_timing(Some(0), None)
            .is_err());
        assert!(BootstrapProfile::default().with_max_validators(0).is_err());
    }

    #[test]
    fn profile_checks_committee_dependent_bounds() {
        let owner_outside = BootstrapProfile::default().with_validator_set_owner(4);
        assert!(owner_outside.validate_for_committee(4).is_err());

        let too_small = BootstrapProfile::default().with_max_validators(3).unwrap();
        assert!(too_small.validate_for_committee(4).is_err());

        let valid = BootstrapProfile::default()
            .with_validator_set_owner(0)
            .with_max_validators(5)
            .unwrap()
            .with_reregistration_cooldown_blocks(0);
        valid.validate_for_committee(4).unwrap();
    }

    #[test]
    fn legacy_tuning_is_strict_and_maps_to_typed_profile() {
        let profile = BootstrapProfile::from_tuning(&[
            ("TESTNET_EPOCH_LENGTH_BLOCKS", "60".to_owned()),
            ("TESTNET_DKG_PREPARE_WINDOW_BLOCKS", "15".to_owned()),
            ("TESTNET_DKG_ACTIVATION_GRACE_BLOCKS", "20".to_owned()),
            ("TESTNET_DEV_FELONY_THRESHOLD", "59".to_owned()),
            ("TESTNET_UNBONDING_PERIOD_SECS", "8".to_owned()),
        ])
        .unwrap();
        assert_eq!(profile.epoch_length_blocks.get(), 60);
        assert_eq!(profile.dkg_prepare_window_blocks.get(), 15);
        assert_eq!(profile.unbonding_period_secs.unwrap().get(), 8);

        assert!(BootstrapProfile::from_tuning(&[(
            "TESTNET_EPOCH_LENGTH_BLOCKS",
            "not-a-number".to_owned(),
        )])
        .is_err());
        assert!(BootstrapProfile::from_tuning(&[("TESTNET_UNKNOWN", "1".to_owned(),)]).is_err());
    }
}
