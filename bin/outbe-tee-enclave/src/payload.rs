//! Decrypted offer payload (`TributeInputPayload`) parsing + validation.
//!
//! `worldwide_day` and `currency` are part of the ENCRYPTED payload — the
//! client's authenticated commitment to the offer. The enclave uses these
//! decrypted values for the result and `token_id`; the cleartext ABI copies are
//! only the node's pricing key, which the enclave verifies against these.
//!
//! NOTE (Enclave Return Rule): `creator`, `su_hashes`, `wallet_addresses`, and
//! `sra_addresses` are L2-linking material. The enclave reads them to validate
//! and (in a later slice) to emit privacy-preserving used-markers / agent-reward
//! routing, but it MUST NOT return them raw to the host.

use serde::Deserialize;

use crate::compute::worldwide_day_is_valid;

#[derive(Debug, Clone, Deserialize)]
pub struct TributeInputPayload {
    pub creator: String,
    pub tribute_draft_id: String,
    pub worldwide_day: u32,
    pub currency: u16,
    pub amount_base: String,
    pub amount_atto: String,
    pub su_hashes: Vec<String>,
    #[serde(default)]
    pub wallet_addresses: Vec<String>,
    #[serde(default)]
    pub sra_addresses: Vec<String>,
}

/// Parse decrypted JSON and validate the required fields. Returns a
/// human-readable reject reason on failure.
pub fn parse_and_validate(plaintext: &[u8]) -> Result<TributeInputPayload, String> {
    let payload: TributeInputPayload = serde_json::from_slice(plaintext)
        .map_err(|e| format!("failed to parse decrypted payload: {e}"))?;

    if payload.tribute_draft_id.is_empty() {
        return Err("tribute_draft_id is required".to_string());
    }
    if payload.creator.is_empty() {
        return Err("creator is required".to_string());
    }
    if !worldwide_day_is_valid(payload.worldwide_day) {
        return Err("worldwide_day is invalid".to_string());
    }
    if payload.su_hashes.is_empty() {
        return Err("su_hashes cannot be empty".to_string());
    }
    Ok(payload)
}
