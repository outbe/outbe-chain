//! Frozen Tribute offer wire types and the v1 inputs hash.

use alloy_primitives::{Address, B256, U256};
pub use outbe_system_common::time::WorldwideDay;

/// A single offer handed to the Tribute engine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EncryptedTributeOffer {
    /// L1 `msg.sender` that owns the resulting Tribute (public, on-chain).
    pub owner: Address,
    /// ABI `cipherText`: AEAD ciphertext of the offer payload.
    pub cipher_text: Vec<u8>,
    /// ABI `nonce`: 12-byte ChaCha20Poly1305 nonce.
    pub nonce: Vec<u8>,
    /// ABI `ephemeralPubkey` (uint256): client ephemeral X25519 public key for ECDHE, big-endian.
    pub ephemeral_pubkey: U256,
    /// ABI `worldwideDay`: UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// ABI `tributeCurrency`: ISO 4217 code the tribute amount is denominated in.
    pub tribute_currency: u16,
    /// ABI `referenceCurrency`: ISO 4217 reference currency code.
    pub reference_currency: u16,
    /// Exact WorldwideDay VWAP for `COEN/<tribute_currency>`, scale 1e6.
    pub issuance_wwd_vwap_minor: U256,
    /// Exact WorldwideDay VWAP for `COEN/<reference_currency>`, scale 1e6.
    pub reference_wwd_vwap_minor: U256,
    /// Continuous reference-currency S-curve value, scale 1e6.
    pub reference_scurve_minor: U256,
    /// Public ZK claim context supplied for every admitted offer.
    #[serde(default)]
    pub zk_context: Option<TributeZkContext>,
}

/// Public ZK claim context bound to one offer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkContext {
    /// Public input zero of the submitted Tribute proof.
    pub derived_owner: B256,
    /// Host chain id bound by the proof.
    pub chain_id: u64,
    /// L2 chain id bound by the proof.
    pub l2_chain_id: u64,
}

/// Shared four-word public claim for Demo Tribute and Niflheim Tribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TributePublicInputs {
    /// Public input zero.
    pub derived_owner: B256,
    /// Poseidon entity hash of the draft.
    pub nft_hash: B256,
    /// Submission binding hash.
    pub binding_hash: B256,
    /// Merkle root supplied with the proof.
    pub merkle_root: B256,
}

impl TributePublicInputs {
    /// Map ABI-ordered words after the caller validates canonical field encoding.
    #[must_use]
    pub fn from_raw_parts(
        [derived_owner, nft_hash, binding_hash, merkle_root]: [[u8; 32]; 4],
    ) -> Self {
        Self {
            derived_owner: B256::from(derived_owner),
            nft_hash: B256::from(nft_hash),
            binding_hash: B256::from(binding_hash),
            merkle_root: B256::from(merkle_root),
        }
    }
}

/// Recomputed public hashes of a decrypted TributeDraft.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkExpectedHashes {
    /// Poseidon entity hash.
    pub nft_hash: B256,
    /// Submission binding hash.
    pub binding_hash: B256,
}

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

/// Deterministic hash over the canonical batch inputs: every field of every offer.
#[must_use]
pub fn inputs_canonical_hash(offers: &[EncryptedTributeOffer]) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&(offers.len() as u32).to_be_bytes());
    for offer in offers {
        buf.extend_from_slice(offer.owner.as_slice());
        buf.extend_from_slice(&(offer.cipher_text.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.cipher_text);
        buf.extend_from_slice(&(offer.nonce.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.nonce);
        buf.extend_from_slice(&offer.ephemeral_pubkey.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.worldwide_day.value().to_be_bytes());
        buf.extend_from_slice(&offer.tribute_currency.to_be_bytes());
        buf.extend_from_slice(&offer.reference_currency.to_be_bytes());
        buf.extend_from_slice(&offer.issuance_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_scurve_minor.to_be_bytes::<32>());
        match &offer.zk_context {
            Some(context) => {
                buf.push(1);
                buf.extend_from_slice(context.derived_owner.as_slice());
                buf.extend_from_slice(&context.chain_id.to_be_bytes());
                buf.extend_from_slice(&context.l2_chain_id.to_be_bytes());
            }
            None => buf.push(0),
        }
    }
    alloy_primitives::keccak256(buf)
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
