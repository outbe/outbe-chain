//! Decrypted offer payload (`TributeInputPayload`) parsing and validation.

use serde::Deserialize;

use super::errors::{OfferResult, PayloadError};

/// Confidential fields of one Tribute offer.
#[derive(Debug, Clone, Deserialize)]
pub struct TributeInputPayload {
    /// L2 creator address, hex.
    pub creator: String,
    /// 32-byte draft id, hex.
    pub tribute_draft_id: String,
    /// Whole units.
    pub amount_base: String,
    /// Fractional remainder in micro-units (0..999999). One whole unit is 10^6.
    pub amount_micro: String,
    /// Claimed SU hashes.
    pub su_hashes: Vec<String>,
    /// WAA wallet addresses.
    #[serde(default)]
    pub wallet_addresses: Vec<String>,
    /// SRA addresses.
    #[serde(default)]
    pub sra_addresses: Vec<String>,
}

/// Parse decrypted JSON and validate the required fields.
pub fn parse_and_validate(plaintext: &[u8]) -> OfferResult<TributeInputPayload> {
    let payload: TributeInputPayload =
        serde_json::from_slice(plaintext).map_err(PayloadError::ParseDecryptedPayload)?;
    validate(&payload)?;
    Ok(payload)
}

pub(crate) fn validate(payload: &TributeInputPayload) -> OfferResult<()> {
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
