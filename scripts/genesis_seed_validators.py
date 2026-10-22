"""Seed founding validator, staking and reward state."""

from __future__ import annotations
from genesis_inputs import StakeSeedTerms, ValidatorSeedTerms

from genesis_seed_codec import (
    DEFAULT_EPOCH_LENGTH_BLOCKS,
    DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS,
    DEFAULT_UNBONDING_PERIOD,
    HYPERLANE_CONTROLLER_ADDRESS,
    HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT,
    MIN_STAKE,
    REWARDS_ADDRESS,
    STAKING_ADDRESS,
    StorageBuilder,
    VALIDATOR_SET_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    address_as_u256,
    address_bytes,
    alloc_balance_hex,
    parse_genesis_timestamp,
    parse_int,
    pubkey_bytes,
    radicle_node_id_bytes,
    timestamp_to_utc_date_key,
    u64_bytes,
    write_mapping_bytes,
)

from genesis_keccak import keccak256
from genesis_seed_economy import seed_vault_router
from genesis_seed_p2p import encode_p2p_address_payload

# `CONSENSUS_VALIDATOR_BOUND` in crates/system/validatorset/src/runtime/mod.rs
# (`outbe_consensus::bls::MAX_VALIDATORS`).
CONSENSUS_VALIDATOR_BOUND = 256


def seed_validator_set(
    storage: StorageBuilder,
    validators: list[dict],
    config: dict,
    *,
    terms: ValidatorSeedTerms,
):
    """
    ValidatorSet storage layout:
      slots 0-4: config
      slots 5-18: per-validator mappings and reverse indexes
      slot 20: validator_count
      slots 21-26: epoch / consensus-set tracking
      slot 27: re-registration cooldown
      slots 28-29: versioned Commonware P2P address registry
      slots 50-51: Radicle NodeId forward and reverse indexes
    """
    epoch_length_blocks = terms.epoch_length_blocks
    epoch_start_timestamp = terms.epoch_start_timestamp
    min_stake = terms.min_stake
    radicle_node_ids = [
        radicle_node_id_bytes(validator.get("radicle_node_id"), validator_index=index)
        for index, validator in enumerate(validators)
    ]
    if len(set(radicle_node_ids)) != len(radicle_node_ids):
        raise ValueError("duplicate Radicle NodeId in founder validators")

    storage.set_slot(
        0,
        address_as_u256(
            config.get("owner", "0x0000000000000000000000000000000000000000")
        ),
    )
    max_validators = parse_int(config.get("max_validators", 128))
    # Same bound as ValidatorSet::set_config_max_validators. The Oracle slash
    # window processes at most this many registered validators.
    if max_validators > CONSENSUS_VALIDATOR_BOUND:
        raise ValueError(
            f"validator_set.max_validators {max_validators} exceeds the consensus "
            f"bound {CONSENSUS_VALIDATOR_BOUND}"
        )
    storage.set_slot(1, max_validators)
    if "epoch_duration" in config:
        raise ValueError(
            "validator_set.epoch_duration is deprecated; use config.epochLengthBlocks"
        )
    if "epoch_length_blocks" in config:
        raise ValueError(
            "validator_set.epoch_length_blocks is deprecated; use config.epochLengthBlocks"
        )
    storage.set_slot(2, epoch_length_blocks)
    storage.set_slot(3, min_stake)
    storage.set_slot(4, 1)
    storage.set_slot(20, len(validators))
    storage.set_slot(21, parse_int(config.get("epoch_number", 0)))
    storage.set_slot(
        22, parse_int(config.get("epoch_start_timestamp", epoch_start_timestamp))
    )
    storage.set_slot(23, parse_int(config.get("epoch_start_block", 0)))
    storage.set_slot(25, 0)
    storage.set_slot(26, parse_int(config.get("active_consensus_set_hash", 0)))
    storage.set_slot(
        27,
        parse_int(
            config.get(
                "reregistration_cooldown_blocks",
                DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS,
            )
        ),
    )

    for index, (validator, radicle_node_id) in enumerate(
        zip(validators, radicle_node_ids, strict=True), start=1
    ):
        addr = validator["address"]
        pk = pubkey_bytes(validator["public_key"])
        pk_hi = pk[32:] + (b"\x00" * 16)
        pk_hash = keccak256(pk)

        storage.set_mapping_b256(5, address_bytes(addr), pk[:32])
        storage.set_mapping_b256(6, address_bytes(addr), pk_hi)
        # Slot 7 is reserved. Current bonded stake is Staking mapping slot 3.
        storage.set_mapping(8, address_bytes(addr), 2)  # ACTIVE
        storage.set_mapping(13, address_bytes(addr), 0)
        storage.set_mapping(16, address_bytes(addr), index)
        storage.set_mapping(17, u64_bytes(index), address_as_u256(addr))
        storage.set_mapping(18, pk_hash, address_as_u256(addr))
        storage.set_mapping(24, address_bytes(addr), 1)
        storage.set_mapping_b256(50, address_bytes(addr), radicle_node_id)
        storage.set_mapping(51, radicle_node_id, address_as_u256(addr))
        p2p_seed = encode_p2p_address_payload(validator.get("p2p_address"))
        if p2p_seed is not None:
            version, payload = p2p_seed
            storage.set_mapping(28, address_bytes(addr), version)
            write_mapping_bytes(storage, 29, address_bytes(addr), payload)


def seed_staking(
    storage: StorageBuilder,
    validators: list[dict],
    config: dict,
    *,
    terms: StakeSeedTerms,
):
    """
    Staking storage layout:
      slots 0-2: config
      slot 3: mapping(validator => stake_amount)
      slot 4: total_staked
    """
    min_stake = terms.min_stake
    validator_stake = terms.validator_stake
    if validator_stake < min_stake:
        raise ValueError("genesis_validator_stake must be >= min_stake")

    storage.set_slot(0, min_stake)
    unbonding_period = parse_int(
        config.get("unbonding_period", DEFAULT_UNBONDING_PERIOD)
    )
    storage.set_slot(1, unbonding_period)
    storage.set_slot(2, parse_int(config.get("max_stake_percent", 33)))
    storage.set_slot(
        11,
        parse_int(config.get("slashed_withdrawal_delay", unbonding_period * 2)),
    )

    total_staked = 0
    for validator in validators:
        storage.set_mapping(3, address_bytes(validator["address"]), validator_stake)
        total_staked += validator_stake

    storage.set_slot(4, total_staked)
    return total_staked


def seed_rewards(storage: StorageBuilder, genesis_timestamp: int):
    """
    Rewards storage layout:
      slot 0: genesis_utc_day (uint32 yyyymmdd of genesis timestamp).

    Layout history: `genesis_utc_day` moved from slot 1 to slot 0 when the leading
    `pending_rewards` field was removed (PR #12 / 941c4eb). The runtime also
    lazily anchors this value at block 0 via `rewards::ensure_genesis_anchor`
    (= timestamp_to_date_key(block0.timestamp)). Seeding it here keeps genesis
    state explicit and matches that block-0 value.
    """
    storage.set_slot(0, timestamp_to_utc_date_key(genesis_timestamp))


def _seed_founders(genesis, seed, validators, alloc):
    staking_cfg = seed.get("staking", {})
    min_stake = parse_int(staking_cfg.get("min_stake", MIN_STAKE))
    validator_stake = parse_int(staking_cfg.get("genesis_validator_stake", min_stake))
    if validator_stake < min_stake:
        raise ValueError("genesis_validator_stake must be >= min_stake")
    config = genesis.get("config", {})
    if "epochDuration" in config:
        raise ValueError(
            "genesis config uses deprecated epochDuration; use epochLengthBlocks"
        )
    if "dkgRotationIntervalBlocks" in config:
        raise ValueError(
            "genesis config uses deprecated dkgRotationIntervalBlocks; use epochLengthBlocks"
        )
    epoch_length_blocks = parse_int(
        config.get("epochLengthBlocks", DEFAULT_EPOCH_LENGTH_BLOCKS)
    )
    if epoch_length_blocks <= 0:
        raise ValueError("genesis config epochLengthBlocks must be > 0")
    # Pass-through sanity check for the consensus-sync timing trio. The seeder
    # does not author these (they fall back to outbe_consensus::timing
    # defaults). It only rejects an obviously malformed non-positive value so
    # a bad genesis fails early. validate_timing enforces the full ordering
    # invariant (0 < min < leader <= cert) at startup.
    for _timing_key in ("minBlockTimeMs", "leaderTimeoutMs", "certificationTimeoutMs"):
        if _timing_key in config and parse_int(config[_timing_key]) <= 0:
            raise ValueError(f"genesis config {_timing_key} must be > 0")
    epoch_start_timestamp = parse_genesis_timestamp(genesis)

    validator_storage = StorageBuilder()
    seed_validator_set(
        validator_storage,
        validators,
        seed.get("validator_set", {}),
        terms=ValidatorSeedTerms(
            epoch_length_blocks=epoch_length_blocks,
            epoch_start_timestamp=epoch_start_timestamp,
            min_stake=min_stake,
            validator_stake=validator_stake,
        ),
    )
    alloc[VALIDATOR_SET_ADDRESS].setdefault("storage", {}).update(
        validator_storage.entries
    )

    # Seed only factory liquidity routes. Vault registration requires governance.
    vault_router_storage = StorageBuilder()
    seed_vault_router(vault_router_storage)
    alloc[VAULT_ROUTER_ADDRESS].setdefault("storage", {}).update(
        vault_router_storage.entries
    )
    print("  VaultRouter: factory liquidity routes seeded, no owner or vaults")

    # HyperlaneController bootstrap admin: the only caller of the one-shot
    # `initialize`. Validator0 by default (overridable via
    # seed["hyperlane_controller"]["admin"]).
    hyperlane_admin = seed.get("hyperlane_controller", {}).get(
        "admin", validators[0]["address"]
    )
    hyperlane_storage = StorageBuilder()
    hyperlane_storage.set_slot(
        HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT, address_as_u256(hyperlane_admin)
    )
    alloc[HYPERLANE_CONTROLLER_ADDRESS].setdefault("storage", {}).update(
        hyperlane_storage.entries
    )
    print(f"  HyperlaneController: bootstrap admin={hyperlane_admin}")

    staking_storage = StorageBuilder()
    total_staked = seed_staking(
        staking_storage,
        validators,
        staking_cfg,
        terms=StakeSeedTerms(min_stake=min_stake, validator_stake=validator_stake),
    )
    staking_entry = alloc[STAKING_ADDRESS]
    staking_entry.setdefault("storage", {}).update(staking_storage.entries)
    staking_entry["balance"] = alloc_balance_hex(total_staked)

    rewards_storage = StorageBuilder()
    seed_rewards(rewards_storage, epoch_start_timestamp)
    alloc[REWARDS_ADDRESS].setdefault("storage", {}).update(rewards_storage.entries)

    print(
        f"  ValidatorSet: {len(validators)} active validators, "
        f"{len(validator_storage.entries)} storage entries"
    )
    print(
        f"  Staking: total_staked={total_staked}, "
        f"{len(staking_storage.entries)} storage entries"
    )
    print(f"  Rewards: {len(rewards_storage.entries)} storage entries")
