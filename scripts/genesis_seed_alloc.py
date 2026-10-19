"""Validate reserved namespaces and install external contract allocations."""

from __future__ import annotations

import hashlib
import json
import os

from genesis_network import (
    DEFAULT_DKG_PREPARE_WINDOW_BLOCKS,
    DEFAULT_EPOCH_LENGTH_BLOCKS,
)
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
    parse_header_timestamp,
    parse_int,
    write_storage_bytes,
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


def seed_code_registry(storage: StorageBuilder, config: dict):
    """Seed immutable CodeRegistry V1 capacity at slot 5."""
    if not isinstance(config, dict):
        raise ValueError("code_registry must be a JSON object")
    configured = parse_int(config.get("max_repositories", 0))
    maximum = 0xFFFFFFFF if configured == -1 else configured
    if maximum <= 0 or maximum >= 0xFFFFFFFF and configured != -1:
        raise ValueError(
            "code_registry.max_repositories must be -1 or in 1..=4294967294"
        )
    storage.set_slot(5, maximum)


def seed_governments_descriptor(genesis: dict, storage: StorageBuilder, config: dict):
    """Commit the pre-genesis OGD1 artifact without a future RID or group key."""
    if "governments" not in config:
        return
    governments = config["governments"]
    fields = {"description", "initial_tree_oid", "initial_content_manifest_sha256"}
    if not isinstance(governments, dict) or set(governments) != fields:
        raise ValueError(
            "code_registry.governments requires exactly " + ", ".join(sorted(fields))
        )
    description = governments["description"]
    if not isinstance(description, str) or "\r" in description or "\0" in description:
        raise ValueError("invalid governments description")
    description_bytes = description.encode("utf-8")
    if len(description_bytes) > 256:
        raise ValueError("governments description exceeds 256 UTF-8 bytes")

    def commitment(field, size):
        value = governments[field]
        if (
            not isinstance(value, str)
            or not value.startswith("0x")
            or len(value) != 2 + size * 2
        ):
            raise ValueError(f"governments {field} must contain {size} hex bytes")
        try:
            raw = bytes.fromhex(value[2:])
        except ValueError as error:
            raise ValueError(f"invalid governments {field}") from error
        if len(raw) != size or not any(raw):
            raise ValueError(f"governments {field} must be nonzero")
        return raw

    chain = genesis.setdefault("config", {})
    chain_id = parse_int(chain["chainId"])
    timestamp = parse_header_timestamp(genesis)
    if (
        not 0 <= chain_id <= 0xFFFFFFFFFFFFFFFF
        or not 0 <= timestamp <= 0xFFFFFFFFFFFFFFFF
    ):
        raise ValueError("governments chain ID/timestamp must fit u64")
    epoch = parse_int(chain.get("epochLengthBlocks", DEFAULT_EPOCH_LENGTH_BLOCKS))
    prepare = parse_int(
        chain.get("dkgPrepareWindowBlocks", DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
    )
    if epoch <= 0 or prepare < 0 or epoch - min(prepare, epoch) < 64:
        raise ValueError(
            "governments requires at least 64 blocks before the next DKG freeze"
        )
    descriptor = b"".join(
        (
            b"OGD1",
            (1).to_bytes(2, "big"),
            chain_id.to_bytes(8, "big"),
            timestamp.to_bytes(8, "big"),
            (11).to_bytes(2, "big"),
            b"governments",
            (4).to_bytes(2, "big"),
            b"main",
            len(description_bytes).to_bytes(2, "big"),
            description_bytes,
            commitment("initial_tree_oid", 20),
            commitment("initial_content_manifest_sha256", 32),
            b"\x01\x01",
        )
    )
    encoded = "0x" + descriptor.hex()
    if "radicleGovernments" in chain and chain["radicleGovernments"] != encoded:
        raise ValueError("conflicting radicleGovernments genesis descriptor")
    storage.set_raw_slot_hex(10, "0x" + hashlib.sha256(descriptor).hexdigest())
    write_storage_bytes(storage, 11, descriptor)
    chain["radicleGovernments"] = encoded


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
