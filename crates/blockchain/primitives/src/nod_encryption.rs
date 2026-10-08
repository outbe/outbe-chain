//! Self-contained encrypted NOD amounts.
use crate::{time::WorldwideDay, wwd_entity_id::WwdEntityId};
use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};
pub const NOD_AMOUNT_KEY_INFO: &[u8] = b"outbe/nod/amount-key/v2";
pub const NOD_AMOUNT_NONCE_INFO: &[u8] = b"outbe/nod/amount-nonce/v2";
pub const NOD_BLOB_LEN: usize = 8 + 32 + 16;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodTermsV2 {
    pub chain_id: u64,
    pub nod_id: WwdEntityId,
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}
impl NodTermsV2 {
    pub fn digest(&self) -> B256 {
        let mut bytes = b"outbe/nod/terms/v2".to_vec();
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(self.nod_id.as_slice());
        bytes.extend_from_slice(self.owner.as_slice());
        bytes.extend_from_slice(&self.worldwide_day.value().to_be_bytes());
        bytes.extend_from_slice(&self.league_id.to_be_bytes());
        bytes.extend_from_slice(&self.entry_price_minor.to_be_bytes::<32>());
        bytes.extend_from_slice(&self.issuance_currency.to_be_bytes());
        bytes.extend_from_slice(&self.reference_currency.to_be_bytes());
        keccak256(bytes)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedNodV2 {
    pub terms: NodTermsV2,
    pub encryption_binding: B256,
    pub encrypted_creator_public_key: Vec<u8>,
    pub encrypted_gratis_amount: Vec<u8>,
}
impl EncryptedNodV2 {
    pub fn context_digest(&self) -> B256 {
        let mut bytes = b"outbe/nod/context/v2".to_vec();
        bytes.extend_from_slice(self.terms.digest().as_slice());
        bytes.extend_from_slice(self.encryption_binding.as_slice());
        keccak256(bytes)
    }
    pub fn crypto_slot(&self) -> Address {
        Address::from_slice(&self.context_digest().as_slice()[..20])
    }
    pub fn amount_key_info(&self, enclave_public: &[u8; 32], creator_public: &[u8; 32]) -> Vec<u8> {
        let mut info = NOD_AMOUNT_KEY_INFO.to_vec();
        info.extend_from_slice(self.context_digest().as_slice());
        info.extend_from_slice(enclave_public);
        info.extend_from_slice(creator_public);
        info.extend_from_slice(&self.encrypted_creator_public_key);
        info
    }
    pub fn has_valid_encoding(&self) -> bool {
        self.terms.nod_id.worldwide_day() == self.terms.worldwide_day
            && [
                &self.encrypted_creator_public_key,
                &self.encrypted_gratis_amount,
            ]
            .iter()
            .all(|blob| blob.len() == NOD_BLOB_LEN && blob[..8] == 1u64.to_be_bytes())
    }
}
