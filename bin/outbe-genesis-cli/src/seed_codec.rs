//! Precompile addresses, canonical values and EVM storage encoding.

use std::collections::BTreeMap;

use alloy_primitives::U256;
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, Date, Month, OffsetDateTime};

use crate::error::GenesisError;
use crate::keccak::keccak256;

pub const GRATIS_ADDRESS: &str = "0000000000000000000000000000000000001003";
pub const GRATIS_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000002003";
pub const PROMIS_ADDRESS: &str = "0000000000000000000000000000000000001337";
pub const TRIBUTE_ADDRESS: &str = "0000000000000000000000000000000000001101";
pub const NOD_ADDRESS: &str = "0000000000000000000000000000000000001006";
pub const METADOSIS_ADDRESS: &str = "000000000000000000000000000000000000100e";
pub const TRIBUTE_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000001100";
pub const AGENT_REWARD_ADDRESS: &str = "000000000000000000000000000000000000100b";
pub const FIDELITY_ADDRESS: &str = "000000000000000000000000000000000000100c";
pub const EMISSION_ADDRESS: &str = "000000000000000000000000000000000000100d";
pub const EMISSION_LIMIT_ADDRESS: &str = EMISSION_ADDRESS;
pub const PROMIS_LIMIT_ADDRESS: &str = "000000000000000000000000000000000000100f";
pub const CYCLE_ADDRESS: &str = "0000000000000000000000000000000000001010";
pub const CCA_REGISTRY_ADDRESS: &str = "0000000000000000000000000000000000001011";
pub const CREDIS_ADDRESS: &str = "000000000000000000000000000000000000100a";
pub const CREDIS_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000001009";
pub const INTEX_ADDRESS: &str = "0000000000000000000000000000000000001014";
pub const INTEX_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000001015";
pub const NOD_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000001007";
pub const GEM_FACTORY_ADDRESS: &str = "0000000000000000000000000000000000002013";
pub const VAULT_ROUTER_ADDRESS: &str = "0000000000000000000000000000000000001017";
pub const GEM_ADDRESS: &str = "0000000000000000000000000000000000001013";
pub const GOVERNANCE_ADDRESS: &str = "0000000000000000000000000000000000001018";
pub const VALIDATOR_SET_ADDRESS: &str = "000000000000000000000000000000000000ee00";
pub const SLASH_INDICATOR_ADDRESS: &str = "000000000000000000000000000000000000ee01";
pub const STAKING_ADDRESS: &str = "000000000000000000000000000000000000ee02";
pub const REWARDS_ADDRESS: &str = "000000000000000000000000000000000000ee03";
pub const ACCOUNTING_PROGRESS_ADDRESS: &str = "000000000000000000000000000000000000ee04";
pub const ORACLE_ADDRESS: &str = "000000000000000000000000000000000000ee05";
pub const ZEROFEE_ADDRESS: &str = "000000000000000000000000000000000000ee09";
pub const TEE_REGISTRY_ADDRESS: &str = "000000000000000000000000000000000000ee0a";
pub const UPDATE_ADDRESS: &str = "0x000000000000000000000000000000000000ee0b";
pub const VOTE_ADDRESS: &str = "0x000000000000000000000000000000000000ee0c";
pub const STABLECOIN_FACTORY_ADDRESS: &str = "000000000000000000000000000000000000ee0f";
pub const STABLECOIN_POLICY_REGISTRY_ADDRESS: &str = "000000000000000000000000000000000000ee10";
pub const CODE_REGISTRY_ADDRESS: &str = "000000000000000000000000000000000000ee11";
pub const HYPERLANE_CONTROLLER_ADDRESS: &str = "000000000000000000000000000000000000ee14";
pub const HYPERLANE_CONTROLLER_BOOTSTRAP_ADMIN_SLOT: u64 = 9;
pub const STABLECOIN_ADDRESS_PREFIX: &str = "53c0";
pub const OUTBE_SYSTEM_TX_ADDRESS: &str = "ff00000000000000000000000000000000000001";

pub use outbe_primitives::validators::DEFAULT_MIN_STAKE_UNITS as MIN_STAKE;
pub const SECONDS_PER_DAY: u64 = 86_400;
pub const DEFAULT_UNBONDING_PERIOD: u64 = 21 * SECONDS_PER_DAY;
pub const DEFAULT_REREGISTRATION_COOLDOWN_BLOCKS: u64 = 151_200;
pub const DEFAULT_EPOCH_LENGTH_BLOCKS: u64 = 1_200;

pub const INTEX_PROFILE_SLOT: u64 = 15;
pub const GEM_PROFILE_SLOT: u64 = 32;
pub const NOD_PROFILE_SLOT: u64 = 24;
pub const CREDIS_PROFILE_SLOT: u64 = 38;

pub const MARKER_CODE: &str = "0xef";
pub const P2P_ADDRESS_VERSION_V1: u64 = 1;
pub const MAX_P2P_ADDRESS_ENCODED_LEN: usize = 512;
pub const ISO_MARKER: u32 = 0xCC000;

pub const ALL_PRECOMPILE_ADDRESSES: [&str; 32] = [
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
    EMISSION_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    CYCLE_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    GOVERNANCE_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    CODE_REGISTRY_ADDRESS,
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
    OUTBE_SYSTEM_TX_ADDRESS,
];

pub const VAULT_ROUTER_LIQUIDITY_SOURCES: [(&str, u64); 4] = [
    (INTEX_FACTORY_ADDRESS, 1),
    (CREDIS_FACTORY_ADDRESS, 2),
    (GEM_FACTORY_ADDRESS, 3),
    (NOD_FACTORY_ADDRESS, 4),
];

pub const VAULT_ROUTER_LIQUIDITY_TARGETS: [(&str, u64); 1] = [(CREDIS_FACTORY_ADDRESS, 1)];

/// Check whether an address string matches one of the protected protocol precompile addresses.
pub fn is_protected_protocol_address(addr_norm: &str) -> bool {
    ALL_PRECOMPILE_ADDRESSES
        .iter()
        .any(|&entry| entry.trim_start_matches("0x") == addr_norm)
}

/// Conversion helper into `U256` for unsigned integer primitives and `U256`.
pub trait IntoU256 {
    /// Convert `self` into a `U256`.
    fn into_u256(self) -> U256;
}

impl IntoU256 for U256 {
    fn into_u256(self) -> U256 {
        self
    }
}

impl IntoU256 for u8 {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for u16 {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for u32 {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for u64 {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for u128 {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for usize {
    fn into_u256(self) -> U256 {
        U256::from(self)
    }
}

impl IntoU256 for i32 {
    fn into_u256(self) -> U256 {
        U256::from(self as u64)
    }
}

/// Encode a `U256` integer as 32-byte big-endian.
pub fn to_be32(val: impl IntoU256) -> [u8; 32] {
    val.into_u256().to_be_bytes::<32>()
}

/// Encode a `U256` integer as a `0x`-prefixed 32-byte lowercase hex string.
pub fn hex32(val: impl IntoU256) -> String {
    format!("0x{:064x}", val.into_u256())
}

/// Compute a Solidity-compatible mapping slot: `keccak256(left_pad(key_bytes, 32) ++ to_be(base_slot, 32))`.
pub fn mapping_key(key_bytes: &[u8], base_slot: u64) -> String {
    let mut buf = Vec::with_capacity(key_bytes.len().max(32) + 32);
    if key_bytes.len() < 32 {
        buf.resize(32 - key_bytes.len(), 0);
    }
    buf.extend_from_slice(key_bytes);
    buf.extend_from_slice(&to_be32(U256::from(base_slot)));
    format!("0x{}", hex::encode(keccak256(&buf)))
}

/// Compute the slot of `mapping(K1 => mapping(K2 => V))[outer_key][inner_key]` at `base_slot`.
pub fn nested_mapping_key(outer_key: &[u8], inner_key: &[u8], base_slot: u64) -> U256 {
    let outer = mapping_key(outer_key, base_slot);
    let mut buf = [0u8; 64];
    buf[32 - inner_key.len()..32].copy_from_slice(inner_key);
    buf[32..].copy_from_slice(&hex::decode(&outer[2..]).expect("mapping_key returns hex"));
    U256::from_be_bytes(keccak256(&buf))
}

/// Parse a `0x`-prefixed or bare 40-char hex address into 20 bytes.
pub fn address_bytes(addr_hex: &str) -> Result<[u8; 20], GenesisError> {
    let stripped = addr_hex.trim().to_ascii_lowercase();
    let clean = stripped.strip_prefix("0x").unwrap_or(&stripped);
    if clean.len() != 40 {
        return Err(GenesisError::validation(format!(
            "invalid address length: {addr_hex}"
        )));
    }
    let decoded = hex::decode(clean)
        .map_err(|_| GenesisError::validation(format!("invalid address hex: {addr_hex}")))?;
    let mut out = [0u8; 20];
    out.copy_from_slice(&decoded);
    Ok(out)
}

/// Convert an address hex string to a right-aligned `U256`.
pub fn address_as_u256(addr_hex: &str) -> Result<U256, GenesisError> {
    let bytes = address_bytes(addr_hex)?;
    Ok(U256::from_be_slice(&bytes))
}

/// Encode `u32` as 4-byte big-endian.
pub fn u32_bytes(val: u32) -> [u8; 4] {
    val.to_be_bytes()
}

/// Encode `u64` as 8-byte big-endian.
pub fn u64_bytes(val: u64) -> [u8; 8] {
    val.to_be_bytes()
}

/// Parse a 32-byte `B256` hex string.
pub fn b256_bytes(hex_str: &str) -> Result<[u8; 32], GenesisError> {
    let stripped = hex_str.trim().to_ascii_lowercase();
    let clean = stripped.strip_prefix("0x").unwrap_or(&stripped);
    if clean.len() != 64 {
        return Err(GenesisError::validation(format!(
            "invalid B256 length: {hex_str}"
        )));
    }
    let decoded = hex::decode(clean)
        .map_err(|_| GenesisError::validation(format!("invalid B256 hex: {hex_str}")))?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);
    Ok(out)
}

/// Parse a 48-byte BLS MinPk public key hex string.
pub fn pubkey_bytes(hex_str: &str) -> Result<[u8; 48], GenesisError> {
    let stripped = hex_str.trim().to_ascii_lowercase();
    let clean = stripped.strip_prefix("0x").unwrap_or(&stripped);
    if clean.len() != 96 {
        return Err(GenesisError::validation(format!(
            "invalid BLS public key length: {hex_str}"
        )));
    }
    let decoded = hex::decode(clean)
        .map_err(|_| GenesisError::validation(format!("invalid BLS public key hex: {hex_str}")))?;
    let mut out = [0u8; 48];
    out.copy_from_slice(&decoded);
    Ok(out)
}

/// Parse one founder's required non-zero 32-byte Radicle NodeId.
pub fn radicle_node_id_bytes(
    value: Option<&Value>,
    validator_index: usize,
) -> Result<[u8; 32], GenesisError> {
    let Some(Value::String(raw_str)) = value else {
        return Err(GenesisError::validation(format!(
            "validator {validator_index} radicle_node_id is required"
        )));
    };
    let clean = raw_str
        .strip_prefix("0x")
        .or_else(|| raw_str.strip_prefix("0X"))
        .unwrap_or(raw_str);
    if clean.len() != 64 {
        return Err(GenesisError::validation(format!(
            "validator {validator_index} radicle_node_id must be 64 hex chars"
        )));
    }
    let decoded = hex::decode(clean).map_err(|_| {
        GenesisError::validation(format!(
            "validator {validator_index} radicle_node_id must be hexadecimal"
        ))
    })?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&decoded);
    if out == [0u8; 32] {
        return Err(GenesisError::validation(format!(
            "validator {validator_index} radicle_node_id must not be zero"
        )));
    }
    Ok(out)
}

/// Compute the canonical genesis TEE policy hash (`outbe/tee/policy/v1`).
pub fn compute_tee_policy_hash(
    allowed_mrsigner: &[String],
    allowed_mrenclave: &[String],
    min_isv_svn: u16,
) -> Result<[u8; 32], GenesisError> {
    let mut signers = allowed_mrsigner
        .iter()
        .map(|s| b256_bytes(s))
        .collect::<Result<Vec<_>, _>>()?;
    signers.sort_unstable();
    let mut enclaves = allowed_mrenclave
        .iter()
        .map(|e| b256_bytes(e))
        .collect::<Result<Vec<_>, _>>()?;
    enclaves.sort_unstable();

    let mut buf = Vec::new();
    buf.extend_from_slice(b"outbe/tee/policy/v1");
    buf.extend_from_slice(&(signers.len() as u16).to_be_bytes());
    for s in &signers {
        buf.extend_from_slice(s);
    }
    buf.extend_from_slice(&(enclaves.len() as u16).to_be_bytes());
    for e in &enclaves {
        buf.extend_from_slice(e);
    }
    buf.extend_from_slice(&min_isv_svn.to_be_bytes());
    Ok(keccak256(&buf))
}

/// Parse a JSON number or decimal/hex string into `U256`.
pub fn parse_u256(val: &Value) -> Result<U256, GenesisError> {
    match val {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(U256::from(u))
            } else {
                Err(GenesisError::validation(format!(
                    "cannot parse as unsigned int: {val}"
                )))
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if let Some(hex_part) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                U256::from_str_radix(hex_part, 16)
                    .map_err(|_| GenesisError::validation(format!("cannot parse as int: {val}")))
            } else {
                U256::from_str_radix(trimmed, 10)
                    .map_err(|_| GenesisError::validation(format!("cannot parse as int: {val}")))
            }
        }
        _ => Err(GenesisError::validation(format!(
            "cannot parse as int: {val}"
        ))),
    }
}

/// Parse a JSON number or decimal/hex string into signed `i128`.
pub fn parse_i128(val: &Value) -> Result<i128, GenesisError> {
    match val {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i128::from(i))
            } else if let Some(u) = n.as_u64() {
                Ok(i128::from(u))
            } else {
                Err(GenesisError::validation(format!(
                    "cannot parse as int: {val}"
                )))
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if let Some(hex_part) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                i128::from_str_radix(hex_part, 16)
                    .map_err(|_| GenesisError::validation(format!("cannot parse as int: {val}")))
            } else {
                trimmed
                    .parse::<i128>()
                    .map_err(|_| GenesisError::validation(format!("cannot parse as int: {val}")))
            }
        }
        _ => Err(GenesisError::validation(format!(
            "cannot parse as int: {val}"
        ))),
    }
}

/// Parse a JSON value into `u64`.
pub fn parse_u64(val: &Value) -> Result<u64, GenesisError> {
    let v = parse_i128(val)?;
    u64::try_from(v).map_err(|_| GenesisError::validation(format!("cannot parse as u64: {val}")))
}

/// Parse a JSON value into `u32`.
pub fn parse_u32(val: &Value) -> Result<u32, GenesisError> {
    let v = parse_i128(val)?;
    u32::try_from(v).map_err(|_| GenesisError::validation(format!("cannot parse as u32: {val}")))
}

/// Convert a `YYYYMMDD` worldwide day into its UTC midnight unix timestamp.
pub fn wwd_to_day_timestamp(wwd: u32) -> Result<u64, GenesisError> {
    let year = (wwd / 10_000) as i32;
    let month_num = ((wwd / 100) % 100) as u8;
    let day = (wwd % 100) as u8;
    let month = Month::try_from(month_num)
        .map_err(|_| GenesisError::validation(format!("invalid month in worldwide day: {wwd}")))?;
    let date = Date::from_calendar_date(year, month, day)
        .map_err(|_| GenesisError::validation(format!("invalid calendar date: {wwd}")))?;
    let ts = date.midnight().assume_utc().unix_timestamp();
    u64::try_from(ts)
        .map_err(|_| GenesisError::validation(format!("pre-epoch worldwide day: {wwd}")))
}

/// Parse `config.genesisTime` (ISO 8601 UTC) as a unix timestamp.
pub fn parse_genesis_timestamp(genesis: &Value) -> Result<u64, GenesisError> {
    if let Some(genesis_time_str) = genesis
        .pointer("/config/genesisTime")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let dt = OffsetDateTime::parse(genesis_time_str, &Rfc3339).map_err(|err| {
            GenesisError::validation(format!(
                "invalid config.genesisTime {genesis_time_str}: {err}"
            ))
        })?;
        return u64::try_from(dt.unix_timestamp()).map_err(|_| {
            GenesisError::validation(format!(
                "genesisTime must be non-negative: {genesis_time_str}"
            ))
        });
    }
    Ok(OffsetDateTime::now_utc().unix_timestamp().max(0) as u64)
}

/// Parse the consensus header `timestamp` field as a unix timestamp.
pub fn parse_header_timestamp(genesis: &Value) -> Result<u64, GenesisError> {
    let raw = genesis
        .get("timestamp")
        .ok_or_else(|| GenesisError::validation("genesis header timestamp is required"))?;
    let signed = parse_i128(raw)?;
    if signed < 0 {
        return Err(GenesisError::validation(format!(
            "genesis timestamp must be non-negative: {signed}"
        )));
    }
    u64::try_from(signed)
        .map_err(|_| GenesisError::validation(format!("genesis timestamp overflows u64: {signed}")))
}

/// Convert a unix timestamp to a UTC `YYYYMMDD` date key.
pub fn timestamp_to_utc_date_key(timestamp: u64) -> u32 {
    civil_date_from_days(timestamp / SECONDS_PER_DAY)
}

/// Integer UTC calendar conversion matching `outbe_primitives::time`.
pub fn civil_date_from_days(days_since_epoch: u64) -> u32 {
    let z = days_since_epoch + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y * 10_000 + m * 100 + d) as u32
}

/// Encode an account balance as compact `0x`-prefixed hex.
pub fn alloc_balance_hex(amount: impl IntoU256) -> String {
    format!("0x{:x}", amount.into_u256())
}

/// Solidity dynamic bytes/string data slot: `keccak256(to_be32(base_slot))`.
pub fn data_slot(base_slot: impl IntoU256) -> U256 {
    U256::from_be_bytes(keccak256(&to_be32(base_slot)))
}

/// Accumulates storage entries for a single contract address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageBuilder {
    /// Map from `0x`-prefixed 32-byte slot key to `0x`-prefixed 32-byte word value.
    pub entries: BTreeMap<String, String>,
}

impl StorageBuilder {
    /// Create a new empty storage builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a direct slot value.
    pub fn set_slot(&mut self, slot: impl IntoU256, value: impl IntoU256) {
        self.entries.insert(hex32(slot), hex32(value));
    }

    /// Set a storage slot by `U256` slot to `U256` value.
    pub fn set_raw_slot(&mut self, slot: impl IntoU256, value: impl IntoU256) {
        self.entries.insert(hex32(slot), hex32(value));
    }

    /// Set a storage slot by `0x`-prefixed slot key string to a `U256` value.
    pub fn set_raw_slot_str(&mut self, slot_hex: &str, value: impl IntoU256) {
        self.entries
            .insert(slot_hex.to_ascii_lowercase(), hex32(value));
    }

    /// Set a storage slot to an already encoded `0x`-prefixed 32-byte hex value.
    pub fn set_raw_slot_hex(
        &mut self,
        slot: impl IntoU256,
        value_hex: &str,
    ) -> Result<(), GenesisError> {
        self.set_raw_slot_key_hex(&hex32(slot), value_hex)
    }

    /// Set a storage slot (given as `0x`-prefixed key) to a `0x`-prefixed 32-byte hex value.
    pub fn set_raw_slot_key_hex(
        &mut self,
        slot_key: &str,
        value_hex: &str,
    ) -> Result<(), GenesisError> {
        let value = value_hex.to_ascii_lowercase();
        if !value.starts_with("0x") || value.len() != 66 {
            return Err(GenesisError::validation(format!(
                "invalid storage word: {value_hex}"
            )));
        }
        self.entries.insert(slot_key.to_ascii_lowercase(), value);
        Ok(())
    }

    /// Set a `mapping(K => V)` entry at `base_slot`.
    pub fn set_mapping(&mut self, base_slot: u64, key_bytes: &[u8], value: impl IntoU256) {
        let k = mapping_key(key_bytes, base_slot);
        self.entries.insert(k, hex32(value));
    }

    /// Set a `mapping(K => B256)` entry at `base_slot`.
    pub fn set_mapping_b256(&mut self, base_slot: u64, key_bytes: &[u8], value_bytes: &[u8; 32]) {
        let k = mapping_key(key_bytes, base_slot);
        self.entries
            .insert(k, format!("0x{}", hex::encode(value_bytes)));
    }

    /// Set a two-word `Mapping<K, AddressPair>` entry at `base_slot`.
    pub fn set_mapping_pair(
        &mut self,
        base_slot: u64,
        key_bytes: &[u8],
        base: &str,
        quote: &str,
    ) -> Result<(), GenesisError> {
        let slot_key = mapping_key(key_bytes, base_slot);
        let slot = U256::from_str_radix(slot_key.trim_start_matches("0x"), 16)
            .map_err(|_| GenesisError::validation("invalid mapping key"))?;
        let base_addr = U256::from_be_slice(&asset_address(base)?);
        let quote_addr = U256::from_be_slice(&asset_address(quote)?);
        self.set_raw_slot(slot, base_addr);
        self.set_raw_slot(slot + U256::from(1u64), quote_addr);
        Ok(())
    }
}

/// Write Solidity-compatible dynamic `bytes`/`string` storage at `base_slot`.
pub fn write_storage_bytes(
    storage: &mut StorageBuilder,
    base_slot: U256,
    data: &[u8],
) -> Result<(), GenesisError> {
    let length = data.len();
    if length <= 31 {
        let mut word = [0u8; 32];
        word[..length].copy_from_slice(data);
        word[31] = (length * 2) as u8;
        return storage.set_raw_slot_hex(base_slot, &format!("0x{}", hex::encode(word)));
    }
    storage.set_raw_slot(base_slot, U256::from(length * 2 + 1));
    let start = data_slot(base_slot);
    for (i, chunk) in data.chunks(32).enumerate() {
        let mut word = [0u8; 32];
        word[..chunk.len()].copy_from_slice(chunk);
        storage.set_raw_slot_hex(start + U256::from(i), &format!("0x{}", hex::encode(word)))?;
    }
    Ok(())
}

/// Write `Mapping<K, StorageBytes>::write_string`-compatible metadata.
pub fn write_mapping_string(
    storage: &mut StorageBuilder,
    base_slot: u64,
    key_bytes: &[u8],
    value: &str,
) -> Result<(), GenesisError> {
    write_mapping_bytes(storage, base_slot, key_bytes, value.as_bytes())
}

/// Write `Mapping<K, StorageBytes>::write`-compatible metadata.
pub fn write_mapping_bytes(
    storage: &mut StorageBuilder,
    base_slot: u64,
    key_bytes: &[u8],
    value: &[u8],
) -> Result<(), GenesisError> {
    let slot_hex = mapping_key(key_bytes, base_slot);
    let slot = U256::from_str_radix(slot_hex.trim_start_matches("0x"), 16)
        .map_err(|_| GenesisError::validation("invalid mapping slot"))?;
    write_storage_bytes(storage, slot, value)
}

/// Parse an oracle asset specifier (`"COEN"`/`"native"`, ISO 4217 numeric code, or `0x` address) into 20 bytes.
pub fn asset_address(spec: &str) -> Result<[u8; 20], GenesisError> {
    let text = spec.trim();
    if text.eq_ignore_ascii_case("COEN") || text.eq_ignore_ascii_case("NATIVE") {
        return Ok([0u8; 20]);
    }
    if text.starts_with("0x") || text.starts_with("0X") {
        return address_bytes(text);
    }
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        let code: u32 = text
            .parse()
            .map_err(|_| GenesisError::validation(format!("ISO 4217 code out of range: {text}")))?;
        if !(1..=999).contains(&code) {
            return Err(GenesisError::validation(format!(
                "ISO 4217 code out of range: {text}"
            )));
        }
        let bcd = u32::from_str_radix(&format!("{code:04}"), 16)
            .map_err(|_| GenesisError::validation(format!("invalid ISO BCD: {text}")))?;
        let val = U256::from(ISO_MARKER | bcd);
        let be = val.to_be_bytes::<32>();
        let mut out = [0u8; 20];
        out.copy_from_slice(&be[12..]);
        return Ok(out);
    }
    Err(GenesisError::validation(format!(
        "oracle asset must be COEN, an ISO 4217 numeric code or a 0x address: '{spec}'"
    )))
}

/// Compute the order-independent 40-byte oracle pair storage key.
pub fn address_pair(base: &str, quote: &str) -> Result<[u8; 40], GenesisError> {
    let a = asset_address(base)?;
    let b = asset_address(quote)?;
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let mut out = [0u8; 40];
    out[..20].copy_from_slice(&low);
    out[20..].copy_from_slice(&high);
    Ok(out)
}

/// Check whether a 20-byte asset address represents an `AssetType::IsoCurrency`.
pub fn is_iso_asset_address(address: &[u8; 20]) -> bool {
    if address[..16] != [0u8; 16] {
        return false;
    }
    let raw = u32::from_be_bytes([address[16], address[17], address[18], address[19]]);
    if !(ISO_MARKER..=(ISO_MARKER | 0x999)).contains(&raw) {
        return false;
    }
    let bcd = raw - ISO_MARKER;
    [0, 4, 8, 12]
        .iter()
        .all(|shift| ((bcd >> shift) & 0xF) <= 9)
}

/// Seed a factory profile selector slot (`auto` = 0, `dev` = 1, `prod` = 2).
pub fn seed_profile_selector(
    storage: &mut StorageBuilder,
    config: &Value,
    section: &str,
    slot: u64,
) -> Result<(), GenesisError> {
    let profile = config
        .get("profile")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_ascii_lowercase();
    let selector = match profile.as_str() {
        "auto" => 0u64,
        "dev" => 1u64,
        "prod" => 2u64,
        _ => {
            return Err(GenesisError::validation(format!(
                "{section}: unknown profile '{profile}'; expected one of ['auto', 'dev', 'prod']"
            )));
        }
    };
    if selector != 0 {
        storage.set_slot(slot, selector);
    }
    Ok(())
}
