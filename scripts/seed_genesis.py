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
Falls back to a small pure-Python Keccak-256 implementation for hermetic
localnet smoke runs when neither optional package is installed.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import sys

# --- Keccak256 ---

_MASK64 = (1 << 64) - 1
_KECCAK_RATE_BYTES = 136  # Keccak-256: rate=1088 bits, capacity=512 bits.
_KECCAK_ROUND_CONSTANTS = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808A,
    0x8000000080008000,
    0x000000000000808B,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008A,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000A,
    0x000000008000808B,
    0x800000000000008B,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800A,
    0x800000008000000A,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
]
_KECCAK_ROTATION_OFFSETS = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
]


def _rotl64(value: int, shift: int) -> int:
    value &= _MASK64
    if shift == 0:
        return value
    return ((value << shift) | (value >> (64 - shift))) & _MASK64


def _keccak_f1600(state: list[int]) -> None:
    """Apply the Keccak-f[1600] permutation in-place."""
    for rc in _KECCAK_ROUND_CONSTANTS:
        c = [
            state[x]
            ^ state[x + 5]
            ^ state[x + 10]
            ^ state[x + 15]
            ^ state[x + 20]
            for x in range(5)
        ]
        d = [c[(x - 1) % 5] ^ _rotl64(c[(x + 1) % 5], 1) for x in range(5)]
        for y in range(5):
            for x in range(5):
                state[x + 5 * y] ^= d[x]

        b = [0] * 25
        for y in range(5):
            for x in range(5):
                b[y + 5 * ((2 * x + 3 * y) % 5)] = _rotl64(
                    state[x + 5 * y], _KECCAK_ROTATION_OFFSETS[x][y]
                )

        for y in range(5):
            for x in range(5):
                state[x + 5 * y] = (
                    b[x + 5 * y]
                    ^ ((~b[((x + 1) % 5) + 5 * y]) & b[((x + 2) % 5) + 5 * y])
                ) & _MASK64

        state[0] ^= rc


def _pure_python_keccak256(data: bytes) -> bytes:
    """Return Ethereum Keccak-256, not FIPS SHA3-256."""
    state = [0] * 25
    padded = bytearray(data)
    pad_len = _KECCAK_RATE_BYTES - (len(padded) % _KECCAK_RATE_BYTES)
    if pad_len == 1:
        padded.append(0x81)
    else:
        padded.append(0x01)
        padded.extend(b"\x00" * (pad_len - 2))
        padded.append(0x80)

    for offset in range(0, len(padded), _KECCAK_RATE_BYTES):
        block = padded[offset : offset + _KECCAK_RATE_BYTES]
        for lane in range(_KECCAK_RATE_BYTES // 8):
            start = lane * 8
            state[lane] ^= int.from_bytes(block[start : start + 8], "little")
        _keccak_f1600(state)

    return b"".join(lane.to_bytes(8, "little") for lane in state)[:32]


try:
    from Crypto.Hash import keccak as _keccak_mod

    def keccak256(data: bytes) -> bytes:
        return _keccak_mod.new(data=data, digest_bits=256).digest()
except ImportError:
    try:
        import sha3

        def keccak256(data: bytes) -> bytes:
            return sha3.keccak_256(data).digest()
    except ImportError:

        def keccak256(data: bytes) -> bytes:
            return _pure_python_keccak256(data)


# --- Precompile addresses ---

GRATIS_ADDRESS = "0000000000000000000000000000000000001003"
GRATIS_FACTORY_ADDRESS = "0000000000000000000000000000000000002003"
PROMIS_ADDRESS = "0000000000000000000000000000000000001337"
TRIBUTE_ADDRESS = "0000000000000000000000000000000000001101"
NOD_ADDRESS = "0000000000000000000000000000000000001006"
METADOSIS_ADDRESS = "000000000000000000000000000000000000100e"
TRIBUTE_FACTORY_ADDRESS = "0000000000000000000000000000000000001100"
AGENT_REWARD_ADDRESS = "000000000000000000000000000000000000100b"
FIDELITY_ADDRESS = "000000000000000000000000000000000000100c"
EMISSION_LIMIT_ADDRESS = "000000000000000000000000000000000000100d"
PROMIS_LIMIT_ADDRESS = "000000000000000000000000000000000000100f"
CYCLE_ADDRESS = "0000000000000000000000000000000000001010"
CCA_ADDRESS = "0000000000000000000000000000000000001011"
MERCHANT_ADDRESS = "0000000000000000000000000000000000001012"
CREDIS_ADDRESS = "000000000000000000000000000000000000100a"
CREDIS_FACTORY_ADDRESS = "0000000000000000000000000000000000001009"
INTEX_FACTORY_ADDRESS = "0000000000000000000000000000000000001015"
VALIDATOR_SET_ADDRESS = "000000000000000000000000000000000000ee00"
SLASH_INDICATOR_ADDRESS = "000000000000000000000000000000000000ee01"
STAKING_ADDRESS = "000000000000000000000000000000000000ee02"
REWARDS_ADDRESS = "000000000000000000000000000000000000ee03"
# V2 Phase 1 accounting progress marker. Mirrors the Rust constant
# `outbe_primitives::addresses::ACCOUNTING_PROGRESS_ADDRESS`. The account has
# no precompile dispatch; the executor relies on the `0xef` marker bytecode
# (deployed via `ALL_PRECOMPILE_ADDRESSES` below) to keep slot 0
# (`last_accounted_block_number: u64`) alive across EIP-161 cleanup.
ACCOUNTING_PROGRESS_ADDRESS = "000000000000000000000000000000000000ee04"
ORACLE_ADDRESS = "000000000000000000000000000000000000ee05"
# ZeroFee paymaster precompile at 0xEE09. Holds per-signer EIP-7702
# sponsorship counters; the precompile itself has dispatch logic in
# `outbe-evm/src/precompiles.rs`, so the marker bytecode below is what
# protects its account (and slot 0) from EIP-161 cleanup before the
# first sponsored tx ever lands.
ZEROFEE_ADDRESS = "000000000000000000000000000000000000ee09"
# TEE registry precompile at 0xEE0A. Genesis seeds only slot 2 (`policy_hash`),
# and only when `tee_policy` is present in the seed config; the rest of the
# registry is written by the block-1 `TeeBootstrap` system tx. The account is
# preserved across EIP-161 at runtime by `OUTBE_RUNTIME_MARKER_ADDRESSES`; when a
# policy is seeded it also gets genesis marker bytecode so slot 2 survives to
# block 1. Mirrors `outbe_primitives::addresses::TEE_REGISTRY_ADDRESS`.
TEE_REGISTRY_ADDRESS = "000000000000000000000000000000000000ee0a"
OUTBE_SYSTEM_TX_ADDRESS = "ff00000000000000000000000000000000000001"

MIN_STAKE = 100_000 * 10**18
DEFAULT_UNBONDING_PERIOD = 21 * 24 * 3600
DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS = 151_200
# ~1 hour at a ~3s block (40 min at 2s … 2.7 h at 8s). The epoch is the cadence
# for DKG reshare, active-set rotation, and the per-epoch slash-counter reset, so
# it bounds the felony window: a felony threshold (default 150) must stay below it.
DEFAULT_EPOCH_LENGTH_BLOCKS = 1_200
SECONDS_PER_DAY = 86_400

# IntexFactory profile selector (config slot 13). Numbers live in Rust
# (crates/core/intexfactory/src/config.rs); genesis only picks one.
INTEX_PROFILE_SELECTORS = {"prod": 0, "dev": 1}

ALL_PRECOMPILE_ADDRESSES = [
    GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS, PROMIS_ADDRESS, TRIBUTE_ADDRESS,
    NOD_ADDRESS, METADOSIS_ADDRESS, TRIBUTE_FACTORY_ADDRESS, AGENT_REWARD_ADDRESS,
    FIDELITY_ADDRESS, EMISSION_LIMIT_ADDRESS, PROMIS_LIMIT_ADDRESS,
    CYCLE_ADDRESS, CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS,
    VALIDATOR_SET_ADDRESS, SLASH_INDICATOR_ADDRESS,
    STAKING_ADDRESS, REWARDS_ADDRESS, ACCOUNTING_PROGRESS_ADDRESS, ORACLE_ADDRESS,
    ZEROFEE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS,
]

# Protocol-owned balance accumulators without precompile dispatch. They are
# collision-protected for genesis tooling but do not receive marker bytecode.
PROTOCOL_ACCUMULATOR_ADDRESSES = [CCA_ADDRESS, MERCHANT_ADDRESS]

PROTECTED_PROTOCOL_ADDRESSES = set(ALL_PRECOMPILE_ADDRESSES + PROTOCOL_ACCUMULATOR_ADDRESSES)

# Marker bytecode for precompile accounts (prevents EIP-161 empty account removal)
MARKER_CODE = "0xef"


# --- Storage slot computation ---

def to_be32(val: int) -> bytes:
    """Encode integer as 32-byte big-endian."""
    return val.to_bytes(32, "big")


def hex32(val: int) -> str:
    """Encode integer as 0x-prefixed 32-byte hex string."""
    return "0x" + val.to_bytes(32, "big").hex()


def mapping_key(key_bytes: bytes, base_slot: int) -> str:
    """
    Compute Solidity-compatible mapping slot.
    slot = keccak256(left_pad(key_bytes, 32) ++ to_be(base_slot, 32))
    """
    padded = key_bytes.rjust(32, b"\x00")
    slot_bytes = to_be32(base_slot)
    h = keccak256(padded + slot_bytes)
    return "0x" + h.hex()


def address_bytes(addr_hex: str) -> bytes:
    """Parse 0x-prefixed address to 20 bytes."""
    addr = addr_hex.lower().replace("0x", "")
    assert len(addr) == 40, f"invalid address length: {addr_hex}"
    return bytes.fromhex(addr)


def u32_bytes(val: int) -> bytes:
    """Encode u32 as 4-byte big-endian."""
    return val.to_bytes(4, "big")


def wwd_to_day_timestamp(wwd: int) -> int:
    """UTC-midnight unix timestamp for a YYYYMMDD worldwide day. Matches the
    runtime's `worldwide_day_to_utc_timestamp` + `truncate_to_day`."""
    import datetime as _dt

    y, m, d = wwd // 10_000, (wwd // 100) % 100, wwd % 100
    return int(_dt.datetime(y, m, d, tzinfo=_dt.timezone.utc).timestamp())


def u64_bytes(val: int) -> bytes:
    """Encode u64 as 8-byte big-endian."""
    return val.to_bytes(8, "big")


def b256_bytes(hex_str: str) -> bytes:
    """Parse 0x-prefixed B256 to 32 bytes."""
    h = hex_str.lower().replace("0x", "")
    assert len(h) == 64, f"invalid B256 length: {hex_str}"
    return bytes.fromhex(h)


def compute_tee_policy_hash(
    allowed_mrsigner: list, allowed_mrenclave: list, min_isv_svn: int
) -> bytes:
    """Canonical genesis TEE policy hash.

    MUST match `outbe_primitives::tee_bootstrap::TeePolicy::compute_hash`:
    `keccak256(b"outbe/tee/policy/v1" || u16_be(len(mrsigner)) || sorted(mrsigner)
    || u16_be(len(mrenclave)) || sorted(mrenclave) || u16_be(min_isv_svn))`.
    Lists are sorted ascending by their 32 raw bytes (matching Rust's `B256`
    `sort_unstable`), so the hash is independent of allowlist ordering.
    """
    signers = sorted(b256_bytes(s) for s in allowed_mrsigner)
    enclaves = sorted(b256_bytes(e) for e in allowed_mrenclave)
    buf = b"outbe/tee/policy/v1"
    buf += len(signers).to_bytes(2, "big")
    for s in signers:
        buf += s
    buf += len(enclaves).to_bytes(2, "big")
    for e in enclaves:
        buf += e
    buf += int(min_isv_svn).to_bytes(2, "big")
    return keccak256(buf)


def pubkey_bytes(hex_str: str) -> bytes:
    """Parse a 48-byte BLS MinPk public key."""
    h = hex_str.lower().replace("0x", "")
    if len(h) != 96:
        raise ValueError(f"invalid BLS public key length: {hex_str}")
    return bytes.fromhex(h)


def address_as_u256(addr_hex: str) -> int:
    """Convert address to U256 (right-aligned in 32 bytes)."""
    return int(addr_hex, 16)


def parse_int(val) -> int:
    """Parse string or int to Python int."""
    if isinstance(val, int):
        return val
    if isinstance(val, str):
        if val.startswith("0x"):
            return int(val, 16)
        return int(val)
    raise ValueError(f"cannot parse as int: {val}")


def parse_genesis_timestamp(genesis: dict) -> int:
    """Parse ``config.genesisTime`` (ISO 8601 UTC) as a unix timestamp."""
    from datetime import datetime
    config = genesis.get("config", {})
    genesis_time_str = config.get("genesisTime")
    if not genesis_time_str:
        genesis_time_str = datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")
    dt = datetime.fromisoformat(genesis_time_str.replace("Z", "+00:00"))
    return int(dt.timestamp())


def timestamp_to_utc_date_key(timestamp: int) -> int:
    """Convert a unix timestamp to a UTC yyyymmdd date key."""
    if timestamp < 0:
        raise ValueError(f"genesis timestamp must be non-negative: {timestamp}")
    return civil_date_from_days(timestamp // SECONDS_PER_DAY)


def civil_date_from_days(days_since_epoch: int) -> int:
    """Integer UTC calendar conversion matching outbe_primitives::time."""
    z = days_since_epoch + 719468
    era = z // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    y = yoe + era * 400
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp = (5 * doy + 2) // 153
    d = doy - (153 * mp + 2) // 5 + 1
    m = mp + 3 if mp < 10 else mp - 9
    if m <= 2:
        y += 1
    return y * 10000 + m * 100 + d


def alloc_balance_hex(amount: int) -> str:
    """Encode an account balance as compact 0x-prefixed hex."""
    return hex(amount)


# --- Storage builder ---

class StorageBuilder:
    """Accumulates storage entries for a contract address."""

    def __init__(self):
        self.entries: dict[str, str] = {}

    def set_slot(self, slot: int, value: int):
        """Set a direct slot value."""
        self.entries[hex32(slot)] = hex32(value)

    def set_raw_slot(self, slot: int | str, value: int):
        """Set a storage slot by integer slot or 0x-prefixed slot key."""
        key = hex32(slot) if isinstance(slot, int) else slot.lower()
        self.entries[key] = hex32(value)

    def set_raw_slot_hex(self, slot: int | str, value_hex: str):
        """Set a storage slot to an already encoded 0x-prefixed 32-byte value."""
        key = hex32(slot) if isinstance(slot, int) else slot.lower()
        value = value_hex.lower()
        assert value.startswith("0x") and len(value) == 66, f"invalid storage word: {value_hex}"
        self.entries[key] = value

    def set_mapping(self, base_slot: int, key_bytes: bytes, value: int):
        """Set a mapping entry."""
        k = mapping_key(key_bytes, base_slot)
        self.entries[k] = hex32(value)

    def set_mapping_b256(self, base_slot: int, key_bytes: bytes, value_bytes: bytes):
        """Set a mapping entry with a B256 value."""
        k = mapping_key(key_bytes, base_slot)
        self.entries[k] = "0x" + value_bytes.hex()


def data_slot(base_slot: int) -> int:
    """Solidity dynamic bytes/string data slot: keccak256(base_slot)."""
    return int.from_bytes(keccak256(to_be32(base_slot)), "big")


def write_storage_bytes(storage: StorageBuilder, base_slot: int | str, data: bytes):
    """Write Solidity-compatible bytes/string storage at a direct base slot."""
    slot_int = base_slot if isinstance(base_slot, int) else int(base_slot, 16)
    length = len(data)

    if length <= 31:
        word = bytearray(32)
        word[:length] = data
        word[31] = length * 2
        storage.set_raw_slot_hex(base_slot, "0x" + word.hex())
        return

    storage.set_raw_slot(base_slot, length * 2 + 1)
    start = data_slot(slot_int)
    for i in range((length + 31) // 32):
        chunk = data[i * 32:(i + 1) * 32]
        word = bytearray(32)
        word[:len(chunk)] = chunk
        storage.set_raw_slot_hex(start + i, "0x" + word.hex())


def write_mapping_string(storage: StorageBuilder, base_slot: int, key_bytes: bytes, value: str):
    """Write Mapping<K, StorageBytes>::write_string-compatible metadata."""
    write_storage_bytes(storage, mapping_key(key_bytes, base_slot), value.encode())


def write_mapping_bytes(storage: StorageBuilder, base_slot: int, key_bytes: bytes, value: bytes):
    """Write Mapping<K, StorageBytes>::write-compatible metadata."""
    write_storage_bytes(storage, mapping_key(key_bytes, base_slot), value)


P2P_ADDRESS_VERSION_V1 = 1
MAX_P2P_ADDRESS_ENCODED_LEN = 512


def parse_host_port(value: str) -> tuple[str, int]:
    """Parse host:port, including [IPv6]:port."""
    if value.startswith("["):
        end = value.find("]")
        if end < 0 or end + 1 >= len(value) or value[end + 1] != ":":
            raise ValueError(f"invalid socket address: {value}")
        host = value[1:end]
        port_s = value[end + 2:]
    else:
        if ":" not in value:
            raise ValueError(f"invalid socket address: {value}")
        host, port_s = value.rsplit(":", 1)
    try:
        port = int(port_s)
    except ValueError as exc:
        raise ValueError(f"invalid port in socket address: {value}") from exc
    if port <= 0 or port > 65535:
        raise ValueError(f"invalid port in socket address: {value}")
    return host, port


def validate_hostname(host: str):
    if not host or len(host) > 253:
        raise ValueError(f"invalid DNS hostname: {host}")
    for label in host.split("."):
        if not label or len(label) > 63:
            raise ValueError(f"invalid DNS hostname: {host}")
        if label[0] == "-" or label[-1] == "-":
            raise ValueError(f"invalid DNS hostname: {host}")
        if not all(ch.isascii() and (ch.isalnum() or ch == "-") for ch in label):
            raise ValueError(f"invalid DNS hostname: {host}")


def encode_p2p_socket(value: str) -> bytes:
    host, port = parse_host_port(value)
    ip = ipaddress.ip_address(host)
    if ip.version == 4:
        return bytes([4]) + ip.packed + port.to_bytes(2, "big")
    if ip.version == 6:
        return bytes([6]) + ip.packed + port.to_bytes(2, "big")
    raise ValueError(f"unsupported ip version in socket address: {value}")


def encode_p2p_ingress(value) -> bytes:
    if isinstance(value, str):
        return bytes([0]) + encode_p2p_socket(value)
    if isinstance(value, dict):
        if "socket" in value:
            return bytes([0]) + encode_p2p_socket(value["socket"])
        if "dns" in value:
            dns = value["dns"]
            host = dns["host"]
            port = parse_int(dns["port"])
            if port <= 0 or port > 65535:
                raise ValueError(f"invalid DNS ingress port: {port}")
            validate_hostname(host)
            encoded_host = host.encode()
            return (
                bytes([1])
                + len(encoded_host).to_bytes(2, "big")
                + encoded_host
                + port.to_bytes(2, "big")
            )
    raise ValueError(f"invalid p2p ingress value: {value}")


def encode_p2p_address_payload(value) -> tuple[int, bytes] | None:
    """Encode a validator p2p address seed into Outbe versioned bytes."""
    if value is None:
        return None
    if isinstance(value, str):
        payload = bytes([0]) + encode_p2p_socket(value)
    elif isinstance(value, dict):
        if "symmetric" in value:
            payload = bytes([0]) + encode_p2p_socket(value["symmetric"])
        elif "asymmetric" in value:
            asymmetric = value["asymmetric"]
            payload = (
                bytes([1])
                + encode_p2p_ingress(asymmetric["ingress"])
                + encode_p2p_socket(asymmetric["egress"])
            )
        else:
            raise ValueError(f"invalid p2p_address object: {value}")
    else:
        raise ValueError(f"invalid p2p_address value: {value}")
    if len(payload) > MAX_P2P_ADDRESS_ENCODED_LEN:
        raise ValueError(
            f"p2p_address payload exceeds {MAX_P2P_ADDRESS_ENCODED_LEN} bytes"
        )
    return P2P_ADDRESS_VERSION_V1, payload


def pair_hash(base: str, quote: str) -> bytes:
    """Oracle pair hash: keccak256('BASE/QUOTE')."""
    if not base or not quote:
        raise ValueError("oracle pair base/quote must not be empty")
    if "/" in base or "/" in quote:
        raise ValueError("oracle pair base/quote must not contain '/'")
    return keccak256(f"{base}/{quote}".encode())


# --- Seeders ---

def seed_gratis(storage: StorageBuilder, balances: dict):
    """
    Gratis storage layout:
      slot 0: total_supply (U256)
      slot 1: mapping(address => U256) balances (available)
      slot 2: mapping(address => U256) pledged_balances (not seeded here)
    """
    total = 0
    for addr, amount_str in balances.items():
        amount = parse_int(amount_str)
        total += amount
        storage.set_mapping(1, address_bytes(addr), amount)
    storage.set_slot(0, total)


def seed_promis(storage: StorageBuilder, balances: dict):
    """
    Promis storage layout:
      slot 0: total_supply (U256)
      slot 1: mapping(address => U256) balances
    """
    total = 0
    for addr, amount_str in balances.items():
        amount = parse_int(amount_str)
        total += amount
        storage.set_mapping(1, address_bytes(addr), amount)
    storage.set_slot(0, total)


def seed_coen(alloc: dict, balances: dict):
    """
    native EVM token alloc layout:
      alloc[addr].balance: U256 wei
    """
    for addr, amount_str in balances.items():
        amount = parse_int(amount_str)
        alloc.setdefault(address_bytes(addr).hex(), {})["balance"] = alloc_balance_hex(amount)


def tribute_token_id(owner: str, worldwide_day: int) -> bytes:
    """Generate tribute token_id = keccak256(owner_20B ++ wwd_4B)."""
    buf = address_bytes(owner) + u32_bytes(worldwide_day)
    return keccak256(buf)


def day_index_key(day: int, index: int) -> bytes:
    """keccak256(day_4B ++ index_4B) for tribute day index."""
    buf = u32_bytes(day) + u32_bytes(index)
    return keccak256(buf)


def owner_index_key(owner: str, index: int) -> bytes:
    """keccak256(owner_20B ++ index_4B) for tribute/nod owner index."""
    buf = address_bytes(owner) + u32_bytes(index)
    return keccak256(buf)


def seed_tributes(storage: StorageBuilder, tributes: list):
    """
    Tribute storage layout:
      slot 0: total_supply (u64)
      slot 1: mapping(B256 => Address) owners
      slot 2: mapping(B256 => u32) worldwide_days
      slot 3: mapping(B256 => U256) issuance_amounts
      slot 4: mapping(B256 => u32) settlement_currencies
      slot 5: mapping(B256 => U256) nominal_amounts
      slot 6: mapping(u32 => u32) day_tribute_counts
      slot 7: mapping(u32 => U256) day_nominal_amounts
      slot 8: mapping(u32 => bool) day_blocked
      slot 9: mapping(B256 => B256) day_token_ids
      slot 10: mapping(Address => u32) owner_tribute_counts
      slot 11: mapping(B256 => B256) owner_tribute_ids
    """
    # Track per-day and per-owner counters
    day_counts: dict[int, int] = {}
    day_nominals: dict[int, int] = {}
    owner_counts: dict[str, int] = {}

    for tribute in tributes:
        owner = tribute["owner"]
        wwd = tribute["worldwide_day"]
        settlement = parse_int(tribute["issuance_amount"])
        currency = tribute["issuance_currency"]
        nominal = parse_int(tribute["nominal_amount"])

        # Generate token_id
        token_id = tribute_token_id(owner, wwd)

        # Store tribute fields
        storage.set_mapping(1, token_id, address_as_u256(owner))
        storage.set_mapping(2, token_id, wwd)
        storage.set_mapping(3, token_id, settlement)
        storage.set_mapping(4, token_id, currency)
        storage.set_mapping(5, token_id, nominal)

        # Day index (slot 9)
        day_idx = day_counts.get(wwd, 0)
        di_key = day_index_key(wwd, day_idx)
        storage.set_mapping_b256(9, di_key, token_id)
        day_counts[wwd] = day_idx + 1

        # Day nominal accumulator
        day_nominals[wwd] = day_nominals.get(wwd, 0) + nominal

        # Owner index (slot 11)
        owner_lower = owner.lower()
        oi = owner_counts.get(owner_lower, 0)
        oi_key = owner_index_key(owner, oi)
        storage.set_mapping_b256(11, oi_key, token_id)
        owner_counts[owner_lower] = oi + 1

    # Write day counts and nominals
    for wwd, count in day_counts.items():
        storage.set_mapping(6, u32_bytes(wwd), count)
    for wwd, nominal_total in day_nominals.items():
        storage.set_mapping(7, u32_bytes(wwd), nominal_total)

    # Write owner counts
    for owner, count in owner_counts.items():
        storage.set_mapping(10, address_bytes(owner), count)

    # Total supply
    storage.set_slot(0, len(tributes))


def seed_tribute_day_totals(storage: StorageBuilder, days: list[int]):
    """Initialize the Tribute `day_totals` DSL record for OFFERING days so
    `offerTribute` is accepted: `ensure_day_accepts_tributes` requires
    `initialized == true && !is_sealed`, and a directly-seeded OFFERING worldwide
    day never ran the metadosis `unseal_day` that normally initializes it.

    `day_totals` is `Map<WorldwideDay, DayTotals>` at TributeContract slot 8
    (storage_schema cumulative offsets: `total_supply`@0 = 1 slot, then
    `tributes: Map<_, TributeData>` reserves `TributeData::SLOTS` = 7 slots
    (1..7), so `day_totals` lands at slot 8). Within the `DayTotals` record the
    field offset is the cumulative slot index by `#[attribute(order)]`:
    `initialized`@0, `tribute_count`@1, `tribute_nominal_amount`@2,
    `is_sealed`@3 (its `order = 4` only sorts; the gap at 3 is not reserved).
    So `day_totals[wwd].initialized` is `Mapping(base_slot=8).get(wwd)`; writing
    1 makes the record exist + initialized, with `is_sealed` left at its `false`
    default (slot 11)."""
    for wwd in days:
        storage.set_mapping(8, u32_bytes(wwd), 1)


def nod_id_gen(owner: str, worldwide_day: int, index: int) -> bytes:
    """Generate nod_id = keccak256(owner_20B ++ wwd_4B ++ index_4B)."""
    buf = address_bytes(owner) + u32_bytes(worldwide_day) + u32_bytes(index)
    return keccak256(buf)


def nod_bucket_key(wwd: int, league_id: int, floor_price: int) -> bytes:
    """Compute bucket key = keccak256(wwd_4B ++ league_id_4B ++ floor_price_32B)."""
    buf = u32_bytes(wwd) + u32_bytes(league_id) + to_be32(floor_price)
    return keccak256(buf)


def seed_nods(storage: StorageBuilder, nods: list):
    """
    Nod storage layout:
      slot 0: total_supply (u64)
      slot 1: mapping(B256 => Address) item_owners
      slot 2: mapping(B256 => U256) item_gratis_loads
      slot 3: mapping(B256 => u32) item_worldwide_days
      slot 4: mapping(B256 => u32) item_league_ids
      slot 5: mapping(B256 => U256) item_floor_prices
      slot 6: mapping(B256 => B256) item_bucket_keys
      slot 7: mapping(B256 => U256) bucket_floor_prices
      slot 8: mapping(B256 => u64) bucket_total_nods
      slot 9: mapping(B256 => bool) bucket_is_qualified
      slot 10: mapping(Address => u32) owner_nod_counts
      slot 11: mapping(B256 => B256) owner_nod_ids
    """
    # Track counters
    owner_counts: dict[str, int] = {}
    # bucket totals: keyed by bucket_key bytes
    bucket_totals: dict[bytes, int] = {}

    for i, nod in enumerate(nods):
        owner = nod["owner"]
        gratis_load = parse_int(nod["gratis_load"])
        wwd = nod["worldwide_day"]
        league_id = nod["league_id"]
        floor_price = parse_int(nod["floor_price"])

        # Generate nod_id
        nod_id = nod_id_gen(owner, wwd, i)

        # Bucket key
        bk = nod_bucket_key(wwd, league_id, floor_price)

        # Item fields
        storage.set_mapping(1, nod_id, address_as_u256(owner))
        storage.set_mapping(2, nod_id, gratis_load)
        storage.set_mapping(3, nod_id, wwd)
        storage.set_mapping(4, nod_id, league_id)
        storage.set_mapping(5, nod_id, floor_price)
        storage.set_mapping_b256(6, nod_id, bk)

        # Bucket
        bk_tuple = bytes(bk)
        bucket_totals[bk_tuple] = bucket_totals.get(bk_tuple, 0) + 1
        # Store floor price for this bucket (idempotent)
        storage.set_mapping(7, bk, floor_price)

        # Owner index (slots 10-11)
        owner_lower = owner.lower()
        oi = owner_counts.get(owner_lower, 0)
        oi_key = owner_index_key(owner, oi)
        storage.set_mapping_b256(11, oi_key, nod_id)
        owner_counts[owner_lower] = oi + 1

    # Write bucket totals
    for bk_bytes, total in bucket_totals.items():
        storage.set_mapping(8, bk_bytes, total)

    # Write owner counts
    for owner, count in owner_counts.items():
        storage.set_mapping(10, address_bytes(owner), count)

    # Total supply
    storage.set_slot(0, len(nods))


def seed_metadosis(storage: StorageBuilder, config: dict):
    """
    Metadosis storage layout:
      slot 0: bootstrap_end_time (u64)
      slot 1: mapping(u32 => u8) wwd_status
      slot 2: mapping(u32 => u8) wwd_day_type
      slot 3: mapping(u32 => u64) wwd_forming_start
      slot 4: mapping(u32 => u64) wwd_forming_end
      slot 5: mapping(u32 => u64) wwd_lookback_end
      slot 6: mapping(u32 => u64) wwd_offering_end
      slot 7: mapping(u32 => u64) wwd_scheduled_process_time
      slot 8: mapping(u32 => U256) wwd_previous_vwap
      slot 9: mapping(u32 => U256) wwd_current_vwap
      slot 10: mapping(u32 => U256) day_limit_amount
      slot 11: mapping(u32 => bool) day_limit_used
      slot 12: active_wwd_count (u32)
      slot 13: mapping(u32 => u32) active_wwds (index => wwd)
    """
    wwds = config.get("worldwide_days", [])

    for idx, entry in enumerate(wwds):
        wwd = entry["wwd"]
        wwd_key = u32_bytes(wwd)

        storage.set_mapping(1, wwd_key, entry.get("status", 0))
        storage.set_mapping(2, wwd_key, entry.get("day_type", 0))
        storage.set_mapping(3, wwd_key, entry.get("forming_start", 0))
        storage.set_mapping(4, wwd_key, entry.get("forming_end", 0))
        storage.set_mapping(5, wwd_key, entry.get("lookback_end", 0))
        storage.set_mapping(6, wwd_key, entry.get("offering_end", 0))
        storage.set_mapping(7, wwd_key, entry.get("scheduled_process_time", 0))

        prev_vwap = parse_int(entry.get("previous_vwap", "0"))
        curr_vwap = parse_int(entry.get("current_vwap", "0"))
        storage.set_mapping(8, wwd_key, prev_vwap)
        storage.set_mapping(9, wwd_key, curr_vwap)

        # Day limit
        day_limit = parse_int(entry.get("day_limit", "0"))
        if day_limit > 0:
            storage.set_mapping(10, wwd_key, day_limit)

        # Active WWD list
        storage.set_mapping(13, u32_bytes(idx), wwd)

    # Active WWD count
    storage.set_slot(12, len(wwds))

    # Bootstrap end time
    bootstrap_end = config.get("bootstrap_end_time", 0)
    if bootstrap_end:
        storage.set_slot(0, bootstrap_end)


def seed_validator_set(
    storage: StorageBuilder,
    validators: list[dict],
    config: dict,
    *,
    epoch_length_blocks: int,
    epoch_start_timestamp: int,
    min_stake: int,
    validator_stake: int,
):
    """
    ValidatorSet storage layout:
      slots 0-4: config
      slots 5-18: per-validator mappings and reverse indexes
      slot 20: validator_count
      slots 21-26: epoch / consensus-set tracking
      slot 27: re-registration cooldown
      slots 28-29: versioned Commonware P2P address registry
    """
    storage.set_slot(0, address_as_u256(config.get("owner", "0x0000000000000000000000000000000000000000")))
    storage.set_slot(1, parse_int(config.get("max_validators", 128)))
    if "epoch_duration" in config:
        raise ValueError("validator_set.epoch_duration is deprecated; use config.epochLengthBlocks")
    if "epoch_length_blocks" in config:
        raise ValueError("validator_set.epoch_length_blocks is deprecated; use config.epochLengthBlocks")
    storage.set_slot(2, epoch_length_blocks)
    storage.set_slot(3, min_stake)
    storage.set_slot(4, 1)
    storage.set_slot(20, len(validators))
    storage.set_slot(21, parse_int(config.get("epoch_number", 0)))
    storage.set_slot(22, parse_int(config.get("epoch_start_timestamp", epoch_start_timestamp)))
    storage.set_slot(23, parse_int(config.get("epoch_start_block", 0)))
    storage.set_slot(25, 0)
    storage.set_slot(26, parse_int(config.get("active_consensus_set_hash", 0)))
    storage.set_slot(27, parse_int(config.get(
        "reregistration_cooldown_blocks",
        DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS,
    )))

    for index, validator in enumerate(validators, start=1):
        addr = validator["address"]
        pk = pubkey_bytes(validator["public_key"])
        pk_hi = pk[32:] + (b"\x00" * 16)
        pk_hash = keccak256(pk)

        storage.set_mapping_b256(5, address_bytes(addr), pk[:32])
        storage.set_mapping_b256(6, address_bytes(addr), pk_hi)
        storage.set_mapping(7, address_bytes(addr), validator_stake)
        storage.set_mapping(8, address_bytes(addr), 2)  # ACTIVE
        storage.set_mapping(13, address_bytes(addr), 0)
        storage.set_mapping(16, address_bytes(addr), index)
        storage.set_mapping(17, u64_bytes(index), address_as_u256(addr))
        storage.set_mapping(18, pk_hash, address_as_u256(addr))
        storage.set_mapping(24, address_bytes(addr), 1)
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
    min_stake: int,
    validator_stake: int,
):
    """
    Staking storage layout:
      slots 0-2: config
      slot 3: mapping(validator => stake_amount)
      slot 4: total_staked
    """
    if validator_stake < min_stake:
        raise ValueError("genesis_validator_stake must be >= min_stake")

    storage.set_slot(0, min_stake)
    unbonding_period = parse_int(config.get("unbonding_period", DEFAULT_UNBONDING_PERIOD))
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

    NOTE: `genesis_utc_day` moved from slot 1 to slot 0 when the leading
    `pending_rewards` field was removed (PR #12 / 941c4eb). The runtime also
    lazily anchors this value at block 0 via `rewards::ensure_genesis_anchor`
    (= timestamp_to_date_key(block0.timestamp)); seeding it here keeps genesis
    state explicit and matches that block-0 value.
    """
    storage.set_slot(0, timestamp_to_utc_date_key(genesis_timestamp))


def seed_tee_policy(genesis: dict, alloc: dict, seed: dict):
    """Seed the genesis TEE attestation policy (WS-B), if `tee_policy` is present
    in the seed config.

    Writes two places:
      1. `TeeRegistry` (0xEE0A) slot 2 = `policy_hash` — the consensus-critical,
         deterministic gate the Phase 3b `TeeBootstrap` handler reads from EVM
         state. The account also gets marker bytecode so the slot survives
         EIP-161 cleanup until block 1.
      2. `config.teePolicy` — read by the node at startup to build the host
         `QuotePolicy` (defense-in-depth measurement check at enclave connect).

    No-op when `tee_policy` is absent: genesis is unchanged and the handler skips
    measurement enforcement (slot 2 == ZERO).
    """
    policy = seed.get("tee_policy")
    if not policy:
        return
    allowed_mrsigner = policy.get("allowed_mrsigner", [])
    allowed_mrenclave = policy.get("allowed_mrenclave", [])
    min_isv_svn = parse_int(policy.get("min_isv_svn", 0))
    policy_hash = compute_tee_policy_hash(allowed_mrsigner, allowed_mrenclave, min_isv_svn)

    storage = StorageBuilder()
    storage.set_raw_slot_hex(2, "0x" + policy_hash.hex())
    entry = alloc.setdefault(TEE_REGISTRY_ADDRESS, {})
    entry["code"] = MARKER_CODE
    entry.setdefault("balance", "0x0")
    entry.setdefault("storage", {}).update(storage.entries)

    genesis.setdefault("config", {})["teePolicy"] = {
        "allowed_mrsigner": allowed_mrsigner,
        "allowed_mrenclave": allowed_mrenclave,
        "min_isv_svn": min_isv_svn,
    }
    print(
        f"  teePolicy: {len(allowed_mrsigner)} mrsigner, {len(allowed_mrenclave)} mrenclave, "
        f"min_isv_svn={min_isv_svn}, policy_hash=0x{policy_hash.hex()}"
    )


def seed_zerofee(storage: StorageBuilder):
    """
    ZeroFee paymaster storage layout:
      slot 0: schema version (uint32) — pinned at 1 for the initial
              `Map<Address, u64> counter` layout. The macro's
              `counter` Map keys are `keccak256(addr || base_slot)` so
              they never collide with slot 0 even though `counter`
              nominally uses slot 0 as the base_slot for keccak.

    The slot-0 schema marker is required by the README rule
    "All precompiles ... storage versioned (slot 0 = version)". A
    future layout migration would bump this value and key off it from
    the runtime.
    """
    storage.set_slot(0, 1)


def seed_accounting_progress(storage: StorageBuilder):
    """
    Accounting progress storage layout (V2):
      slot 0: last_accounted_block_number (u64) — pre-V2 genesis is `0`
              meaning Phase 1 has not yet processed any block. The first
              certified-parent accounting begin-zone system transaction
              advances this slot for block N >= 2.

    Genesis V2 requires this slot to be explicitly written so the resulting
    storage map contains the canonical zero word, matching the Rust schema
    `outbe_accounting::schema::Accounting::last_accounted_block_number`.
    """
    storage.set_slot(0, 0)


def seed_oracle(storage: StorageBuilder, config: dict):
    """
    Oracle storage layout:
      slots 0-7: config
      slot 8: pair_count
      slot 9: mapping(pair_id => pair_hash)
      slot 10: mapping(pair_hash => pair_id)
      slot 11: mapping(pair_hash => is_vote_target)
      slots 12-14: exchange_rate / block / timestamp
      slot 15: feeder delegations
      slots 33-34: protected validators
      slots 41-43: settlement currency runtime mappings
      slots 44-47: reversible pair/settlement metadata
      slot 55: reference_currencies (StorageVec<u16>)
    """
    cfg = config.get("config", {})
    storage.set_slot(0, parse_int(cfg.get("vote_period", 2)))
    storage.set_slot(1, parse_int(cfg.get("reward_band", "20000000000000000")))
    penalties_enabled = cfg.get("penalties_enabled", True)
    if penalties_enabled:
        min_valid_per_window = cfg.get("min_valid_per_window", "50000000000000000")
        slash_fraction = cfg.get("slash_fraction", "0")
    else:
        min_valid_per_window = "0"
        slash_fraction = "0"
    storage.set_slot(2, parse_int(cfg.get("slash_window", 96)))
    storage.set_slot(3, parse_int(min_valid_per_window))
    storage.set_slot(4, parse_int(slash_fraction))
    storage.set_slot(5, parse_int(cfg.get("lookback_duration", 86400)))
    storage.set_slot(6, 1 if cfg.get("enabled", True) else 0)
    storage.set_slot(7, 1 if cfg.get("initialized", True) else 0)

    pair_hashes: dict[tuple[str, str], bytes] = {}
    pair_ids: dict[tuple[str, str], int] = {}
    pairs = config.get("pairs", [])
    storage.set_slot(8, len(pairs))

    for idx, pair in enumerate(pairs, start=1):
        base = pair["base"]
        quote = pair["quote"]
        h = pair_hash(base, quote)
        key = (base, quote)
        if key in pair_hashes:
            raise ValueError(f"duplicate oracle pair: {base}/{quote}")
        pair_hashes[key] = h
        pair_ids[key] = idx

        storage.set_mapping_b256(9, u32_bytes(idx), h)
        storage.set_mapping(10, h, idx)
        storage.set_mapping(11, h, 1 if pair.get("vote_target", True) else 0)
        # pair_id_to_base / pair_id_to_quote (macro slots 43/44).
        write_mapping_string(storage, 43, u32_bytes(idx), base)
        write_mapping_string(storage, 44, u32_bytes(idx), quote)

        rate = parse_int(pair.get("initial_rate", "0"))
        if rate:
            storage.set_mapping(12, h, rate)
            storage.set_mapping(13, h, parse_int(pair.get("initial_block", 0)))
            storage.set_mapping(14, h, parse_int(pair.get("initial_timestamp", 0)))

    for rate_entry in config.get("initial_rates", []):
        key = (rate_entry["base"], rate_entry["quote"])
        h = pair_hashes.get(key)
        if h is None:
            raise ValueError(f"initial rate pair is not registered: {key[0]}/{key[1]}")
        storage.set_mapping(12, h, parse_int(rate_entry["rate"]))
        storage.set_mapping(13, h, parse_int(rate_entry.get("block", 0)))
        storage.set_mapping(14, h, parse_int(rate_entry.get("timestamp", 0)))

    for delegation in config.get("feeder_delegations", []):
        validator = delegation["validator"]
        feeder = delegation["feeder"]
        storage.set_mapping(15, address_bytes(validator), address_as_u256(feeder))

    protected = config.get("protected_validators", [])
    if protected:
        # config_allow_protected (macro slot 33) / protected_validator (slot 32).
        storage.set_slot(33, 1)
        for validator in protected:
            storage.set_mapping(32, address_bytes(validator), 1)

    settlements = config.get("settlement_currencies", [])
    seen_iso: set[int] = set()
    # settlement_count (macro slot 40).
    storage.set_slot(40, len(settlements))
    for idx, settlement in enumerate(settlements):
        iso_code = parse_int(settlement["iso_code"])
        if iso_code == 0:
            raise ValueError("oracle settlement iso_code must be non-zero")
        if iso_code in seen_iso:
            raise ValueError(f"duplicate oracle settlement iso_code: {iso_code}")
        seen_iso.add(iso_code)

        denom = settlement["denom"]
        pair_base = settlement["pair_base"]
        pair_quote = settlement["pair_quote"]
        pair = (pair_base, pair_quote)
        h = pair_hashes.get(pair)
        if h is None:
            raise ValueError(f"settlement pair is not registered: {pair_base}/{pair_quote}")

        # settlement_iso_to_denom (41) / settlement_iso_to_pair (42) /
        # settlement_index_to_iso (45) / settlement_iso_to_denom_string (46).
        storage.set_mapping_b256(41, u32_bytes(iso_code), keccak256(denom.encode()))
        storage.set_mapping_b256(42, u32_bytes(iso_code), h)
        storage.set_mapping(45, u32_bytes(idx), iso_code)
        write_mapping_string(storage, 46, u32_bytes(iso_code), denom)

    # S-curve genesis seeds (macro slots 34-38). `resolve_tribute_price` reads
    # `max(per-day VWAP, S-curve)`; pre-seeded OFFERING days have no runtime-
    # computed per-day VWAP, so without an S-curve entry the price is 0 and
    # `offerTribute` reverts with `NominalPriceUnavailable`. Each seed gives a
    # pair a peak at a worldwide day so days within the S-curve period resolve.
    scurve_seeds = config.get("scurve_seeds", [])
    if scurve_seeds:
        storage.set_slot(34, len(scurve_seeds))  # scurve_count
        storage.set_slot(38, 0)  # scurve_oldest_idx
        for idx, sc in enumerate(scurve_seeds):
            pair = (sc["pair_base"], sc["pair_quote"])
            pid = pair_ids.get(pair)
            if pid is None:
                raise ValueError(
                    f"scurve seed pair is not registered: {pair[0]}/{pair[1]}"
                )
            peak_day_ts = wwd_to_day_timestamp(parse_int(sc["peak_day"]))
            storage.set_mapping(35, u32_bytes(idx), pid)  # scurve_pair_id
            storage.set_mapping(36, u32_bytes(idx), peak_day_ts)  # scurve_peak_day
            storage.set_mapping(
                37, u32_bytes(idx), parse_int(sc["peak_price"])
            )  # scurve_peak_price

    # Reference currencies (slot 55): hard-coded protocol default [840] = USD.
    # Stored as a StorageVec<u16>: length at slot 55, data at keccak256(55) + index.
    # Slot is verified by the `test_reference_currencies_slot_parity` test in
    # `crates/system/oracle/src/tests.rs`; keep this constant in sync with the
    # macro-assigned layout if `OracleContract` field order changes.
    reference_currencies = [840]
    storage.set_slot(55, len(reference_currencies))
    for i, iso_code in enumerate(reference_currencies):
        storage.set_raw_slot(data_slot(55) + i, iso_code)


# --- External contracts ---

def seed_intex_factory(storage: StorageBuilder, config: dict):
    """Write the IntexFactory profile selector (config slot 13) from
    `profile: "prod" | "dev"` (default "prod")."""
    profile = str(config.get("profile", "prod")).lower()
    if profile not in INTEX_PROFILE_SELECTORS:
        raise ValueError(
            f"intex_factory: unknown profile {profile!r}; "
            f"expected one of {sorted(INTEX_PROFILE_SELECTORS)}"
        )
    storage.set_slot(13, INTEX_PROFILE_SELECTORS[profile])  # config_profile


def seed_external_contracts(alloc, contracts_list, contracts_dir):
    """
    Embed externally-fetched contracts (bytecode + storage) into the genesis
    alloc. Each entry has the form:
        {"address": "0x...", "code": "<file>.code.hex",
         "state": "<file>.state.json"?, "nonce": "0x.."?, "balance": "0x.."?}
    Files are read from contracts_dir. Address keys collide-checked against the
    precompile registry to prevent silently overwriting protocol state.
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


# --- Main ---

def main():
    parser = argparse.ArgumentParser(description="Seed genesis.json with precompile storage")
    parser.add_argument("--genesis", required=True, help="Path to genesis.json")
    parser.add_argument("--seed", required=True, help="Path to seed config JSON")
    parser.add_argument("--validators", help="Path to validators.json for genesis validator set")
    parser.add_argument("--output", required=True, help="Output path for patched genesis.json")
    parser.add_argument(
        "--contracts-dir",
        help="Directory containing contract code/state files referenced from "
             "seed['contracts']. Defaults to <seed-file-dir>/contracts.",
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
        if not isinstance(validators, list):
            raise ValueError("validators.json must contain a JSON array")

    alloc = genesis.setdefault("alloc", {})

    # Ensure all precompile addresses have marker bytecode
    for addr in ALL_PRECOMPILE_ADDRESSES:
        entry = alloc.setdefault(addr, {})
        entry["code"] = MARKER_CODE
        entry.setdefault("balance", "0x0")

    # Seed native EVM token balances into alloc.
    if "balance" in seed:
        seed_coen(alloc, seed["balance"])
        print(f"  balance: {len(seed['balance'])} entries")

    # Seed ValidatorSet, Staking, and Rewards from validators.json. This makes
    # genesis.json the canonical protocol state; executor no longer backfills it.
    if validators:
        staking_cfg = seed.get("staking", {})
        min_stake = parse_int(staking_cfg.get("min_stake", MIN_STAKE))
        validator_stake = parse_int(staking_cfg.get("genesis_validator_stake", min_stake))
        if validator_stake < min_stake:
            raise ValueError("genesis_validator_stake must be >= min_stake")
        config = genesis.get("config", {})
        if "epochDuration" in config:
            raise ValueError("genesis config uses deprecated epochDuration; use epochLengthBlocks")
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
        # defaults); it only rejects an obviously malformed non-positive value so
        # a bad genesis fails early. The full ordering invariant
        # (0 < min < leader <= cert) is enforced by validate_timing at startup.
        for _timing_key in ("minBlockTimeMs", "leaderTimeoutMs", "certificationTimeoutMs"):
            if _timing_key in config and parse_int(config[_timing_key]) <= 0:
                raise ValueError(f"genesis config {_timing_key} must be > 0")
        epoch_start_timestamp = parse_genesis_timestamp(genesis)

        validator_storage = StorageBuilder()
        seed_validator_set(
            validator_storage,
            validators,
            seed.get("validator_set", {}),
            epoch_length_blocks=epoch_length_blocks,
            epoch_start_timestamp=epoch_start_timestamp,
            min_stake=min_stake,
            validator_stake=validator_stake,
        )
        alloc[VALIDATOR_SET_ADDRESS].setdefault("storage", {}).update(validator_storage.entries)

        staking_storage = StorageBuilder()
        total_staked = seed_staking(
            staking_storage,
            validators,
            staking_cfg,
            min_stake=min_stake,
            validator_stake=validator_stake,
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

    # V2 Phase 1 accounting progress (slot 0 = 0). Always seeded — independent
    # of validator count — because the executor needs the marker bytecode +
    # an explicit slot 0 = 0 word to record `last_accounted_block_number`
    # under EIP-161-safe storage.
    accounting_storage = StorageBuilder()
    seed_accounting_progress(accounting_storage)
    alloc[ACCOUNTING_PROGRESS_ADDRESS].setdefault("storage", {}).update(
        accounting_storage.entries
    )
    print(
        f"  AccountingProgress: slot 0 = 0, "
        f"{len(accounting_storage.entries)} storage entries"
    )

    # ZeroFee paymaster: slot 0 = schema version (1). Honors the README
    # rule "All precompiles storage versioned (slot 0 = version)" and
    # lets a future migration probe slot 0 to decide whether to apply
    # a layout transformation. The `counter` Map keys are keccak-derived
    # and never write to slot 0 directly, so the version marker has no
    # collision risk.
    zerofee_storage = StorageBuilder()
    seed_zerofee(zerofee_storage)
    alloc[ZEROFEE_ADDRESS].setdefault("storage", {}).update(zerofee_storage.entries)
    print(
        f"  ZeroFee: slot 0 = 1 (schema version), "
        f"{len(zerofee_storage.entries)} storage entries"
    )

    # TEE attestation policy (WS-B): seeds TeeRegistry slot 2 (policy_hash) +
    # config.teePolicy, but only when `tee_policy` is present in the seed config.
    seed_tee_policy(genesis, alloc, seed)

    # Seed Gratis
    if "gratis_balances" in seed:
        gratis_storage = StorageBuilder()
        seed_gratis(gratis_storage, seed["gratis_balances"])
        entry = alloc[GRATIS_ADDRESS]
        entry.setdefault("storage", {}).update(gratis_storage.entries)
        print(f"  Gratis: {len(seed['gratis_balances'])} balances, "
              f"{len(gratis_storage.entries)} storage entries")

    # Seed Promis
    if "promis_balances" in seed:
        promis_storage = StorageBuilder()
        seed_promis(promis_storage, seed["promis_balances"])
        entry = alloc[PROMIS_ADDRESS]
        entry.setdefault("storage", {}).update(promis_storage.entries)
        print(f"  Promis: {len(seed['promis_balances'])} balances, "
              f"{len(promis_storage.entries)} storage entries")

    # Seed Tributes
    if "tributes" in seed:
        tribute_storage = StorageBuilder()
        seed_tributes(tribute_storage, seed["tributes"])
        # Initialize day_totals for OFFERING worldwide days (status 2) so
        # offerTribute is accepted (the directly-seeded OFFERING day never ran
        # the metadosis unseal_day that normally initializes it).
        offering_days = [
            entry_wd["wwd"]
            for entry_wd in seed.get("metadosis", {}).get("worldwide_days", [])
            if entry_wd.get("status", 0) == 2
        ]
        seed_tribute_day_totals(tribute_storage, offering_days)
        entry = alloc[TRIBUTE_ADDRESS]
        entry.setdefault("storage", {}).update(tribute_storage.entries)
        print(f"  Tribute: {len(seed['tributes'])} tributes, "
              f"{len(offering_days)} offering day_totals init, "
              f"{len(tribute_storage.entries)} storage entries")

    # Seed NODs
    if "nods" in seed:
        nod_storage = StorageBuilder()
        seed_nods(nod_storage, seed["nods"])
        entry = alloc[NOD_ADDRESS]
        entry.setdefault("storage", {}).update(nod_storage.entries)
        print(f"  Nod: {len(seed['nods'])} nods, "
              f"{len(nod_storage.entries)} storage entries")

    # Seed Metadosis
    if "metadosis" in seed:
        meta_storage = StorageBuilder()
        seed_metadosis(meta_storage, seed["metadosis"])
        entry = alloc[METADOSIS_ADDRESS]
        entry.setdefault("storage", {}).update(meta_storage.entries)
        wwds = seed["metadosis"].get("worldwide_days", [])
        print(f"  Metadosis: {len(wwds)} worldwide days, "
              f"{len(meta_storage.entries)} storage entries")

    # Seed Oracle
    if "oracle" in seed:
        oracle_storage = StorageBuilder()
        seed_oracle(oracle_storage, seed["oracle"])
        entry = alloc[ORACLE_ADDRESS]
        entry.setdefault("storage", {}).update(oracle_storage.entries)
        pairs = seed["oracle"].get("pairs", [])
        settlements = seed["oracle"].get("settlement_currencies", [])
        print(f"  Oracle: {len(pairs)} pairs, {len(settlements)} settlements, "
              f"{len(oracle_storage.entries)} storage entries")

    # Seed IntexFactory profile selector (account preserved by the runtime
    # marker list, so no extra wiring needed).
    if "intex_factory" in seed:
        intex_factory_storage = StorageBuilder()
        seed_intex_factory(intex_factory_storage, seed["intex_factory"])
        entry = alloc.setdefault(INTEX_FACTORY_ADDRESS, {})
        entry.setdefault("storage", {}).update(intex_factory_storage.entries)
        entry.setdefault("code", MARKER_CODE)
        print(f"  IntexFactory: {len(intex_factory_storage.entries)} storage entries")

    # Seed externally-fetched contracts (e.g. CREATE2 deployer)
    if "contracts" in seed:
        contracts_dir = args.contracts_dir or os.path.join(
            os.path.dirname(os.path.abspath(args.seed)), "contracts"
        )
        seed_external_contracts(alloc, seed["contracts"], contracts_dir)

    with open(args.output, "w") as f:
        json.dump(genesis, f, indent=2)

    total_storage = sum(
        len(v.get("storage", {})) for v in alloc.values()
    )
    print(f"\nGenesis written to {args.output}")
    print(f"Total storage entries: {total_storage}")


if __name__ == "__main__":
    main()
