//! Encrypted Tribute-owned daily amount records.

use alloy_primitives::{keccak256, Address, B256};
use serde::{Deserialize, Serialize};

use crate::time::WorldwideDay;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedTributeDayAmountV2 {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub frozen: bool,
    pub operation_hash: B256,
    pub encrypted_amount: Vec<u8>,
}

impl EncryptedTributeDayAmountV2 {
    pub fn crypto_slot(&self) -> Address {
        let mut context = b"outbe/tribute/day-context/v2".to_vec();
        context.extend_from_slice(&self.chain_id.to_be_bytes());
        context.extend_from_slice(&self.worldwide_day.value().to_be_bytes());
        context.push(u8::from(self.frozen));
        context.extend_from_slice(self.operation_hash.as_slice());
        Address::from_slice(&keccak256(context).as_slice()[..20])
    }

    pub fn version(&self) -> Option<u64> {
        if self.encrypted_amount.len() != 56 {
            return None;
        }
        let version = u64::from_be_bytes(self.encrypted_amount[..8].try_into().ok()?);
        (version != 0).then_some(version)
    }
}
