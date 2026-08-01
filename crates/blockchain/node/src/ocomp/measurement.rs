//! Devnet-only Measurement fork-install generation and genesis arming.
//!
//! Every localnet/devnet flow (shell bootstrap, the e2e harness, smoke) needs a
//! genesis whose `config` carries the `ocompForkInstallV1` and
//! `metadosisStorageLayoutV1` manifests required by
//! [`super::fork::require_startup_ocomp_fork_install`], plus the scheduled
//! protocol-v1 update that activates the Metadosis begin-zone at the
//! Measurement height. This module is the single source of that logic.
//!
//! **Devnet-only.** The result committee is signed with deterministic fixture
//! scalars (`[index + 1; 32]`) and the protocol bundle is a provisional
//! placeholder; the `Measurement` classification expresses exactly that trust
//! model. Nothing here may be used to arm a production (`Final`) genesis —
//! that path lives in `xtask ocomp final-artifacts` with real generated
//! semantic digests and externally supplied key registrations.
//!
//! All outputs are deterministic: no wall clock, no randomness (ECDSA is
//! RFC-6979 with low-S normalization).

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::Arc;

use alloy_primitives::{keccak256, Address, B256, U256};
use eyre::Result;
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_metadosis::config::{
    OcompForkInstallClassification, OcompForkInstallV1, OcompRequestProfile,
};
use outbe_metadosis::proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH;
use outbe_ocomp_protocol::{
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    hash::hash_framed,
    profile::{poc_schema_limits, CapacityProfileV1, ProtocolBundleV1},
    registry::{
        HashDomain, FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID,
    },
    SchemaLimits,
};
use outbe_primitives::{
    addresses::UPDATE_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    OutbeHeader,
};
use outbe_update::{schema::Update, ProtocolVersion};
use reth_chainspec::ChainSpec;

use super::fork::{
    require_startup_ocomp_fork_install, GENESIS_ACTIVE_OCOMP_HEIGHT,
    METADOSIS_STORAGE_LAYOUT_GENESIS_KEY, OCOMP_FORK_INSTALL_GENESIS_KEY,
};

/// Options for [`arm_genesis`].
#[derive(Clone, Debug)]
pub struct ArmOptions {
    /// Measurement activation height; the startup gate requires
    /// [`GENESIS_ACTIVE_OCOMP_HEIGHT`] for fresh devnets.
    pub activation_height: u64,
    /// When set, per-validator OCOMP supervisor domain material
    /// (`protocol-bundle-v1.ocb1`, `ocomp-key-v1.hex`, `ocomp-evm-key.hex`) is
    /// published into each listed domain root. Only needed when `outbe-ocomp`
    /// supervisor processes will run; the chain node itself boots without it.
    pub domain_material_roots: Option<Vec<PathBuf>>,
}

impl Default for ArmOptions {
    fn default() -> Self {
        Self {
            activation_height: GENESIS_ACTIVE_OCOMP_HEIGHT,
            domain_material_roots: None,
        }
    }
}

/// Outcome of one [`arm_genesis`] run.
#[derive(Clone, Debug)]
pub struct ArmedGenesis {
    pub install: Arc<OcompForkInstallV1>,
    pub install_hash: B256,
    pub genesis_hash: B256,
    pub chain_id: u64,
}

/// Arms one devnet genesis in place so the node startup gate accepts it.
///
/// Order matters: the protocol-v1 update schedule mutates `alloc` storage and
/// therefore the genesis hash, so it is written **before** the hash the fork
/// install binds is computed. The two `config` manifests are additive-only and
/// asserted not to change the hash; re-arming an already-armed genesis with
/// identical content is a no-op, while a differing manifest is a hard error.
pub fn arm_genesis(
    genesis_path: &Path,
    validators_path: &Path,
    options: &ArmOptions,
) -> Result<ArmedGenesis> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&fs::read(genesis_path)?)?;
    let chain_id = genesis_chain_id(&genesis)?;

    if schedule_protocol_v1_update(&mut genesis, chain_id, options.activation_height)? {
        replace_json_atomically(genesis_path, &genesis)?;
    }

    let base_spec = parse_outbe_chain_spec(genesis_path)?;
    let base_genesis_hash = base_spec.genesis_hash();
    let identities = validator_identities(validators_path)?;
    let limits = poc_schema_limits();
    let install = measurement_fork_install(
        chain_id,
        base_genesis_hash,
        options.activation_height,
        identities,
        &limits,
    )?;
    install.validate_for_chain(chain_id, base_genesis_hash, &limits)?;
    let canonical_install = install.encode_canonical(&limits)?;
    let install_hash = install.install_hash(&limits)?;

    if let Some(domain_roots) = &options.domain_material_roots {
        write_validator_domain_material(domain_roots, &install)?;
    }

    let config = genesis
        .get_mut("config")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("genesis config is not an object"))?;
    let manifest = serde_json::json!({
        "canonicalBytes": format!("0x{}", hex::encode(&canonical_install)),
        "installHash": install_hash,
    });
    let mut manifest_changed = false;
    match config.get(OCOMP_FORK_INSTALL_GENESIS_KEY) {
        Some(existing) if existing == &manifest => {}
        Some(_) => eyre::bail!("refusing to replace a different OCOMP fork install"),
        None => {
            config.insert(OCOMP_FORK_INSTALL_GENESIS_KEY.to_owned(), manifest);
            manifest_changed = true;
        }
    }
    let layout_manifest = serde_json::json!({
        "layoutHash": METADOSIS_STORAGE_LAYOUT_V1_HASH,
    });
    match config.get(METADOSIS_STORAGE_LAYOUT_GENESIS_KEY) {
        Some(existing) if existing == &layout_manifest => {}
        Some(_) => eyre::bail!("refusing to replace a different Metadosis storage layout"),
        None => {
            config.insert(
                METADOSIS_STORAGE_LAYOUT_GENESIS_KEY.to_owned(),
                layout_manifest,
            );
            manifest_changed = true;
        }
    }
    if manifest_changed {
        replace_json_atomically(genesis_path, &genesis)?;
    }

    let armed_spec = parse_outbe_chain_spec(genesis_path)?;
    if armed_spec.genesis_hash() != base_genesis_hash {
        eyre::bail!("OCOMP genesis config extension changed the base genesis hash");
    }
    let loaded = require_startup_ocomp_fork_install(&armed_spec)?;
    if loaded.as_ref() != &install {
        eyre::bail!("node loader returned a different OCOMP fork install");
    }

    Ok(ArmedGenesis {
        install: loaded,
        install_hash,
        genesis_hash: base_genesis_hash,
        chain_id,
    })
}

/// Parses a genesis file into the node's chain spec.
pub fn parse_outbe_chain_spec(path: &Path) -> Result<Arc<ChainSpec<OutbeHeader>>> {
    let path = path
        .to_str()
        .ok_or_else(|| eyre::eyre!("genesis path is not valid UTF-8"))?;
    Ok(reth_ethereum::cli::chainspec::chain_value_parser(path)?
        .as_ref()
        .clone()
        .map_header(OutbeHeader::new)
        .into())
}

/// Reads `config.chainId` from a raw genesis document (number or hex string).
pub fn genesis_chain_id(genesis: &serde_json::Value) -> Result<u64> {
    let value = genesis
        .get("config")
        .and_then(|config| config.get("chainId"))
        .ok_or_else(|| eyre::eyre!("genesis has no config.chainId"))?;
    match value {
        serde_json::Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| eyre::eyre!("genesis chainId is outside u64")),
        serde_json::Value::String(encoded) => {
            let encoded = encoded.strip_prefix("0x").unwrap_or(encoded);
            u64::from_str_radix(encoded, 16).map_err(Into::into)
        }
        _ => eyre::bail!("genesis chainId is neither a number nor a hex string"),
    }
}

/// Derives the four framed validator-identity hashes from `validators.json`
/// (the `outbe-chain dkg bootstrap` output: `address` + 48-byte BLS
/// `public_key` per entry, in stable sorted order).
pub fn validator_identities(path: &Path) -> Result<[B256; 4]> {
    let validators: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let validators = validators
        .as_array()
        .ok_or_else(|| eyre::eyre!("validators.json is not an array"))?;
    if validators.len() != 4 {
        eyre::bail!(
            "OCOMP measurement requires exactly four validators, got {}",
            validators.len()
        );
    }

    let mut identities = [B256::ZERO; 4];
    for (index, validator) in validators.iter().enumerate() {
        let address = validator
            .get("address")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no address"))
            .and_then(|address| Address::from_str(address).map_err(Into::into))?;
        let public_key = validator
            .get("public_key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no public_key"))?;
        let public_key = hex::decode(public_key.strip_prefix("0x").unwrap_or(public_key))?;
        if public_key.len() != 48 {
            eyre::bail!(
                "validator-{index} consensus public key is {} bytes, expected 48",
                public_key.len()
            );
        }
        let mut payload = Vec::with_capacity(1 + 20 + 4 + public_key.len());
        payload.push(u8::try_from(index)?);
        payload.extend_from_slice(address.as_slice());
        payload.extend_from_slice(&u32::try_from(public_key.len())?.to_be_bytes());
        payload.extend_from_slice(&public_key);
        identities[index] = hash_framed(HashDomain::ValidatorIdentity, &payload)?;
        if identities[index].is_zero() {
            eyre::bail!("validator-{index} identity hash is zero");
        }
    }
    Ok(identities)
}

/// Builds the complete Measurement fork install bound to one chain identity.
pub fn measurement_fork_install(
    chain_id: u64,
    genesis_hash: B256,
    activation_height: u64,
    validator_identities: [B256; 4],
    limits: &SchemaLimits,
) -> Result<OcompForkInstallV1> {
    let protocol_bundle = provisional_measurement_bundle();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(limits)?;
    let result_committee = measurement_committee(
        chain_id,
        genesis_hash,
        protocol_bundle.fork_id,
        protocol_bundle_hash,
        activation_height,
        validator_identities,
        limits,
    )?;
    let result_committee_snapshot_hash = result_committee.snapshot_hash(limits)?;
    Ok(OcompForkInstallV1 {
        classification: OcompForkInstallClassification::Measurement,
        activation_height,
        request_profile: OcompRequestProfile {
            chain_id,
            genesis_hash,
            fork_id: protocol_bundle.fork_id,
            protocol_bundle_hash,
            correctness_profile_id: protocol_bundle.correctness_profile_id,
            capacity_profile: provisional_measurement_capacity_profile(),
            source_availability_policy_id: B256::repeat_byte(44),
            result_committee_snapshot_hash,
        },
        protocol_bundle,
        result_committee,
    })
}

/// Builds the q=3-of-4 measurement result committee with fixture signing keys.
pub fn measurement_committee(
    chain_id: u64,
    genesis_hash: B256,
    fork_id: B256,
    protocol_bundle_hash: B256,
    activation_height: u64,
    validator_identities: [B256; 4],
    limits: &SchemaLimits,
) -> Result<OcompCommitteeSnapshotV1> {
    let mut ordered_members = Vec::with_capacity(4);
    for (validator_index, validator_identity_hash) in validator_identities.into_iter().enumerate() {
        let validator_index = u8::try_from(validator_index)?;
        let key = measurement_signing_key(validator_index)?;
        let public_key: [u8; 33] = key
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()?;
        let mut registration = OcompKeyRegistrationV1 {
            core: OcompKeyRegistrationCoreV1 {
                chain_id,
                genesis_hash,
                fork_id,
                protocol_bundle_hash,
                validator_index,
                validator_identity_hash,
                ocomp_public_key_sec1: public_key,
                key_epoch: 1,
                allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
                valid_from_height: activation_height,
                valid_until_height_exclusive: activation_height.saturating_add(1_000_000),
            },
            proof_of_possession: [0; 64],
        };
        registration.proof_of_possession =
            sign_measurement_digest(&key, registration.proof_of_possession_digest(limits)?)?;
        ordered_members.push(OcompMemberV1 {
            validator_index,
            validator_identity_hash,
            ocomp_public_key_sec1: public_key,
            key_epoch: registration.core.key_epoch,
            allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
            valid_from_height: registration.core.valid_from_height,
            valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
            proof_of_possession: registration.proof_of_possession,
        });
    }
    Ok(OcompCommitteeSnapshotV1 {
        chain_id,
        genesis_hash,
        fork_id,
        protocol_bundle_hash,
        snapshot_epoch: 1,
        threshold: 3,
        ordered_members,
    })
}

/// Deterministic fixture result-signing key for one measurement validator.
/// Devnet-only by construction: the scalar is public knowledge.
pub fn measurement_signing_key(validator_index: u8) -> Result<SigningKey> {
    SigningKey::from_bytes((&[validator_index.saturating_add(1); 32]).into())
        .map_err(|error| eyre::eyre!("invalid deterministic measurement scalar: {error}"))
}

/// Deterministic fixture EVM key for one measurement validator's OCOMP signer.
pub fn ocomp_evm_private_key(validator_index: u8) -> String {
    format!(
        "0x{}",
        hex::encode([validator_index.saturating_add(0x71); 32])
    )
}

/// Low-S canonical RFC-6979 signature over one prehashed digest.
pub fn sign_measurement_digest(key: &SigningKey, digest: B256) -> Result<[u8; 64]> {
    let signature: Signature = key.sign_prehash(digest.as_slice())?;
    Ok(signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into())
}

/// Provisional placeholder protocol bundle for the Measurement profile. Only
/// the three input codec ids are real registry constants.
pub fn provisional_measurement_bundle() -> ProtocolBundleV1 {
    let hash = |byte| B256::repeat_byte(byte);
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: hash(1),
        intent_codec_id: hash(2),
        finalized_intent_proof_codec_id: hash(3),
        tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
        result_codec_id: hash(4),
        action_codec_id: hash(5),
        activation_codec_id: hash(6),
        evidence_codec_id: hash(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: hash(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: hash(9),
        effect_contract_registry_hash: hash(10),
        object_codec_registry_hash: hash(11),
        correctness_profile_id: hash(12),
        capacity_profile_id: hash(13),
        result_signature_profile_id: hash(14),
        finality_verifier_and_vote_domain_id: hash(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: hash(16),
        mode_pause_revocation_semantics_hash: hash(17),
        upgrade_fsm_semantics_hash: hash(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: hash(19),
        release_requirement_catalog_parent_hash: hash(20),
        release_gate_authority_envelope_hash: hash(21),
        release_approval_policy_hash: hash(22),
        release_validator_command_artifact_hash: hash(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: hash(24),
        required_upgrade_handler_set_hash: hash(25),
    }
}

/// Provisional Measurement capacity ceilings (mirrors the frozen PoC shape).
pub fn provisional_measurement_capacity_profile() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(13),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_pending_jobs: 2,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        retry_backoff_blocks: 1,
        max_terminal_job_records: 365,
        max_reference_currencies: 256,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: 64,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: B256::repeat_byte(23),
    }
}

/// Writes the protocol-v1 scheduled update into the `Update` genesis account so
/// the Metadosis begin-zone branch is active from `activation_height`. Creates
/// the alloc container when the seeded genesis omitted it; a conflicting
/// pre-existing slot value is a hard error. Returns whether the document
/// changed.
pub fn schedule_protocol_v1_update(
    genesis: &mut serde_json::Value,
    chain_id: u64,
    activation_height: u64,
) -> Result<bool> {
    let mut provider = HashMapStorageProvider::new(chain_id);
    StorageHandle::enter(&mut provider, |storage| {
        Update::new(storage).write_scheduled_update(
            measurement_update_proposal_id(),
            ProtocolVersion::from_raw(1),
            activation_height,
            "OCOMP PoC measurement profile",
        )
    })?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("genesis has no alloc object"))?;
    let mut changed = false;
    let update_key = match find_alloc_address_key(alloc, UPDATE_ADDRESS)? {
        Some(key) => key,
        None => {
            // Native precompiles do not need bytecode accounts. A fresh seeded
            // genesis may therefore omit Update until its first non-zero
            // storage word; create only that canonical alloc container.
            let key = hex::encode(UPDATE_ADDRESS.as_slice());
            alloc.insert(
                key.clone(),
                serde_json::json!({
                    "balance": "0x0",
                    "storage": {},
                }),
            );
            changed = true;
            key
        }
    };
    let update_account = alloc
        .get_mut(&update_key)
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("Update genesis account is not an object"))?;
    let storage = update_account
        .entry("storage")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| eyre::eyre!("Update genesis storage is not an object"))?;

    for ((address, slot), value) in &provider.storage {
        if *address != UPDATE_ADDRESS || value.is_zero() {
            continue;
        }
        let slot = format!("0x{slot:064x}");
        let value = format!("0x{value:064x}");
        match storage.get(&slot) {
            Some(existing) if parse_storage_word(existing)? == parse_hex_word(&value)? => {}
            Some(_) => eyre::bail!("Update genesis slot {slot} conflicts with OCOMP schedule"),
            None => {
                storage.insert(slot, serde_json::Value::String(value));
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// Proposal id of the devnet measurement protocol-v1 schedule.
pub fn measurement_update_proposal_id() -> U256 {
    U256::from_be_bytes(keccak256(b"OUTBE_OCOMP_MEASUREMENT_UPDATE_PROPOSAL_V1").0)
}

/// Publishes per-validator OCOMP supervisor domain material into each domain
/// root (`protocol-bundle-v1.ocb1`, `ocomp-key-v1.hex`, `ocomp-evm-key.hex`).
/// Write-once: identical existing content is accepted, differing content is a
/// hard error.
pub fn write_validator_domain_material(
    domain_roots: &[PathBuf],
    install: &OcompForkInstallV1,
) -> Result<()> {
    let limits = poc_schema_limits();
    let canonical_bundle = install.protocol_bundle.encode_canonical(&limits)?;
    for (validator_index, root) in domain_roots.iter().enumerate() {
        fs::create_dir_all(root)?;
        publish_exact_file(
            &root.join("protocol-bundle-v1.ocb1"),
            &canonical_bundle,
            0o640,
        )?;
        let key = measurement_signing_key(u8::try_from(validator_index)?)?;
        let key_bytes = format!("{}\n", hex::encode(key.to_bytes()));
        publish_exact_file(&root.join("ocomp-key-v1.hex"), key_bytes.as_bytes(), 0o600)?;
        let evm_key = ocomp_evm_private_key(u8::try_from(validator_index)?);
        publish_exact_file(
            &root.join("ocomp-evm-key.hex"),
            format!("{evm_key}\n").as_bytes(),
            0o600,
        )?;
    }
    Ok(())
}

/// Write-once file publisher with mode enforcement: identical existing content
/// with the exact mode is a no-op; anything else is a hard error.
pub fn publish_exact_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == bytes => {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o777 != mode {
                eyre::bail!(
                    "existing OCOMP artifact has unsafe metadata: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Ok(_) => eyre::bail!(
            "refusing to replace a different OCOMP artifact at {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Atomically replaces one JSON document (temp file + rename + parent fsync).
pub fn replace_json_atomically(path: &Path, value: &serde_json::Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("genesis has no parent directory"))?;
    let temporary = parent.join(format!(".genesis.ocomp.{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o640)
        .open(&temporary)?;
    if let Err(error) = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })() {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

fn find_alloc_address_key(
    alloc: &serde_json::Map<String, serde_json::Value>,
    expected: Address,
) -> Result<Option<String>> {
    for key in alloc.keys() {
        let normalized = if key.starts_with("0x") {
            key.clone()
        } else {
            format!("0x{key}")
        };
        if Address::from_str(&normalized)
            .map_err(|error| eyre::eyre!("invalid genesis alloc address {key}: {error}"))?
            == expected
        {
            return Ok(Some(key.clone()));
        }
    }
    Ok(None)
}

fn parse_storage_word(value: &serde_json::Value) -> Result<U256> {
    let encoded = value
        .as_str()
        .ok_or_else(|| eyre::eyre!("genesis storage word is not a string"))?;
    parse_hex_word(encoded)
}

fn parse_hex_word(encoded: &str) -> Result<U256> {
    U256::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16).map_err(Into::into)
}
