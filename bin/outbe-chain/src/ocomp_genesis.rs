use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    str::FromStr as _,
};

use alloy_primitives::{Address, B256};
use clap::{Parser, Subcommand};
use eyre::{ensure, WrapErr as _};
use outbe_metadosis::{
    config::{OcompForkInstallClassification, OcompForkInstallV1, OcompRequestProfile},
    proof_layout::METADOSIS_STORAGE_LAYOUT_V1_HASH,
};
use outbe_ocomp_protocol::{
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationV1, OcompMemberV1, POC_COMMITTEE_SIZE,
        POC_COMMITTEE_THRESHOLD, POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    hash::hash_framed,
    profile::{CapacityProfileV1, ProtocolBundleV1},
    registry::{
        HashDomain, FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID,
    },
};
use outbe_primitives::OutbeHeader;
use reth_chainspec::ChainSpec;
use reth_ethereum::chainspec::EthChainSpec as _;
use serde::Serialize;

const ACTIVATION_HEIGHT: u64 = outbe_node::ocomp::fork::GENESIS_ACTIVE_OCOMP_HEIGHT;
const OCOMP_FIELD: &str = outbe_node::ocomp::fork::OCOMP_FORK_INSTALL_GENESIS_KEY;
const LAYOUT_FIELD: &str = outbe_node::ocomp::fork::METADOSIS_STORAGE_LAYOUT_GENESIS_KEY;

#[derive(Debug, Parser)]
#[command(name = "outbe-chain-ocomp")]
struct OcompCli {
    #[command(subcommand)]
    command: OcompCommand,
}

#[derive(Debug, Subcommand)]
enum OcompCommand {
    /// Derive the exact chain bindings needed to generate four OCOMP registrations.
    Bindings(OcompBindingsArgs),
    /// Install a block-1 Measurement OCOMP profile using four real registrations.
    Genesis(OcompGenesisArgs),
}

#[derive(Debug, clap::Args)]
struct OcompBindingsArgs {
    /// Seeded genesis JSON without OCOMP or TEE extensions.
    #[arg(long)]
    input: PathBuf,
    /// Exact four-validator public bootstrap manifest.
    #[arg(long)]
    validators: PathBuf,
    /// New JSON document consumed by network preparation tooling.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, clap::Args)]
struct OcompGenesisArgs {
    /// Seeded genesis JSON without OCOMP or TEE extensions.
    #[arg(long)]
    input: PathBuf,
    /// Exact four-validator public bootstrap manifest.
    #[arg(long)]
    validators: PathBuf,
    /// Directory containing validator-N/ocomp-registration-v1.ocb1.
    #[arg(long)]
    registrations_dir: PathBuf,
    /// New OCOMP-armed genesis JSON.
    #[arg(long)]
    output: PathBuf,
    /// New canonical protocol-bundle-v1.ocb1 file.
    #[arg(long)]
    protocol_bundle_output: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OcompBindingsDocumentV1 {
    schema_version: u16,
    chain_id: u64,
    genesis_hash: B256,
    fork_id: B256,
    protocol_bundle_hash: B256,
    activation_height: u64,
    valid_until_height_exclusive: u64,
    validator_identity_hashes: [B256; POC_COMMITTEE_SIZE],
}

pub(crate) fn run(arguments: &[String]) -> eyre::Result<()> {
    let mut ocomp_arguments = vec![arguments[0].clone()];
    ocomp_arguments.extend_from_slice(&arguments[2..]);
    match OcompCli::parse_from(ocomp_arguments).command {
        OcompCommand::Bindings(args) => write_bindings(&args),
        OcompCommand::Genesis(args) => generate_genesis(&args),
    }
}

fn write_bindings(args: &OcompBindingsArgs) -> eyre::Result<()> {
    require_new_output(&args.output, "OCOMP bindings")?;
    let (chain_id, genesis_hash) = parse_base_identity(&args.input)?;
    let validator_identity_hashes = validator_identities(&args.validators)?;
    let protocol_bundle = measurement_protocol_bundle();
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let document = OcompBindingsDocumentV1 {
        schema_version: 1,
        chain_id,
        genesis_hash,
        fork_id: protocol_bundle.fork_id,
        protocol_bundle_hash: protocol_bundle.protocol_bundle_hash(&limits)?,
        activation_height: ACTIVATION_HEIGHT,
        valid_until_height_exclusive: u64::MAX,
        validator_identity_hashes,
    };
    write_new_file(&args.output, &pretty_json(&document)?, "OCOMP bindings")?;
    println!(
        "wrote OCOMP registration bindings: {}",
        args.output.display()
    );
    Ok(())
}

fn generate_genesis(args: &OcompGenesisArgs) -> eyre::Result<()> {
    ensure!(
        args.input != args.output,
        "--input and --output must differ"
    );
    require_new_output(&args.output, "OCOMP genesis")?;
    require_new_output(&args.protocol_bundle_output, "OCOMP protocol bundle")?;

    let (chain_id, genesis_hash) = parse_base_identity(&args.input)?;
    let validator_identity_hashes = validator_identities(&args.validators)?;
    let protocol_bundle = measurement_protocol_bundle();
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let protocol_bundle_hash = protocol_bundle.protocol_bundle_hash(&limits)?;
    let registrations = load_registrations(&args.registrations_dir)?;

    let mut ordered_members = Vec::with_capacity(POC_COMMITTEE_SIZE);
    for (index, (identity, registration)) in validator_identity_hashes
        .into_iter()
        .zip(registrations)
        .enumerate()
    {
        let validator_index = u8::try_from(index)?;
        ensure!(
            registration.core.chain_id == chain_id
                && registration.core.genesis_hash == genesis_hash
                && registration.core.fork_id == protocol_bundle.fork_id
                && registration.core.protocol_bundle_hash == protocol_bundle_hash
                && registration.core.validator_index == validator_index
                && registration.core.validator_identity_hash == identity
                && registration.core.key_epoch == POC_KEY_EPOCH
                && registration.core.allowed_purpose_bitmap == RESULT_SIGNATURE_PURPOSE_BITMAP
                && registration.core.valid_from_height == ACTIVATION_HEIGHT
                && registration.core.valid_until_height_exclusive == u64::MAX,
            "validator-{validator_index} OCOMP registration does not match this genesis"
        );
        registration
            .validate_proof_of_possession(&limits)
            .wrap_err_with(|| format!("verify validator-{validator_index} OCOMP registration"))?;
        ordered_members.push(OcompMemberV1 {
            validator_index,
            validator_identity_hash: identity,
            ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
            key_epoch: registration.core.key_epoch,
            allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
            valid_from_height: registration.core.valid_from_height,
            valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
            proof_of_possession: registration.proof_of_possession,
        });
    }
    let result_committee = OcompCommitteeSnapshotV1 {
        chain_id,
        genesis_hash,
        fork_id: protocol_bundle.fork_id,
        protocol_bundle_hash,
        snapshot_epoch: 1,
        threshold: POC_COMMITTEE_THRESHOLD,
        ordered_members,
    };
    result_committee.validate_semantics(&limits)?;
    let result_committee_snapshot_hash = result_committee.snapshot_hash(&limits)?;
    let install = OcompForkInstallV1 {
        classification: OcompForkInstallClassification::Measurement,
        activation_height: ACTIVATION_HEIGHT,
        request_profile: OcompRequestProfile {
            chain_id,
            genesis_hash,
            fork_id: protocol_bundle.fork_id,
            protocol_bundle_hash,
            correctness_profile_id: protocol_bundle.correctness_profile_id,
            capacity_profile: measurement_capacity_profile(),
            source_availability_policy_id: B256::repeat_byte(44),
            result_committee_snapshot_hash,
        },
        protocol_bundle,
        result_committee,
    };
    install.validate_for_chain(chain_id, genesis_hash, &limits)?;
    let canonical_install = install.encode_canonical(&limits)?;
    let install_hash = install.install_hash(&limits)?;

    let mut genesis: serde_json::Value = serde_json::from_slice(
        &fs::read(&args.input)
            .wrap_err_with(|| format!("read input genesis {}", args.input.display()))?,
    )
    .wrap_err("decode input genesis JSON")?;
    let config = genesis
        .get_mut("config")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("input genesis config must be a JSON object"))?;
    ensure!(
        !config.contains_key(OCOMP_FIELD) && !config.contains_key(LAYOUT_FIELD),
        "input genesis already contains an OCOMP or Metadosis layout extension"
    );
    config.insert(
        OCOMP_FIELD.to_owned(),
        serde_json::json!({
            "canonicalBytes": format!("0x{}", hex::encode(canonical_install)),
            "installHash": install_hash,
        }),
    );
    config.insert(
        LAYOUT_FIELD.to_owned(),
        serde_json::json!({ "layoutHash": METADOSIS_STORAGE_LAYOUT_V1_HASH }),
    );

    let genesis_bytes = pretty_json(&genesis)?;
    let protocol_bundle_bytes = install.protocol_bundle.encode_canonical(&limits)?;
    write_new_file(&args.output, &genesis_bytes, "OCOMP genesis")?;
    if let Err(error) = write_new_file(
        &args.protocol_bundle_output,
        &protocol_bundle_bytes,
        "OCOMP protocol bundle",
    )
    .and_then(|()| validate_output(&args.output, chain_id, genesis_hash, &install))
    {
        let _ = fs::remove_file(&args.output);
        let _ = fs::remove_file(&args.protocol_bundle_output);
        return Err(error);
    }
    println!(
        "wrote block-1 Measurement OCOMP genesis: chain_id={chain_id}, genesis_hash={genesis_hash}, output={}",
        args.output.display()
    );
    Ok(())
}

fn parse_base_identity(path: &Path) -> eyre::Result<(u64, B256)> {
    let path = utf8_path(path, "input genesis")?;
    let spec = reth_ethereum::cli::chainspec::chain_value_parser(path)
        .wrap_err_with(|| format!("parse input genesis {path}"))?;
    let extra = &spec.genesis.config.extra_fields;
    ensure!(
        !extra.contains_key(OCOMP_FIELD) && !extra.contains_key(LAYOUT_FIELD),
        "input genesis must not already contain OCOMP extensions"
    );
    Ok((spec.chain().id(), spec.genesis_hash()))
}

fn validator_identities(path: &Path) -> eyre::Result<[B256; POC_COMMITTEE_SIZE]> {
    let validators: serde_json::Value = serde_json::from_slice(
        &fs::read(path).wrap_err_with(|| format!("read validators {}", path.display()))?,
    )
    .wrap_err("decode validators JSON")?;
    let validators = validators
        .as_array()
        .ok_or_else(|| eyre::eyre!("validators manifest must be a JSON array"))?;
    ensure!(
        validators.len() == POC_COMMITTEE_SIZE,
        "OCOMP V1 requires exactly {POC_COMMITTEE_SIZE} founding validators, got {}",
        validators.len()
    );
    let mut identities = [B256::ZERO; POC_COMMITTEE_SIZE];
    for (index, validator) in validators.iter().enumerate() {
        let address = validator
            .get("address")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no address"))
            .and_then(|value| Address::from_str(value).map_err(Into::into))?;
        let public_key = validator
            .get("public_key")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no public_key"))?;
        let public_key = hex::decode(public_key.strip_prefix("0x").unwrap_or(public_key))?;
        ensure!(
            public_key.len() == 48,
            "validator-{index} public key must be 48 bytes"
        );
        let mut payload = Vec::with_capacity(1 + 20 + 4 + public_key.len());
        payload.push(u8::try_from(index)?);
        payload.extend_from_slice(address.as_slice());
        payload.extend_from_slice(&u32::try_from(public_key.len())?.to_be_bytes());
        payload.extend_from_slice(&public_key);
        identities[index] = hash_framed(HashDomain::ValidatorIdentity, &payload)?;
    }
    Ok(identities)
}

fn load_registrations(
    directory: &Path,
) -> eyre::Result<[OcompKeyRegistrationV1; POC_COMMITTEE_SIZE]> {
    let limits = outbe_ocomp_protocol::profile::poc_schema_limits();
    let mut registrations = Vec::with_capacity(POC_COMMITTEE_SIZE);
    for index in 0..POC_COMMITTEE_SIZE {
        let path = directory
            .join(format!("validator-{index}"))
            .join("ocomp-registration-v1.ocb1");
        registrations.push(
            OcompKeyRegistrationV1::decode_canonical(
                &fs::read(&path)
                    .wrap_err_with(|| format!("read OCOMP registration {}", path.display()))?,
                &limits,
            )
            .wrap_err_with(|| format!("decode OCOMP registration {}", path.display()))?,
        );
    }
    registrations
        .try_into()
        .map_err(|_| eyre::eyre!("expected exactly {POC_COMMITTEE_SIZE} registrations"))
}

fn validate_output(
    output: &Path,
    expected_chain_id: u64,
    expected_genesis_hash: B256,
    expected_install: &OcompForkInstallV1,
) -> eyre::Result<()> {
    let parsed: ChainSpec<OutbeHeader> =
        reth_ethereum::cli::chainspec::chain_value_parser(utf8_path(output, "output genesis")?)?
            .as_ref()
            .clone()
            .map_header(OutbeHeader::new);
    ensure!(
        parsed.chain().id() == expected_chain_id,
        "generated genesis changed chain ID"
    );
    ensure!(
        parsed.genesis_hash() == expected_genesis_hash,
        "OCOMP extension changed genesis hash"
    );
    let loaded = outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&parsed)?;
    ensure!(
        loaded.as_ref() == expected_install,
        "node parser returned a different OCOMP install"
    );
    Ok(())
}

fn measurement_capacity_profile() -> CapacityProfileV1 {
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

fn measurement_protocol_bundle() -> ProtocolBundleV1 {
    let hash = B256::repeat_byte;
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

fn require_new_output(path: &Path, description: &str) -> eyre::Result<()> {
    ensure!(
        !path.exists(),
        "refusing to overwrite existing {description}: {}",
        path.display()
    );
    ensure!(
        path.parent().is_some_and(Path::exists),
        "{description} parent does not exist: {}",
        path.display()
    );
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8], description: &str) -> eyre::Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create {description} {}", path.display()))?;
    output
        .write_all(bytes)
        .and_then(|()| output.sync_all())
        .wrap_err_with(|| format!("write {description} {}", path.display()))
}

fn pretty_json(value: &impl Serialize) -> eyre::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn utf8_path<'a>(path: &'a Path, description: &str) -> eyre::Result<&'a str> {
    path.to_str()
        .ok_or_else(|| eyre::eyre!("{description} path is not UTF-8: {}", path.display()))
}
