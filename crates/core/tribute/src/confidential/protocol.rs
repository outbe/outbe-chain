//! Tribute offer results and the tag-0 attestation preimage.

use alloy_primitives::{Address, B256, U256};
pub use outbe_nod::offer::protocol::{
    inputs_canonical_hash, EncryptedTributeOffer, TributePublicInputs, TributeZkContext,
    TributeZkExpectedHashes, WorldwideDay,
};

/// Status of a single offer after processing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TributeOfferStatus {
    /// The offer was accepted and the public result is filled.
    Created,
    /// The offer was rejected. `reason` is the historical display sentence.
    Rejected {
        /// Human-readable rejection sentence.
        reason: String,
    },
}

/// Public result for a single cleartext-priced offer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeOfferResult {
    /// Poseidon digest of owner and worldwide day.
    pub token_id: B256,
    /// L1 `msg.sender` (public, on-chain).
    pub owner: Address,
    /// Issuance amount in minor units.
    pub issuance_amount_minor: U256,
    /// Nominal amount in minor units.
    pub nominal_amount_minor: U256,
    /// `max(reference_wwd_vwap_minor, reference_scurve_minor)`.
    pub effective_reference_price_minor: U256,
    /// SU hashes (hex).
    pub su_hashes: Vec<String>,
    /// WAA wallet addresses.
    pub wallet_addresses: Vec<String>,
    /// SRA addresses.
    pub sra_addresses: Vec<String>,
    /// Expected public hashes recomputed over the decrypted TributeDraft.
    #[serde(default)]
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
    /// Outcome of this offer.
    pub status: TributeOfferStatus,
}

/// Domain-separated preimage signed for a `TributeOfferBatch` (tag 0).
pub fn tribute_offer_attestation_preimage(
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
) -> Result<Vec<u8>, serde_json::Error> {
    let results_json = serde_json::to_vec(results)?;
    let mut buf = Vec::with_capacity(30 + 32 + 4 + results_json.len());
    buf.extend_from_slice(b"outbe/tee/offer-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(results_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&results_json);
    Ok(buf)
}
