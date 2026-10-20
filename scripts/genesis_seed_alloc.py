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


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


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
        _require(
            isinstance(account, dict), f"genesis alloc {raw_address} must be an object"
        )
        normalized[normalized_address] = (raw_address, account)

        _require(
            not normalized_address.startswith(STABLECOIN_ADDRESS_PREFIX),
            f"genesis alloc {raw_address} collides with reserved stablecoin "
            f"prefix 0x{STABLECOIN_ADDRESS_PREFIX}",
        )

    for address in (
        STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
    ):
        item = normalized.get(address)
        if item is not None:
            _validate_reserved_account(
                *item, require_reserved_markers=require_reserved_markers
            )
        else:
            _require(
                not require_reserved_markers,
                f"missing stablecoin reserved account 0x{address}",
            )


def _validate_reserved_account(raw_address, account, *, require_reserved_markers):
    raw_code = account.get("code")
    if raw_code is not None:
        _require(
            str(raw_code).lower() == MARKER_CODE,
            f"stablecoin reserved account {raw_address} has conflicting code",
        )
    elif require_reserved_markers:
        raise ValueError(
            f"stablecoin reserved account {raw_address} is missing marker code"
        )
    _require(
        not account.get("storage"),
        f"stablecoin reserved account {raw_address} has conflicting storage",
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
        _require(
            not nonzero, f"stablecoin reserved account {raw_address} has nonzero {field}"
        )


def seed_code_registry(storage: StorageBuilder, config: dict):
    """Seed immutable CodeRegistry V1 capacity at slot 5."""
    _require(isinstance(config, dict), "code_registry must be a JSON object")
    configured = parse_int(config.get("max_repositories", 0))
    maximum = 0xFFFFFFFF if configured == -1 else configured
    _require(
        1 <= configured <= 0xFFFFFFFE or configured == -1,
        "code_registry.max_repositories must be -1 or in 1..=4294967294",
    )
    storage.set_slot(5, maximum)


def _parse_governments_description(governments: dict) -> bytes:
    fields = {"description", "initial_tree_oid", "initial_content_manifest_sha256"}
    _require(
        isinstance(governments, dict) and set(governments) == fields,
        "code_registry.governments requires exactly " + ", ".join(sorted(fields)),
    )
    description = governments["description"]
    _require(
        isinstance(description, str) and not ({"\r", "\0"} & set(description)),
        "invalid governments description",
    )
    description_bytes = description.encode("utf-8")
    _require(
        len(description_bytes) <= 256, "governments description exceeds 256 UTF-8 bytes"
    )
    return description_bytes


def _parse_governments_commitment(governments: dict, field: str, size: int) -> bytes:
    value = governments[field]
    _require(
        isinstance(value, str)
        and value.startswith("0x")
        and len(value) == 2 + size * 2,
        f"governments {field} must contain {size} hex bytes",
    )
    try:
        raw = bytes.fromhex(value[2:])
    except ValueError as error:
        raise ValueError(f"invalid governments {field}") from error
    _require(any(raw), f"governments {field} must be nonzero")
    return raw


def seed_governments_descriptor(genesis: dict, storage: StorageBuilder, config: dict):
    """Commit the pre-genesis OGD1 artifact without a future RID or group key."""
    if "governments" not in config:
        return
    governments = config["governments"]
    description_bytes = _parse_governments_description(governments)
    chain = genesis.setdefault("config", {})
    chain_id = parse_int(chain["chainId"])
    timestamp = parse_header_timestamp(genesis)
    _require(
        0 <= min(chain_id, timestamp) and max(chain_id, timestamp) <= 0xFFFFFFFFFFFFFFFF,
        "governments chain ID/timestamp must fit u64",
    )
    epoch = parse_int(chain.get("epochLengthBlocks", DEFAULT_EPOCH_LENGTH_BLOCKS))
    prepare = parse_int(
        chain.get("dkgPrepareWindowBlocks", DEFAULT_DKG_PREPARE_WINDOW_BLOCKS)
    )
    _require(
        min(prepare, epoch - min(prepare, epoch) - 64) >= 0,
        "governments requires at least 64 blocks before the next DKG freeze",
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
            _parse_governments_commitment(governments, "initial_tree_oid", 20),
            _parse_governments_commitment(
                governments, "initial_content_manifest_sha256", 32
            ),
            b"\x01\x01",
        )
    )
    encoded = "0x" + descriptor.hex()
    _require(
        chain.get("radicleGovernments", encoded) == encoded,
        "conflicting radicleGovernments genesis descriptor",
    )
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
        _require(
            addr_norm not in PROTECTED_PROTOCOL_ADDRESSES,
            f"contract {entry['address']} collides with a protocol-reserved address; "
            f"refusing to overwrite protocol state",
        )

        code_path = os.path.join(contracts_dir, entry["code"])
        with open(code_path) as f:
            code_hex = f.read().strip()
        _require(
            len(code_hex) > 2 and code_hex.startswith("0x"),
            f"{code_path}: empty or non-0x-prefixed bytecode",
        )

        storage = {}
        if entry.get("state"):
            state_path = os.path.join(contracts_dir, entry["state"])
            with open(state_path) as f:
                raw = json.load(f)
            for slot, value in raw.items():
                storage[hex32(int(slot, 16))] = hex32(int(value, 16))

        target = alloc.setdefault(addr_norm, {})
        _require(
            target.get("code") in (None, MARKER_CODE, code_hex),
            f"alloc entry {addr_norm} already has different non-marker code; "
            f"refusing to overwrite",
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
    _require(
        isinstance(profile, dict), "seed protocol_constants must be a JSON object"
    )
    config = genesis.setdefault("config", {})
    _require(isinstance(config, dict), "genesis config must be a JSON object")
    _require(
        config.get("outbeProtocol", profile) == profile,
        "seed protocol_constants conflicts with genesis config.outbeProtocol",
    )
    config["outbeProtocol"] = json.loads(json.dumps(profile))
