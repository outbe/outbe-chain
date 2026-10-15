"""Precompile addresses, canonical values and EVM storage encoding."""

from __future__ import annotations
from genesis_keccak import keccak256


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


CCA_REGISTRY_ADDRESS = "0000000000000000000000000000000000001011"


CREDIS_ADDRESS = "000000000000000000000000000000000000100a"


CREDIS_FACTORY_ADDRESS = "0000000000000000000000000000000000001009"


INTEX_ADDRESS = "0000000000000000000000000000000000001014"


INTEX_FACTORY_ADDRESS = "0000000000000000000000000000000000001015"


NOD_FACTORY_ADDRESS = "0000000000000000000000000000000000001007"


GEM_FACTORY_ADDRESS = "0000000000000000000000000000000000002013"


VAULT_ROUTER_ADDRESS = "0000000000000000000000000000000000001017"


GEM_ADDRESS = "0000000000000000000000000000000000001013"


GOVERNANCE_ADDRESS = "0000000000000000000000000000000000001018"


VALIDATOR_SET_ADDRESS = "000000000000000000000000000000000000ee00"


SLASH_INDICATOR_ADDRESS = "000000000000000000000000000000000000ee01"


STAKING_ADDRESS = "000000000000000000000000000000000000ee02"


REWARDS_ADDRESS = "000000000000000000000000000000000000ee03"


ACCOUNTING_PROGRESS_ADDRESS = "000000000000000000000000000000000000ee04"


ORACLE_ADDRESS = "000000000000000000000000000000000000ee05"


ZEROFEE_ADDRESS = "000000000000000000000000000000000000ee09"


COMPRESSED_ENTITIES_ADDRESS = "000000000000000000000000000000000000ee0d"


COMPRESSED_ENTITIES_SCHEMA_VERSION = 4


COMPRESSED_ENTITIES_EMPTY_SEALED_ROOT = int(
    "247048d4786c64554511d6de5ef953f9b3754d3e7a21234fe94d4dc9981cd1ed", 16
)


TEE_REGISTRY_ADDRESS = "000000000000000000000000000000000000ee0a"


UPDATE_ADDRESS = "0x000000000000000000000000000000000000ee0b"


VOTE_ADDRESS = "0x000000000000000000000000000000000000ee0c"


STABLECOIN_FACTORY_ADDRESS = "000000000000000000000000000000000000ee0f"


STABLECOIN_POLICY_REGISTRY_ADDRESS = "000000000000000000000000000000000000ee10"


RADICLE_REGISTRY_ADDRESS = "000000000000000000000000000000000000ee11"


HYPERLANE_CONTROLLER_ADDRESS = "000000000000000000000000000000000000ee14"


HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT = 9


STABLECOIN_ADDRESS_PREFIX = "53c0"


OUTBE_SYSTEM_TX_ADDRESS = "ff00000000000000000000000000000000000001"


MIN_STAKE = 100_000 * 10**18


SECONDS_PER_DAY = 86_400


DEFAULT_UNBONDING_PERIOD = 21 * SECONDS_PER_DAY


DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS = 151_200


DEFAULT_EPOCH_LENGTH_BLOCKS = 1_200



PROFILE_SELECTORS = {"auto": 0, "dev": 1, "prod": 2}


INTEX_PROFILE_SLOT = 35


GEM_PROFILE_SLOT = 34


NOD_PROFILE_SLOT = 44


CREDIS_PROFILE_SLOT = 50


ALL_PRECOMPILE_ADDRESSES = [
    GRATIS_ADDRESS,
    GRATIS_FACTORY_ADDRESS,
    PROMIS_ADDRESS,
    TRIBUTE_ADDRESS,
    NOD_ADDRESS,
    GEM_ADDRESS,
    METADOSIS_ADDRESS,
    TRIBUTE_FACTORY_ADDRESS,
    AGENT_REWARD_ADDRESS,
    FIDELITY_ADDRESS,
    EMISSION_LIMIT_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    CYCLE_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    GOVERNANCE_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    RADICLE_REGISTRY_ADDRESS,
    HYPERLANE_CONTROLLER_ADDRESS,
    UPDATE_ADDRESS,
    VOTE_ADDRESS,
    VALIDATOR_SET_ADDRESS,
    SLASH_INDICATOR_ADDRESS,
    STAKING_ADDRESS,
    REWARDS_ADDRESS,
    ACCOUNTING_PROGRESS_ADDRESS,
    ORACLE_ADDRESS,
    ZEROFEE_ADDRESS,
    COMPRESSED_ENTITIES_ADDRESS,
    OUTBE_SYSTEM_TX_ADDRESS,
]


PROTOCOL_ACCUMULATOR_ADDRESSES = []


PROTECTED_PROTOCOL_ADDRESSES = set(
    ALL_PRECOMPILE_ADDRESSES + PROTOCOL_ACCUMULATOR_ADDRESSES
)


MARKER_CODE = "0xef"


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
    This function sorts lists ascending by their 32 raw bytes, matching Rust's
    `B256` `sort_unstable`. The hash is independent of allowlist ordering.
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


def radicle_node_id_bytes(value: object, *, validator_index: int) -> bytes:
    """Parse one founder's required non-zero 32-byte Radicle NodeId."""
    if not isinstance(value, str):
        raise ValueError(f"validator {validator_index} radicle_node_id is required")
    raw = value.removeprefix("0x").removeprefix("0X")
    if len(raw) != 64:
        raise ValueError(
            f"validator {validator_index} radicle_node_id must be 64 hex chars"
        )
    try:
        node_id = bytes.fromhex(raw)
    except ValueError as error:
        raise ValueError(
            f"validator {validator_index} radicle_node_id must be hexadecimal"
        ) from error
    if node_id == bytes(32):
        raise ValueError(
            f"validator {validator_index} radicle_node_id must not be zero"
        )
    return node_id


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


def parse_header_timestamp(genesis: dict) -> int:
    """Parse the consensus header ``timestamp`` as a unix timestamp."""
    if "timestamp" not in genesis:
        raise ValueError("genesis header timestamp is required")
    return parse_int(genesis["timestamp"])


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
        assert value.startswith("0x") and len(value) == 66, (
            f"invalid storage word: {value_hex}"
        )
        self.entries[key] = value

    def set_mapping(self, base_slot: int, key_bytes: bytes, value: int):
        """Set a mapping entry."""
        k = mapping_key(key_bytes, base_slot)
        self.entries[k] = hex32(value)

    def set_mapping_b256(self, base_slot: int, key_bytes: bytes, value_bytes: bytes):
        """Set a mapping entry with a B256 value."""
        k = mapping_key(key_bytes, base_slot)
        self.entries[k] = "0x" + value_bytes.hex()

    def set_mapping_pair(self, base_slot: int, key_bytes: bytes, base: str, quote: str):
        """
        Set a two-word `Mapping<K, AddressPair>` entry.

        An oracle pair is 40 bytes. A storage word is 32 bytes.
        The value spans the key's mapping slot and the next slot: base, then quote.
        Solidity uses the same layout for `mapping(K => struct { address; address; })`.
        Callers pass the pair in its registered orientation. Nothing here sorts it.
        """
        slot = int(mapping_key(key_bytes, base_slot), 16)
        self.set_raw_slot(slot, int.from_bytes(asset_address(base), "big"))
        self.set_raw_slot(slot + 1, int.from_bytes(asset_address(quote), "big"))


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
        chunk = data[i * 32 : (i + 1) * 32]
        word = bytearray(32)
        word[: len(chunk)] = chunk
        storage.set_raw_slot_hex(start + i, "0x" + word.hex())


def write_mapping_string(
    storage: StorageBuilder, base_slot: int, key_bytes: bytes, value: str
):
    """Write Mapping<K, StorageBytes>::write_string-compatible metadata."""
    write_storage_bytes(storage, mapping_key(key_bytes, base_slot), value.encode())


def write_mapping_bytes(
    storage: StorageBuilder, base_slot: int, key_bytes: bytes, value: bytes
):
    """Write Mapping<K, StorageBytes>::write-compatible metadata."""
    write_storage_bytes(storage, mapping_key(key_bytes, base_slot), value)


P2P_ADDRESS_VERSION_V1 = 1


MAX_P2P_ADDRESS_ENCODED_LEN = 512


ISO_MARKER = 0xCC000


VAULT_ROUTER_LIQUIDITY_SOURCES = [
    (INTEX_FACTORY_ADDRESS, 1),  # IntexCostAmount
    (CREDIS_FACTORY_ADDRESS, 2),  # CredisCostAmount
    (GEM_FACTORY_ADDRESS, 3),  # GemCostAmount
    (NOD_FACTORY_ADDRESS, 4),  # NodCostAmount
]


VAULT_ROUTER_LIQUIDITY_TARGETS = [
    (CREDIS_FACTORY_ADDRESS, 1),  # Credis
]


def asset_address(spec: str) -> bytes:
    """
    An oracle asset as its 20-byte address.

    Accepts "COEN"/"native" for the native asset (the zero address), a decimal
    ISO 4217 numeric code, or an explicit 0x-prefixed ERC20 address. Mirrors
    `AssetType` in crates/blockchain/primitives/src/asset_type.rs.
    """
    text = str(spec).strip()
    if text.upper() in ("COEN", "NATIVE"):
        return bytes(20)
    if text.lower().startswith("0x"):
        return address_bytes(text)
    if text.isdigit():
        code = int(text)
        if not 1 <= code <= 999:
            raise ValueError(f"ISO 4217 code out of range: {text}")
        # One decimal digit per nibble, behind the marker: 840 -> 0x0cc840.
        return (ISO_MARKER | int(f"{code:04d}", 16)).to_bytes(20, "big")
    raise ValueError(
        f"oracle asset must be COEN, an ISO 4217 numeric code or a 0x address: {spec!r}"
    )


def address_pair(base: str, quote: str) -> bytes:
    """
    Oracle pair storage key: both asset addresses concatenated ascending.

    The order-independent key is the same for `COEN/840` and `840/COEN`.
    `mapping_key` needs no special case. It pads on the left with `rjust(32)`,
    which never truncates. A 40-byte key needs no padding and yields the same
    slot as the Rust `StorageKey for AddressPair` override.
    """
    low, high = sorted((asset_address(base), asset_address(quote)))
    return low + high


def is_iso_asset_address(address: bytes) -> bool:
    """Mirror `AssetType::IsoCurrency` for an already encoded asset address."""
    raw = int.from_bytes(address, "big")
    if not ISO_MARKER <= raw <= ISO_MARKER | 0x999:
        return False
    bcd = raw - ISO_MARKER
    return all(((bcd >> shift) & 0xF) <= 9 for shift in (0, 4, 8, 12))


def seed_profile_selector(
    storage: StorageBuilder, config: dict, section: str, slot: int
):
    """Write the profile selector from `profile: "auto"|"dev"|"prod"`. The auto
    profile is the default, seeds nothing, and lets the chain id decide."""
    profile = str(config.get("profile", "auto")).lower()
    if profile not in PROFILE_SELECTORS:
        raise ValueError(
            f"{section}: unknown profile {profile!r}; "
            f"expected one of {sorted(PROFILE_SELECTORS)}"
        )
    selector = PROFILE_SELECTORS[profile]
    if selector == 0:
        return
    storage.set_slot(slot, selector)
