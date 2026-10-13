"""Validate reserved namespaces and install external contract allocations."""

from __future__ import annotations
import json
import os

from genesis_seed_codec import (
    MARKER_CODE,
    PROFILE_SELECTORS,
    PROTECTED_PROTOCOL_ADDRESSES,
    STABLECOIN_ADDRESS_PREFIX,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    StorageBuilder,
    address_bytes,
    hex32,
    parse_int,
)


def validate_stablecoin_namespace_alloc(
    alloc: dict, *, require_reserved_markers: bool
) -> None:
    """Reject genesis state that collides with the Stablecoin V1 namespace.

    The two fixed accounts may contain only the exact marker and zero-valued
    account metadata. Every dynamic-class allocation is forbidden, including a
    balance-only account, because CREATE/CREATE2 reservation starts at genesis.
    """
    normalized = {}
    for raw_address, account in alloc.items():
        normalized_address = address_bytes(raw_address).hex()
        if normalized_address in normalized:
            raise ValueError(
                "duplicate genesis alloc address after normalization: "
                f"{raw_address} and {normalized[normalized_address][0]}"
            )
        if not isinstance(account, dict):
            raise ValueError(f"genesis alloc {raw_address} must be an object")
        normalized[normalized_address] = (raw_address, account)

        if normalized_address.startswith(STABLECOIN_ADDRESS_PREFIX):
            raise ValueError(
                f"genesis alloc {raw_address} collides with reserved stablecoin "
                f"prefix 0x{STABLECOIN_ADDRESS_PREFIX}"
            )

    for address in (
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
    ):
        item = normalized.get(address)
        if item is None:
            if require_reserved_markers:
                raise ValueError(f"missing stablecoin reserved account 0x{address}")
            continue
        _validate_reserved_account(
            *item, require_reserved_markers=require_reserved_markers
        )


def _validate_reserved_account(raw_address, account, *, require_reserved_markers):
    code = account.get("code")
    if code is not None and str(code).lower() != MARKER_CODE:
        raise ValueError(
            f"stablecoin reserved account {raw_address} has conflicting code"
        )
    if require_reserved_markers and str(code).lower() != MARKER_CODE:
        raise ValueError(
            f"stablecoin reserved account {raw_address} is missing marker code"
        )
    if account.get("storage"):
        raise ValueError(
            f"stablecoin reserved account {raw_address} has conflicting storage"
        )
    _validate_reserved_balance(raw_address, account)


def _validate_reserved_balance(raw_address, account):
    for field in ("balance", "nonce"):
        value = account.get(field, "0x0")
        try:
            nonzero = int(str(value), 0) != 0
        except (TypeError, ValueError) as error:
            raise ValueError(
                f"stablecoin reserved account {raw_address} has invalid {field}"
            ) from error
        if nonzero:
            raise ValueError(
                f"stablecoin reserved account {raw_address} has nonzero {field}"
            )


def seed_radicle_registry(storage: StorageBuilder, config: dict):
    """Seed immutable RadicleRegistry V1 capacity at slot 5."""
    if not isinstance(config, dict):
        raise ValueError("radicle_registry must be a JSON object")
    configured = parse_int(config.get("max_repositories", 0))
    maximum = 0xFFFFFFFF if configured == -1 else configured
    if maximum <= 0 or maximum >= 0xFFFFFFFF and configured != -1:
        raise ValueError(
            "radicle_registry.max_repositories must be -1 or in 1..=4294967294"
        )
    storage.set_slot(5, maximum)


def seed_external_contracts(alloc, contracts_list, contracts_dir):
    """
    Embed contracts from external sources (bytecode + storage) into the genesis
    alloc. Each entry has the form:
        {"address": "0x...", "code": "<file>.code.hex",
         "state": "<file>.state.json"?, "nonce": "0x.."?, "balance": "0x.."?}
    The function reads files from contracts_dir. It checks address keys for
    collisions against the precompile registry to prevent silently overwriting
    protocol state.
    """
    for entry in contracts_list:
        addr_norm = address_bytes(entry["address"]).hex()
        if addr_norm in PROTECTED_PROTOCOL_ADDRESSES:
            raise ValueError(
                f"contract {entry['address']} collides with a protocol-reserved address; "
                f"refusing to overwrite protocol state"
            )

        code_path = os.path.join(contracts_dir, entry["code"])
        with open(code_path) as f:
            code_hex = f.read().strip()
        if not code_hex.startswith("0x") or len(code_hex) <= 2:
            raise ValueError(f"{code_path}: empty or non-0x-prefixed bytecode")

        storage = {}
        if entry.get("state"):
            state_path = os.path.join(contracts_dir, entry["state"])
            with open(state_path) as f:
                raw = json.load(f)
            for slot, value in raw.items():
                storage[hex32(int(slot, 16))] = hex32(int(value, 16))

        target = alloc.setdefault(addr_norm, {})
        existing_code = target.get("code")
        if (
            existing_code is not None
            and existing_code != MARKER_CODE
            and existing_code != code_hex
        ):
            raise ValueError(
                f"alloc entry {addr_norm} already has different non-marker code; "
                f"refusing to overwrite"
            )
        target["code"] = code_hex
        target["nonce"] = entry.get("nonce", "0x1")
        target["balance"] = entry.get("balance", "0x0")
        if storage:
            target.setdefault("storage", {}).update(storage)

        print(
            f"  Contract {entry['address']}: code={(len(code_hex) - 2) // 2} bytes, "
            f"{len(storage)} storage entries"
        )


def seed_protocol_constants(genesis: dict, seed: dict) -> None:
    """Copy optional protocol timing overrides into genesis config.

    Rust resolves defaults, validates the complete record, and installs it in
    immutable process memory during node startup.
    """
    profile = seed.get("protocol_constants")
    if profile is None:
        return
    if not isinstance(profile, dict):
        raise ValueError("seed protocol_constants must be a JSON object")
    config = genesis.setdefault("config", {})
    if not isinstance(config, dict):
        raise ValueError("genesis config must be a JSON object")
    existing = config.get("outbeProtocol")
    if existing is not None and existing != profile:
        raise ValueError(
            "seed protocol_constants conflicts with genesis config.outbeProtocol"
        )
    config["outbeProtocol"] = json.loads(json.dumps(profile))
