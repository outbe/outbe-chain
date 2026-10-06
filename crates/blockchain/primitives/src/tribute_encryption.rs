//! Neutral encrypted Tribute records shared by storage, clients and the enclave.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

use crate::{time::WorldwideDay, wwd_entity_id::WwdEntityId};

pub const TRIBUTE_PUBLIC_KEY_BLOB_LEN: usize = 8 + 32 + 16;
pub const TRIBUTE_AMOUNTS_BLOB_LEN: usize = 8 + 64 + 16;
pub const TRIBUTE_AMOUNT_KEY_INFO: &[u8] = b"outbe/tribute/amount-key/v2";
pub const TRIBUTE_AMOUNT_NONCE_INFO: &[u8] = b"outbe/tribute/amount-nonce/v2";

/// Immutable public metadata authenticated by the encrypted amount record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeContextV2 {
    pub chain_id: u64,
    pub tribute_id: WwdEntityId,
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub tribute_price_minor: U256,
    pub exclude_from_intex_issuance: bool,
    pub offer_input_hash: B256,
}

impl TributeContextV2 {
    /// Fixed-width encoding: neither JSON formatting nor field order affects it.
    pub fn digest(&self) -> B256 {
        let mut bytes = b"outbe/tribute/context/v2".to_vec();
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(self.tribute_id.as_slice());
        bytes.extend_from_slice(self.owner.as_slice());
        bytes.extend_from_slice(&self.worldwide_day.value().to_be_bytes());
        bytes.extend_from_slice(&self.issuance_currency.to_be_bytes());
        bytes.extend_from_slice(&self.reference_currency.to_be_bytes());
        bytes.extend_from_slice(&self.tribute_price_minor.to_be_bytes::<32>());
        bytes.push(u8::from(self.exclude_from_intex_issuance));
        bytes.extend_from_slice(self.offer_input_hash.as_slice());
        keccak256(bytes)
    }

    /// Crypto slot for the existing confidential blob helper; never an owner.
    pub fn crypto_slot(&self) -> Address {
        Address::from_slice(&self.digest().as_slice()[..20])
    }

    pub fn amount_key_info(
        &self,
        enclave_public: &[u8; 32],
        creator_public: &[u8; 32],
        encrypted_creator_public_key: &[u8],
    ) -> Vec<u8> {
        let mut info = TRIBUTE_AMOUNT_KEY_INFO.to_vec();
        info.extend_from_slice(self.digest().as_slice());
        info.extend_from_slice(enclave_public);
        info.extend_from_slice(creator_public);
        info.extend_from_slice(encrypted_creator_public_key);
        info
    }
}

/// Canonical public record. No private key or plaintext amount is serialized.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedTributeV2 {
    pub context: TributeContextV2,
    pub encrypted_creator_public_key: Vec<u8>,
    pub encrypted_amounts: Vec<u8>,
}

impl EncryptedTributeV2 {
    /// Immutable Tribute blobs always use version one, unlike mutable ledgers.
    pub fn has_valid_encoding(&self) -> bool {
        self.context.tribute_id.worldwide_day() == self.context.worldwide_day
            && valid_immutable_blob(
                &self.encrypted_creator_public_key,
                TRIBUTE_PUBLIC_KEY_BLOB_LEN,
            )
            && valid_immutable_blob(&self.encrypted_amounts, TRIBUTE_AMOUNTS_BLOB_LEN)
    }
}

fn valid_immutable_blob(blob: &[u8], expected_len: usize) -> bool {
    blob.len() == expected_len && blob[..8] == 1u64.to_be_bytes()
}

/// Transient authorized calculation view, never a canonical storage body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeAmountsV2 {
    pub issuance_amount_minor: U256,
    pub nominal_amount_minor: U256,
}

impl TributeAmountsV2 {
    pub fn to_be_bytes(&self) -> [u8; 64] {
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(&self.issuance_amount_minor.to_be_bytes::<32>());
        bytes[32..].copy_from_slice(&self.nominal_amount_minor.to_be_bytes::<32>());
        bytes
    }

    pub fn from_be_bytes(bytes: &[u8; 64]) -> Self {
        Self {
            issuance_amount_minor: U256::from_be_slice(&bytes[..32]),
            nominal_amount_minor: U256::from_be_slice(&bytes[32..]),
        }
    }
}
