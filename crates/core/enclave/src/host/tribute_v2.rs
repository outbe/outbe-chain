//! Encrypted Tribute offer results and their canonical request binding.

use alloy_primitives::{keccak256, B256};
use outbe_core_common::tribute_encryption::{EncryptedTributeV2, TributeAmountsV2};
use outbe_tee::{tee_call::verify_enclave_attestation, TransportError};
use serde::{Deserialize, Serialize};

use super::{
    errors::TRIBUTE_OFFER_ATTESTATION,
    protocol::{
        inputs_canonical_hash, EncryptedTributeOffer, TributeOfferStatus, TributeZkExpectedHashes,
    },
};

/// Public offer result. Amounts and the creator public key remain encrypted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedTributeOfferResultV2 {
    /// Deterministic Tribute entity token identifier.
    pub token_id: B256,
    /// Canonical encrypted Tribute record when `status` is `Created`.
    pub tribute: Option<EncryptedTributeV2>,
    /// Claimed SU hashes (hex).
    pub su_hashes: Vec<String>,
    /// Claimed WAA wallet addresses.
    pub wallet_addresses: Vec<String>,
    /// Claimed SRA addresses.
    pub sra_addresses: Vec<String>,
    /// Recomputed public ZK claim hashes when `zk_context` was supplied.
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
    /// Outcome status of the offer.
    pub status: TributeOfferStatus,
}

impl EncryptedTributeOfferResultV2 {
    /// Construct a deterministically rejected offer result.
    #[must_use]
    pub fn rejected(reason: String) -> Self {
        Self {
            token_id: B256::ZERO,
            tribute: None,
            su_hashes: Vec::new(),
            wallet_addresses: Vec::new(),
            sra_addresses: Vec::new(),
            zk_expected_hashes: None,
            status: TributeOfferStatus::Rejected { reason },
        }
    }
}

/// V2 reuses the frozen offer wire layout: `offer.owner` is the transaction
/// caller for proof binding. The Tribute owner comes from the encrypted creator.
#[must_use]
pub fn encrypted_offer_inputs_hash(chain_id: u64, offers: &[EncryptedTributeOffer]) -> B256 {
    let mut bytes = b"outbe/tribute/offer-inputs/v2".to_vec();
    bytes.extend_from_slice(&chain_id.to_be_bytes());
    bytes.extend_from_slice(inputs_canonical_hash(offers).as_slice());
    keccak256(bytes)
}

/// Domain-separated preimage signed by the enclave for `EncryptedTributeOfferBatchV2`.
pub fn encrypted_offer_attestation_preimage(
    inputs_hash: B256,
    results: &[EncryptedTributeOfferResultV2],
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/offer-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(results)?);
    Ok(bytes)
}

/// Canonical commitment over a `ReadTributeAmountsV2` batch input.
pub fn tribute_read_inputs_hash(
    tributes: &[EncryptedTributeV2],
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/read-inputs/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(tributes)?);
    Ok(keccak256(bytes))
}

/// Domain-separated preimage signed by the enclave for `TributeAmountsReadV2`.
pub fn tribute_read_attestation_preimage(
    inputs_hash: B256,
    amounts: &[TributeAmountsV2],
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/read-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(amounts)?);
    Ok(bytes)
}

/// Verify a private session response before using any decrypted amount.
pub fn verify_attestation(
    attestation_pub: &[u8; 32],
    preimage: &[u8],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_enclave_attestation(attestation_pub, preimage, tag)
        .map_err(|cause| TransportError::app_attestation(TRIBUTE_OFFER_ATTESTATION, cause))
}
