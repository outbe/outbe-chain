//! Seed founding validator, staking and reward state.

use std::collections::BTreeSet;

use alloy_primitives::U256;
use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::inputs::{StakeSeedTerms, ValidatorSeedTerms};
use crate::keccak::keccak256;
use crate::seed_codec::{
    address_as_u256, address_bytes, alloc_balance_hex, parse_genesis_timestamp, parse_i128,
    parse_u256, parse_u64, pubkey_bytes, radicle_node_id_bytes, u64_bytes, write_mapping_bytes,
    StorageBuilder, DEFAULT_EPOCH_LENGTH_BLOCKS, DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS,
    DEFAULT_UNBONDING_PERIOD, HYPERLANE_CONTROLLER_ADDRESS,
    HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT, MIN_STAKE, STAKING_ADDRESS, VALIDATOR_SET_ADDRESS,
    VAULT_ROUTER_ADDRESS,
};
use crate::seed_economy::{merge_storage_into_alloc, seed_vault_router};
use crate::seed_p2p::encode_p2p_address_payload;

/// Consensus bound on maximum registered validators (`outbe_consensus::bls::MAX_VALIDATORS`).
pub const CONSENSUS_VALIDATOR_BOUND: u64 = 256;

/// Seed the ValidatorSet precompile storage layout.
pub fn seed_validator_set(
    storage: &mut StorageBuilder,
    validators: &[Value],
    config: &Value,
    terms: ValidatorSeedTerms,
) -> Result<(), GenesisError> {
    let mut radicle_node_ids = Vec::with_capacity(validators.len());
    let mut unique_node_ids = BTreeSet::new();
    for (index, validator) in validators.iter().enumerate() {
        let node_id = radicle_node_id_bytes(validator.get("radicle_node_id"), index)?;
        if !unique_node_ids.insert(node_id) {
            return Err(GenesisError::validation(
                "duplicate Radicle NodeId in founder validators",
            ));
        }
        radicle_node_ids.push(node_id);
    }

    let owner = config
        .get("owner")
        .and_then(Value::as_str)
        .unwrap_or("0x0000000000000000000000000000000000000000");
    storage.set_slot(0u64, address_as_u256(owner)?);

    let max_validators = config
        .get("max_validators")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(128);
    if max_validators > CONSENSUS_VALIDATOR_BOUND {
        return Err(GenesisError::validation(format!(
            "validator_set.max_validators {max_validators} exceeds the consensus bound {CONSENSUS_VALIDATOR_BOUND}"
        )));
    }
    storage.set_slot(1u64, max_validators);

    if config.get("epoch_duration").is_some() {
        return Err(GenesisError::validation(
            "validator_set.epoch_duration is deprecated; use config.epochLengthBlocks",
        ));
    }
    if config.get("epoch_length_blocks").is_some() {
        return Err(GenesisError::validation(
            "validator_set.epoch_length_blocks is deprecated; use config.epochLengthBlocks",
        ));
    }

    storage.set_slot(2u64, terms.epoch_length_blocks);
    storage.set_slot(3u64, terms.min_stake);
    storage.set_slot(4u64, 1u64);
    storage.set_slot(20u64, validators.len());
    storage.set_slot(
        21u64,
        config
            .get("epoch_number")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(0),
    );
    storage.set_slot(
        22u64,
        config
            .get("epoch_start_timestamp")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(terms.epoch_start_timestamp),
    );
    storage.set_slot(
        23u64,
        config
            .get("epoch_start_block")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(0),
    );
    storage.set_slot(25u64, 0u64);
    storage.set_slot(
        26u64,
        config
            .get("active_consensus_set_hash")
            .map(parse_u256)
            .transpose()?
            .unwrap_or(U256::ZERO),
    );
    storage.set_slot(
        27u64,
        config
            .get("reregistration_cooldown_blocks")
            .map(parse_u64)
            .transpose()?
            .unwrap_or(DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS),
    );

    for (idx0, (validator, radicle_node_id)) in
        validators.iter().zip(radicle_node_ids.iter()).enumerate()
    {
        let index = (idx0 + 1) as u64;
        let addr = validator
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| GenesisError::validation("validator missing address"))?;
        let pk_str = validator
            .get("public_key")
            .and_then(Value::as_str)
            .ok_or_else(|| GenesisError::validation("validator missing public_key"))?;
        let pk = pubkey_bytes(pk_str)?;
        let mut pk_lo = [0u8; 32];
        pk_lo.copy_from_slice(&pk[..32]);
        let mut pk_hi = [0u8; 32];
        pk_hi[..16].copy_from_slice(&pk[32..]);
        let pk_hash = keccak256(&pk);
        let addr_bytes = address_bytes(addr)?;
        let addr_u256 = address_as_u256(addr)?;

        storage.set_mapping_b256(5, &addr_bytes, &pk_lo);
        storage.set_mapping_b256(6, &addr_bytes, &pk_hi);
        storage.set_mapping(8, &addr_bytes, 2u64);
        storage.set_mapping(13, &addr_bytes, 0u64);
        storage.set_mapping(16, &addr_bytes, index);
        storage.set_mapping(17, &u64_bytes(index), addr_u256);
        storage.set_mapping(18, &pk_hash, addr_u256);
        storage.set_mapping(24, &addr_bytes, 1u64);
        storage.set_mapping_b256(50, &addr_bytes, radicle_node_id);
        storage.set_mapping(51, radicle_node_id, addr_u256);

        if let Some((version, payload)) = encode_p2p_address_payload(validator.get("p2p_address"))?
        {
            storage.set_mapping(28, &addr_bytes, version);
            write_mapping_bytes(storage, 29, &addr_bytes, &payload)?;
        }
    }
    Ok(())
}

/// Seed the Staking precompile storage layout and return `total_staked`.
pub fn seed_staking(
    storage: &mut StorageBuilder,
    validators: &[Value],
    config: &Value,
    terms: StakeSeedTerms,
) -> Result<U256, GenesisError> {
    if terms.validator_stake < terms.min_stake {
        return Err(GenesisError::validation(
            "genesis_validator_stake must be >= min_stake",
        ));
    }
    storage.set_slot(0u64, terms.min_stake);
    let unbonding_period = config
        .get("unbonding_period")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(DEFAULT_UNBONDING_PERIOD);
    storage.set_slot(1u64, unbonding_period);
    let max_stake_percent = config
        .get("max_stake_percent")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(33);
    storage.set_slot(2u64, max_stake_percent);
    let slashed_withdrawal_delay = config
        .get("slashed_withdrawal_delay")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(unbonding_period.saturating_mul(2));
    storage.set_slot(11u64, slashed_withdrawal_delay);

    let mut total_staked = U256::ZERO;
    for validator in validators {
        let addr = validator
            .get("address")
            .and_then(Value::as_str)
            .ok_or_else(|| GenesisError::validation("validator missing address"))?;
        storage.set_mapping(3, &address_bytes(addr)?, terms.validator_stake);
        total_staked += terms.validator_stake;
    }
    storage.set_slot(4u64, total_staked);
    Ok(total_staked)
}

/// Seed ValidatorSet, VaultRouter, HyperlaneController, and Staking for founding validators.
pub fn seed_founders(
    genesis: &Value,
    seed: &Value,
    validators: &[Value],
    alloc: &mut Map<String, Value>,
) -> Result<(), GenesisError> {
    let staking_cfg = seed.get("staking").unwrap_or(&Value::Null);
    let min_stake = staking_cfg
        .get("min_stake")
        .or_else(|| seed.get("min_stake"))
        .map(parse_u256)
        .transpose()?
        .filter(|value| !value.is_zero())
        .unwrap_or_else(|| U256::from(MIN_STAKE));
    let validator_stake = staking_cfg
        .get("genesis_validator_stake")
        .map(parse_u256)
        .transpose()?
        .filter(|value| !value.is_zero())
        .unwrap_or(min_stake);
    if validator_stake < min_stake {
        return Err(GenesisError::validation(
            "genesis_validator_stake must be >= min_stake",
        ));
    }

    let config = genesis.get("config").unwrap_or(&Value::Null);
    if config.get("epochDuration").is_some() {
        return Err(GenesisError::validation(
            "genesis config uses deprecated epochDuration; use epochLengthBlocks",
        ));
    }
    if config.get("dkgRotationIntervalBlocks").is_some() {
        return Err(GenesisError::validation(
            "genesis config uses deprecated dkgRotationIntervalBlocks; use epochLengthBlocks",
        ));
    }
    let epoch_length_signed = match config.get("epochLengthBlocks") {
        Some(v) => parse_i128(v)?,
        None => i128::from(DEFAULT_EPOCH_LENGTH_BLOCKS),
    };
    if epoch_length_signed <= 0 {
        return Err(GenesisError::validation(
            "genesis config epochLengthBlocks must be > 0",
        ));
    }
    let epoch_length_blocks = epoch_length_signed as u64;

    for timing_key in [
        "minBlockTimeMs",
        "leaderTimeoutMs",
        "certificationTimeoutMs",
    ] {
        if let Some(val) = config.get(timing_key) {
            if parse_i128(val)? <= 0 {
                return Err(GenesisError::validation(format!(
                    "genesis config {timing_key} must be > 0"
                )));
            }
        }
    }
    let epoch_start_timestamp = parse_genesis_timestamp(genesis)?;

    let mut validator_storage = StorageBuilder::new();
    let validator_set_cfg = seed.get("validator_set").unwrap_or(&Value::Null);
    seed_validator_set(
        &mut validator_storage,
        validators,
        validator_set_cfg,
        ValidatorSeedTerms {
            epoch_length_blocks,
            epoch_start_timestamp,
            min_stake,
            validator_stake,
        },
    )?;
    merge_storage_into_alloc(alloc, VALIDATOR_SET_ADDRESS, &validator_storage)?;

    let mut vault_router_storage = StorageBuilder::new();
    seed_vault_router(&mut vault_router_storage)?;
    merge_storage_into_alloc(alloc, VAULT_ROUTER_ADDRESS, &vault_router_storage)?;

    let default_admin = validators[0]
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("validator 0 missing address"))?;
    let hyperlane_admin = seed
        .pointer("/hyperlane_controller/admin")
        .and_then(Value::as_str)
        .unwrap_or(default_admin);
    let mut hyperlane_storage = StorageBuilder::new();
    hyperlane_storage.set_slot(
        HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT,
        address_as_u256(hyperlane_admin)?,
    );
    merge_storage_into_alloc(alloc, HYPERLANE_CONTROLLER_ADDRESS, &hyperlane_storage)?;

    let mut staking_storage = StorageBuilder::new();
    let total_staked = seed_staking(
        &mut staking_storage,
        validators,
        staking_cfg,
        StakeSeedTerms {
            min_stake,
            validator_stake,
        },
    )?;
    merge_storage_into_alloc(alloc, STAKING_ADDRESS, &staking_storage)?;
    if let Some(staking_entry) = alloc
        .get_mut(STAKING_ADDRESS)
        .and_then(Value::as_object_mut)
    {
        staking_entry.insert(
            "balance".to_owned(),
            Value::String(alloc_balance_hex(total_staked)),
        );
    }

    Ok(())
}
