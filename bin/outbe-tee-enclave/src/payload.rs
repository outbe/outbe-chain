//! Decrypted offer payload (`TributeInputPayload`) parsing + validation.
//!
//! This carries only what must stay confidential: the L2 linkage and the amount.
//! `worldwide_day` and the issuance currency are cleartext ABI arguments on
//! [`EncryptedTributeOffer`](outbe_tee::protocol::EncryptedTributeOffer). The
//! node needs them to price and admit the offer. Issuance writes both to public
//! chain state, so encrypting them protected nothing.
//!
//! Enclave Return Rule: `creator`, `su_hashes`, `wallet_addresses`, and
//! `sra_addresses` are L2-linking material. The enclave reads them to validate
//! and (in a later slice) to emit privacy-preserving used-markers / agent-reward
//! routing, but it MUST NOT return them raw to the host.

use serde::Deserialize;

use crate::errors::{EnclaveResult, PayloadError};

#[derive(Debug, Clone, Deserialize)]
pub struct TributeInputPayload {
    pub creator: String,
    pub tribute_draft_id: String,
    pub amount_base: String,
    /// Fractional remainder in micro-units (0..999999). One whole unit is 10^6.
    pub amount_micro: String,
    pub su_hashes: Vec<String>,
    #[serde(default)]
    pub wallet_addresses: Vec<String>,
    #[serde(default)]
    pub sra_addresses: Vec<String>,
}

/// Parse decrypted JSON and validate the required fields. Returns a
/// human-readable reject reason on failure.
pub fn parse_and_validate(plaintext: &[u8]) -> EnclaveResult<TributeInputPayload> {
    let payload: TributeInputPayload =
        serde_json::from_slice(plaintext).map_err(PayloadError::parse_decrypted_payload)?;

    validate(&payload)?;
    Ok(payload)
}

pub(crate) fn validate(payload: &TributeInputPayload) -> EnclaveResult<()> {
    if payload.tribute_draft_id.is_empty() {
        return Err(PayloadError::DraftIdRequired.into());
    }
    if payload.creator.is_empty() {
        return Err(PayloadError::CreatorRequired.into());
    }
    if payload.su_hashes.is_empty() {
        return Err(PayloadError::SuHashesEmpty.into());
    }
    Ok(())
}
