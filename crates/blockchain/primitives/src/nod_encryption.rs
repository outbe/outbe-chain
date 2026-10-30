//! Self-contained encrypted NOD amounts.
use crate::series_id::{InvalidSeriesId, SeriesId};
use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};
pub const NOD_AMOUNT_KEY_INFO: &[u8] = b"outbe/nod/amount-key/v2";
pub const NOD_AMOUNT_NONCE_INFO: &[u8] = b"outbe/nod/amount-nonce/v2";
pub const NOD_BLOB_LEN: usize = 8 + 32 + 16;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodTermsV1 {
    pub chain_id: u64,
    pub nod_id: U256,
    pub owner: Address,
    /// The UTC day whose VWAP prices the Nod, `YYYYMMDD`. With the currencies it names the series.
    pub emission_day: u32,
    pub league_id: u16,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}
impl NodTermsV1 {
    pub fn series_id(&self) -> Result<SeriesId, InvalidSeriesId> {
        SeriesId::for_pair(
            self.emission_day,
            self.issuance_currency,
            self.reference_currency,
        )
    }

    pub fn digest(&self) -> B256 {
        let mut bytes = b"outbe/nod/terms/v2".to_vec();
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(&self.nod_id.to_be_bytes::<32>());
        bytes.extend_from_slice(self.owner.as_slice());
        bytes.extend_from_slice(&self.emission_day.to_be_bytes());
        bytes.extend_from_slice(&self.league_id.to_be_bytes());
        bytes.extend_from_slice(&self.entry_price_minor.to_be_bytes::<32>());
        bytes.extend_from_slice(&self.issuance_currency.to_be_bytes());
        bytes.extend_from_slice(&self.reference_currency.to_be_bytes());
        keccak256(bytes)
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedNodV1 {
    pub terms: NodTermsV1,
    pub encryption_binding: B256,
    pub encrypted_gratis_amount: Vec<u8>,
}
impl EncryptedNodV1 {
    pub fn context_digest(&self) -> B256 {
        let mut bytes = b"outbe/nod/context/v2".to_vec();
        bytes.extend_from_slice(self.terms.digest().as_slice());
        bytes.extend_from_slice(self.encryption_binding.as_slice());
        keccak256(bytes)
    }
    pub fn crypto_slot(&self) -> Address {
        Address::from_slice(&self.context_digest().as_slice()[..20])
    }
    pub fn amount_key_info(&self) -> Vec<u8> {
        let mut info = NOD_AMOUNT_KEY_INFO.to_vec();
        info.extend_from_slice(self.context_digest().as_slice());
        info
    }
    pub fn has_valid_encoding(&self) -> bool {
        self.encrypted_gratis_amount.len() == NOD_BLOB_LEN
            && self.encrypted_gratis_amount[..8] == 1u64.to_be_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_use_native_u256_serde_and_full_width_big_endian_hashing() {
        let terms = NodTermsV1 {
            chain_id: 1,
            nod_id: U256::ONE,
            owner: Address::repeat_byte(0x42),
            emission_day: 20261030,
            league_id: 2,
            entry_price_minor: U256::from(3),
            issuance_currency: 840,
            reference_currency: 978,
        };
        let json = serde_json::to_value(&terms).unwrap();
        assert_eq!(json["nod_id"], "0x1");
        assert_eq!(serde_json::from_value::<NodTermsV1>(json).unwrap(), terms);
        let mut expected = b"outbe/nod/terms/v2".to_vec();
        expected.extend_from_slice(&1u64.to_be_bytes());
        let mut id = [0u8; 32];
        id[31] = 1;
        expected.extend_from_slice(&id);
        expected.extend_from_slice(&[0x42; 20]);
        expected.extend_from_slice(&20261030u32.to_be_bytes());
        expected.extend_from_slice(&2u16.to_be_bytes());
        let mut entry_price = [0u8; 32];
        entry_price[31] = 3;
        expected.extend_from_slice(&entry_price);
        expected.extend_from_slice(&840u16.to_be_bytes());
        expected.extend_from_slice(&978u16.to_be_bytes());
        assert_eq!(terms.digest(), keccak256(expected));
    }
}
