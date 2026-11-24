//! Offer wire types and the v1 inputs hash.

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
