//! Canonical Stablecoin Factory V1 proposal and token-identity primitives.
//!
//! The proposal codec accepts only the byte-exact JSON form specified by
//! ADR-C-TOK-004. Token identity uses a fixed preimage layout:
//!
//! ```text
//! "OUTBE_STABLECOIN_V1"
//! || chain_id: u64 big-endian (8 bytes)
//! || factory (20 bytes)
//! || issuer (20 bytes)
//! || ticker_length: u8 (1 byte)
//! || ticker bytes
//! ```

use crate::units::MAX_ASSET_DECIMALS;
use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Canonical proposal version encoded in the JSON payload.
pub const STABLECOIN_CREATE_VERSION: u8 = 1;
/// Canonical proposal kind encoded in the JSON payload.
pub const STABLECOIN_CREATE_KIND: &str = "StablecoinCreate";
/// Domain separator at the start of every Stablecoin Factory V1 token-id preimage.
pub const STABLECOIN_TOKEN_ID_DOMAIN: &[u8] = b"OUTBE_STABLECOIN_V1";
/// Minimum canonical ticker length in bytes.
pub const MIN_TICKER_LEN: usize = 2;
/// Maximum canonical ticker length in bytes.
pub const MAX_TICKER_LEN: usize = 12;
/// Maximum immutable token-name length in UTF-8 bytes.
pub const MAX_NAME_LEN: usize = 64;
pub use crate::iso_4217::*;

pub fn validate_currency_code(iso_code: u16) -> Result<(), CurrencyCodeError> {
    if iso_4217_alpha(iso_code).is_none() {
        return Err(CurrencyCodeError::InvalidCurrency { currency: iso_code });
    }
    Ok(())
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CurrencyCodeError {
    #[error("Invalid currency code {currency}")]
    InvalidCurrency { currency: u16 },
}

/// Validated fields carried by a canonical stablecoin-creation proposal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StablecoinCreatePayload {
    pub issuer: Address,
    pub name: String,
    pub ticker: String,
    pub iso4217: u16,
    pub decimals: u8,
    pub supply_cap: U256,
    pub policy_id: U256,
}

/// Stable error classes for canonical proposal and identity validation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StablecoinCodecError {
    #[error("stablecoin proposal is not valid UTF-8 JSON")]
    InvalidUtf8,
    #[error("stablecoin proposal is not valid typed JSON")]
    InvalidJson,
    #[error("stablecoin proposal bytes are not canonical")]
    NonCanonicalEncoding,
    #[error("unsupported stablecoin proposal version: {0}")]
    UnsupportedVersion(u8),
    #[error("invalid stablecoin proposal kind")]
    InvalidKind,
    #[error("invalid stablecoin Factory address")]
    InvalidFactory,
    #[error("invalid stablecoin issuer address")]
    InvalidIssuer,
    #[error("invalid stablecoin name")]
    InvalidName,
    #[error("invalid stablecoin ticker")]
    InvalidTicker,
    #[error("invalid ISO-4217 numeric code: {0}")]
    InvalidIso4217(u16),
    #[error("invalid stablecoin decimals: {0}")]
    InvalidDecimals(u8),
    #[error("invalid stablecoin supply cap")]
    InvalidSupplyCap,
    #[error("invalid stablecoin policy id")]
    InvalidPolicyId,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WirePayload {
    version: u8,
    kind: String,
    issuer: String,
    name: String,
    ticker: String,
    iso4217: u16,
    decimals: u8,
    #[serde(rename = "supplyCap")]
    supply_cap: String,
    #[serde(rename = "policyId")]
    policy_id: String,
}

/// Decodes a byte-exact canonical Stablecoin Factory V1 proposal.
///
/// The decoder does these steps:
/// 1. Parse the bytes into a fixed typed shape.
/// 2. Validate every field.
/// 3. Re-encode the payload.
/// 4. Require byte equality with the input.
///
/// This rejects whitespace, alternate key order, duplicate or unknown keys, non-shortest
/// JSON escaping and alternate numeric spellings.
pub fn decode_canonical_stablecoin_create(
    bytes: &[u8],
) -> Result<StablecoinCreatePayload, StablecoinCodecError> {
    core::str::from_utf8(bytes).map_err(|_| StablecoinCodecError::InvalidUtf8)?;
    let wire: WirePayload =
        serde_json::from_slice(bytes).map_err(|_| StablecoinCodecError::InvalidJson)?;
    let payload = payload_from_wire(&wire)?;
    let canonical = encode_canonical_stablecoin_create(&payload)?;
    if canonical != bytes {
        return Err(StablecoinCodecError::NonCanonicalEncoding);
    }
    Ok(payload)
}

/// Encodes validated fields into the one canonical Stablecoin Factory V1 JSON form.
pub fn encode_canonical_stablecoin_create(
    payload: &StablecoinCreatePayload,
) -> Result<Vec<u8>, StablecoinCodecError> {
    validate_payload(payload)?;
    let wire = WirePayload {
        version: STABLECOIN_CREATE_VERSION,
        kind: STABLECOIN_CREATE_KIND.to_owned(),
        issuer: format!("{:#x}", payload.issuer),
        name: payload.name.clone(),
        ticker: payload.ticker.clone(),
        iso4217: payload.iso4217,
        decimals: payload.decimals,
        supply_cap: payload.supply_cap.to_string(),
        policy_id: payload.policy_id.to_string(),
    };
    serde_json::to_vec(&wire).map_err(|_| StablecoinCodecError::InvalidJson)
}

/// Returns the exact bytes hashed to derive a Stablecoin Factory V1 token id.
pub fn stablecoin_token_id_preimage(
    chain_id: u64,
    factory: Address,
    issuer: Address,
    ticker: &str,
) -> Result<Vec<u8>, StablecoinCodecError> {
    if factory.is_zero() {
        return Err(StablecoinCodecError::InvalidFactory);
    }
    if issuer.is_zero() {
        return Err(StablecoinCodecError::InvalidIssuer);
    }
    validate_ticker(ticker)?;

    let ticker_len = u8::try_from(ticker.len()).map_err(|_| StablecoinCodecError::InvalidTicker)?;
    let mut preimage = Vec::with_capacity(
        STABLECOIN_TOKEN_ID_DOMAIN.len() + 8 + Address::len_bytes() * 2 + 1 + ticker.len(),
    );
    preimage.extend_from_slice(STABLECOIN_TOKEN_ID_DOMAIN);
    preimage.extend_from_slice(&chain_id.to_be_bytes());
    preimage.extend_from_slice(factory.as_slice());
    preimage.extend_from_slice(issuer.as_slice());
    preimage.push(ticker_len);
    preimage.extend_from_slice(ticker.as_bytes());
    Ok(preimage)
}

/// Derives the full 256-bit stablecoin token id.
pub fn stablecoin_token_id(
    chain_id: u64,
    factory: Address,
    issuer: Address,
    ticker: &str,
) -> Result<B256, StablecoinCodecError> {
    Ok(keccak256(stablecoin_token_id_preimage(
        chain_id, factory, issuer, ticker,
    )?))
}

/// Maps a full token id into the reserved two-byte class and rightmost 144 hash bits.
pub fn stablecoin_address(token_id: B256, prefix: [u8; 2]) -> Address {
    let mut bytes = [0u8; Address::len_bytes()];
    bytes[..2].copy_from_slice(&prefix);
    bytes[2..].copy_from_slice(&token_id.as_slice()[14..]);
    Address::from(bytes)
}

/// Derives both the full token id and its reserved-class EVM address.
pub fn predict_stablecoin(
    chain_id: u64,
    factory: Address,
    issuer: Address,
    ticker: &str,
    prefix: [u8; 2],
) -> Result<(B256, Address), StablecoinCodecError> {
    let token_id = stablecoin_token_id(chain_id, factory, issuer, ticker)?;
    Ok((token_id, stablecoin_address(token_id, prefix)))
}

fn payload_from_wire(wire: &WirePayload) -> Result<StablecoinCreatePayload, StablecoinCodecError> {
    if wire.version != STABLECOIN_CREATE_VERSION {
        return Err(StablecoinCodecError::UnsupportedVersion(wire.version));
    }
    if wire.kind != STABLECOIN_CREATE_KIND {
        return Err(StablecoinCodecError::InvalidKind);
    }

    let payload = StablecoinCreatePayload {
        issuer: parse_canonical_address(&wire.issuer)?,
        name: wire.name.clone(),
        ticker: wire.ticker.clone(),
        iso4217: wire.iso4217,
        decimals: wire.decimals,
        supply_cap: parse_canonical_decimal(&wire.supply_cap, true)
            .map_err(|_| StablecoinCodecError::InvalidSupplyCap)?,
        policy_id: parse_canonical_decimal(&wire.policy_id, false)
            .map_err(|_| StablecoinCodecError::InvalidPolicyId)?,
    };
    validate_payload(&payload)?;
    Ok(payload)
}

fn validate_payload(payload: &StablecoinCreatePayload) -> Result<(), StablecoinCodecError> {
    if payload.issuer.is_zero() {
        return Err(StablecoinCodecError::InvalidIssuer);
    }
    let name_len = payload.name.len();
    if name_len == 0 || name_len > MAX_NAME_LEN || payload.name.chars().any(char::is_control) {
        return Err(StablecoinCodecError::InvalidName);
    }
    validate_ticker(&payload.ticker)?;
    if !is_supported_iso4217(payload.iso4217) {
        return Err(StablecoinCodecError::InvalidIso4217(payload.iso4217));
    }
    if payload.decimals > MAX_ASSET_DECIMALS {
        return Err(StablecoinCodecError::InvalidDecimals(payload.decimals));
    }
    if payload.supply_cap.is_zero() {
        return Err(StablecoinCodecError::InvalidSupplyCap);
    }
    Ok(())
}

/// Returns whether a numeric code is present in the protocol-pinned ISO 4217 snapshot.
pub fn is_supported_iso4217(code: u16) -> bool {
    ISO_4217_NUMERIC_CODES.binary_search(&code).is_ok()
}

/// Validates the canonical V1 ticker grammar without deriving an identity.
pub fn validate_ticker(ticker: &str) -> Result<(), StablecoinCodecError> {
    let bytes = ticker.as_bytes();
    if !(MIN_TICKER_LEN..=MAX_TICKER_LEN).contains(&bytes.len())
        || !bytes.first().is_some_and(u8::is_ascii_uppercase)
        || !bytes
            .iter()
            .skip(1)
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(StablecoinCodecError::InvalidTicker);
    }
    Ok(())
}

fn parse_canonical_address(value: &str) -> Result<Address, StablecoinCodecError> {
    let bytes = value.as_bytes();
    if bytes.len() != 42
        || !value.starts_with("0x")
        || !bytes[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(StablecoinCodecError::InvalidIssuer);
    }
    let mut decoded = [0u8; Address::len_bytes()];
    hex::decode_to_slice(&value[2..], &mut decoded)
        .map_err(|_| StablecoinCodecError::InvalidIssuer)?;
    let address = Address::from(decoded);
    if address.is_zero() {
        return Err(StablecoinCodecError::InvalidIssuer);
    }
    Ok(address)
}

fn parse_canonical_decimal(value: &str, reject_zero: bool) -> Result<U256, StablecoinCodecError> {
    let bytes = value.as_bytes();
    let canonical = value == "0"
        || (!bytes.is_empty() && bytes[0] != b'0' && bytes.iter().all(u8::is_ascii_digit));
    if !canonical {
        return Err(StablecoinCodecError::InvalidJson);
    }
    let parsed = value
        .parse::<U256>()
        .map_err(|_| StablecoinCodecError::InvalidJson)?;
    if reject_zero && parsed.is_zero() {
        return Err(StablecoinCodecError::InvalidJson);
    }
    Ok(parsed)
}
