#!/usr/bin/env python3
"""
Seed genesis.json with precompile storage entries for outbe-chain.

Computes EVM storage slots matching the Rust contract layout
(Solidity-compatible: keccak256(left_pad(key, 32) ++ to_be(slot, 32))).

Usage:
  python3 scripts/seed_genesis.py \
    --genesis /tmp/outbe/genesis.json \
    --seed scripts/seed-testnet.json \
    --validators /tmp/outbe/validators.json \
    --output /tmp/outbe/genesis-seeded.json

Dependencies: pycryptodome (pip install pycryptodome) or pysha3.
Uses a small pure-Python Keccak-256 implementation for hermetic localnet smoke
runs when neither optional package is available.
"""

from __future__ import annotations
import argparse
import ipaddress
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from genesis_seed_codec import seed_profile_selector
from genesis_seed_system import _seed_system_state
from genesis_seed_economy import _seed_economic_state
from genesis_inputs import (
    SeedInputs,
    SeedOptions,
    StakeSeedTerms,
    ValidatorSeedTerms,
)
from genesis_seed_alloc import (
    _validate_reserved_account,
    _validate_reserved_balance,
    seed_external_contracts,
    seed_protocol_constants,
    seed_radicle_registry,
    validate_stablecoin_namespace_alloc,
)
from genesis_seed_economy import clear_seeded_metadosis_days, override_worldwide_day
from genesis_seed_codec import (
    ACCOUNTING_PROGRESS_ADDRESS,
    AGENT_REWARD_ADDRESS,
    ALL_PRECOMPILE_ADDRESSES,
    CCA_REGISTRY_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_PROFILE_SLOT,
    CREDIS_FACTORY_ADDRESS,
    CYCLE_ADDRESS,
    DEFAULT_EPOCH_LENGTH_BLOCKS,
    DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS,
    DEFAULT_UNBONDING_PERIOD,
    EMISSION_LIMIT_ADDRESS,
    FIDELITY_ADDRESS,
    GEM_ADDRESS,
    GEM_FACTORY_ADDRESS,
    GEM_PROFILE_SLOT,
    GOVERNANCE_ADDRESS,
    GRATIS_ADDRESS,
    GRATIS_FACTORY_ADDRESS,
    INTEX_ADDRESS,
    INTEX_PROFILE_SLOT,
    ISO_MARKER,
    MARKER_CODE,
    MAX_P2P_ADDRESS_ENCODED_LEN,
    METADOSIS_ADDRESS,
    MIN_STAKE,
    NOD_ADDRESS,
    NOD_FACTORY_ADDRESS,
    NOD_PROFILE_SLOT,
    ORACLE_ADDRESS,
    OUTBE_SYSTEM_TX_ADDRESS,
    P2P_ADDRESS_VERSION_V1,
    PROFILE_SELECTORS,
    PROMIS_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    PROTECTED_PROTOCOL_ADDRESSES,
    PROTOCOL_ACCUMULATOR_ADDRESSES,
    RADICLE_REGISTRY_ADDRESS,
    REWARDS_ADDRESS,
    SECONDS_PER_DAY,
    SLASH_INDICATOR_ADDRESS,
    STABLECOIN_ADDRESS_PREFIX,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    STAKING_ADDRESS,
    StorageBuilder,
    TEE_REGISTRY_ADDRESS,
    TRIBUTE_ADDRESS,
    TRIBUTE_FACTORY_ADDRESS,
    UPDATE_ADDRESS,
    VALIDATOR_SET_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    VAULT_ROUTER_LIQUIDITY_SOURCES,
    VAULT_ROUTER_LIQUIDITY_TARGETS,
    VOTE_ADDRESS,
    ZEROFEE_ADDRESS,
    address_as_u256,
    address_bytes,
    alloc_balance_hex,
    b256_bytes,
    civil_date_from_days,
    compute_tee_policy_hash,
    data_slot,
    hex32,
    mapping_key,
    parse_genesis_timestamp,
    parse_header_timestamp,
    parse_int,
    pubkey_bytes,
    radicle_node_id_bytes,
    timestamp_to_utc_date_key,
    to_be32,
    u32_bytes,
    u64_bytes,
    write_mapping_bytes,
    write_mapping_string,
    write_storage_bytes,
    wwd_to_day_timestamp,
)
from genesis_keccak import (
    _KECCAK_RATE_BYTES,
    _KECCAK_ROTATION_OFFSETS,
    _KECCAK_ROUND_CONSTANTS,
    _MASK64,
    _keccak_f1600,
    _pure_python_keccak256,
    _rotl64,
    keccak256,
)
from genesis_seed_economy import (
    _seed_address_set_with_types,
    day_index_key,
    owner_index_key,
    seed_coen,
    seed_cycle,
    seed_metadosis,
    seed_nod_materialization_fifo,
    seed_tribute_day_totals,
    seed_tributes,
    seed_vault_router,
    tribute_token_id,
)
from genesis_seed_codec import address_pair, asset_address, is_iso_asset_address
from genesis_seed_oracle import (
    _seed_oracle_config,
    _seed_oracle_currencies,
    _seed_oracle_registry,
    _seed_oracle_scurves,
    _validate_oracle_pairs,
    seed_oracle,
)
from genesis_seed_p2p import (
    encode_p2p_address_payload,
    encode_p2p_ingress,
    encode_p2p_socket,
    parse_host_port,
    validate_hostname,
)
from genesis_seed_validators import (
    _seed_founders,
    seed_rewards,
    seed_staking,
    seed_validator_set,
)
from genesis_seed_system import (
    seed_accounting_progress,
    seed_governance,
    seed_tee_policy,
    seed_zerofee,
)


def apply_seed(
    genesis: dict,
    seed: dict,
    validators: list | None = None,
    *,
    options: SeedOptions | None = None,
) -> dict:
    """Compute every derived value a genesis needs and write it into `genesis`.

    This function calculates these genesis values:
    - The storage slot layout for each precompile.
    - The keccak-derived mapping keys.
    - The active WorldwideDay for the genesis timestamp.
    - The marker bytecode.
    - The balances that must match seeded counters.
    `create_genesis.py` owns the declarative half
    (what the network *is*, from a yaml) and calls this to render it.

    The function mutates `genesis` in place and returns it.
    """
    options = options or SeedOptions()
    contracts_dir = options.contracts_dir
    canon_dir = options.canon_dir
    worldwide_day = options.worldwide_day
    fresh_metadosis = options.fresh_metadosis

    seed_protocol_constants(genesis, seed)

    # When requested, retarget the seeded worldwide-day to the genesis (current) date.
    # Do this before any seeder consumes `seed`. This gives metadosis, the oracle
    # S-curve, tribute day_totals, and the NODs the same active day.
    if worldwide_day is not None:
        override_worldwide_day(seed, worldwide_day)
    if fresh_metadosis:
        clear_seeded_metadosis_days(seed)

    validators = validators or []
    if not isinstance(validators, list):
        raise ValueError("validators must be a list")

    alloc = genesis.setdefault("alloc", {})
    validate_stablecoin_namespace_alloc(alloc, require_reserved_markers=False)

    # Ensure all precompile addresses have marker bytecode
    for addr in ALL_PRECOMPILE_ADDRESSES:
        entry = alloc.setdefault(addr, {})
        entry["code"] = MARKER_CODE
        entry.setdefault("balance", "0x0")

    header_timestamp = parse_header_timestamp(genesis)
    cycle_storage = StorageBuilder()
    seed_cycle(cycle_storage, header_timestamp)
    alloc[CYCLE_ADDRESS].setdefault("storage", {}).update(cycle_storage.entries)
    print(
        "  Cycle: active_utc_day="
        f"{timestamp_to_utc_date_key(header_timestamp)}, slot 2 seeded"
    )

    # Seed native EVM token balances into alloc.
    if "balance" in seed:
        seed_coen(alloc, seed["balance"])
        print(f"  balance: {len(seed['balance'])} entries")

    # Seed ValidatorSet, Staking, and Rewards from validators.json. This makes
    # genesis.json the canonical protocol state. The executor no longer backfills it.
    if validators:
        _seed_founders(genesis, seed, validators, alloc)

    _seed_system_state(genesis, seed, alloc, canon_dir)

    # Gratis and Promis use TEE encryption at rest. Per-account balances are
    # ciphertext that uses enclave state keys. Genesis cannot seed plaintext balances.
    # A Gem is issued at runtime and mined Gem -> Promis -> Gratis through the
    # enclave. Fail loudly if a stale seed still carries these keys.
    for _unsupported_key in ("gratis_balances", "promis_balances", "gems"):
        if _unsupported_key in seed:
            raise ValueError(
                f"{_unsupported_key} is no longer supported at genesis: issue the Gem at "
                f"runtime and mine Gem -> Promis -> Gratis"
            )
    if "intex_factory" in seed:
        raise ValueError("intex_factory was renamed to intex_profile")

    _seed_economic_state(seed, alloc)

    _seed_contract_alloc(alloc, seed, contracts_dir)

    return genesis


def _seed_contract_alloc(alloc, seed, contracts_dir):
    # Seed the profile selectors (auto seeds nothing).
    for section, address, slot, label in (
        ("intex_profile", INTEX_ADDRESS, INTEX_PROFILE_SLOT, "Intex"),
        ("gem_profile", GEM_ADDRESS, GEM_PROFILE_SLOT, "Gem"),
        ("nod_profile", NOD_ADDRESS, NOD_PROFILE_SLOT, "Nod"),
        ("credis_profile", CREDIS_ADDRESS, CREDIS_PROFILE_SLOT, "Credis"),
    ):
        if section not in seed:
            continue
        profile_storage = StorageBuilder()
        seed_profile_selector(profile_storage, seed[section], section, slot)
        if profile_storage.entries:
            entry = alloc.setdefault(address, {})
            entry.setdefault("storage", {}).update(profile_storage.entries)
            entry.setdefault("code", MARKER_CODE)
            print(f"  {label}: {len(profile_storage.entries)} storage entries")

    # Seed externally-fetched contracts (e.g. CREATE2 deployer)
    if "contracts" in seed:
        if contracts_dir is None:
            raise ValueError("contracts_dir is required to seed `contracts`")
        seed_external_contracts(alloc, seed["contracts"], contracts_dir)

    # reth v2.2 `GenesisAccount` requires an explicit `balance` on every alloc
    # entry, including code/storage-only marker accounts (the `0xef` markers and
    # system storage accounts) that the seeders above leave balance-less. Default
    # any such account to zero so the chain spec parses. Accounts that already
    # carry a real balance keep it (setdefault is a no-op for them).
    for account in alloc.values():
        account.setdefault("balance", "0x0")

    validate_stablecoin_namespace_alloc(alloc, require_reserved_markers=True)


def main():
    parser = argparse.ArgumentParser(
        description="Seed genesis.json with precompile storage"
    )
    parser.add_argument("--genesis", required=True, help="Path to genesis.json")
    parser.add_argument("--seed", required=True, help="Path to seed config JSON")
    parser.add_argument(
        "--validators", help="Path to validators.json for genesis validator set"
    )
    parser.add_argument(
        "--output", required=True, help="Output path for patched genesis.json"
    )
    parser.add_argument(
        "--contracts-dir",
        help="Directory containing contract code/state files referenced from "
        "seed['contracts']. Defaults to <seed-file-dir>/contracts.",
    )
    parser.add_argument(
        "--canon-dir",
        help="Directory holding metacanon.md and canon.md to seed into the "
        "Governance precompile at version 1. Defaults to <script-dir>/canon. "
        "When absent, the canon texts start empty and an ACTIVE validator sets them "
        "post-genesis.",
    )
    parser.add_argument(
        "--worldwide-day",
        type=int,
        help="Override the seeded active worldwide-day (YYYYMMDD): its S-curve peak "
        "and NOD references too. Localnet bootstrap passes the genesis date so "
        "the seeded OFFERING day tracks the chain's wall-clock; a stale "
        "hardcoded day desyncs from the per-block "
        "WorldwideDay::from_timestamp(block) and wedges metadosis processing.",
    )
    parser.add_argument(
        "--fresh-metadosis",
        action="store_true",
        help="Do not seed an already-OFFERING WorldwideDay; block 1 creates it "
        "from config.outbeProtocol timings in a test-protocol-overrides node. "
        "Intended for production-shaped E2E.",
    )
    args = parser.parse_args()

    with open(args.genesis) as f:
        genesis = json.load(f)
    with open(args.seed) as f:
        seed = json.load(f)
    validators = []
    if args.validators:
        with open(args.validators) as f:
            validators = json.load(f)

    # Build the declarative config this profile describes and hand it to
    # create_genesis. Thus this CLI creates its genesis through the same path that
    # a yaml-driven deployment uses. The e2e harness and the localnet scripts call
    # this CLI. create_genesis calls apply_seed below to render it.
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import create_genesis

    genesis = create_genesis.seed_genesis_from_config(
        inputs=create_genesis.SeedInputs(
            base_genesis=genesis, seed=seed, validators=validators
        ),
        options=create_genesis.SeedOptions(
            contracts_dir=args.contracts_dir
            or os.path.join(os.path.dirname(os.path.abspath(args.seed)), "contracts"),
            canon_dir=args.canon_dir,
            worldwide_day=args.worldwide_day,
            fresh_metadosis=args.fresh_metadosis,
        ),
    )

    with open(args.output, "w") as f:
        json.dump(genesis, f, indent=2)

    total_storage = sum(len(v.get("storage", {})) for v in genesis["alloc"].values())
    print(f"\nGenesis written to {args.output}")
    print(f"Total storage entries: {total_storage}")


if __name__ == "__main__":
    main()
