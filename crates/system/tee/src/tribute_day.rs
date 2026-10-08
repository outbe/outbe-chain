//! Pure request bindings for enclave-owned encrypted Tribute day arithmetic.

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::{
    time::WorldwideDay, tribute_day_encryption::EncryptedTributeDayAmountV2,
    tribute_encryption::EncryptedTributeV2,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TributeDayOperationV2 {
    Adjust {
        tribute: Box<EncryptedTributeV2>,
        add: bool,
    },
    /// Existing calculation views may supply a transient delta during the
    /// compatibility period. Canonical offer creation uses encrypted Tribute.
    AdjustTransient {
        nominal_amount_minor: U256,
        add: bool,
    },
    Reset {
        expected_total: U256,
    },
    Freeze,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeDayOpRequestV2 {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub previous: Option<EncryptedTributeDayAmountV2>,
    /// Digest of the exact previous public day metadata and intended transition.
    pub public_state_hash: B256,
    pub operation: TributeDayOperationV2,
}

pub fn day_operation_inputs_hash(
    request: &TributeDayOpRequestV2,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-inputs/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(request)?);
    Ok(keccak256(bytes))
}

pub fn day_operation_attestation_preimage(
    inputs_hash: B256,
    result: &EncryptedTributeDayAmountV2,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(result)?);
    Ok(bytes)
}

pub fn day_read_inputs_hash(
    record: &EncryptedTributeDayAmountV2,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-read/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(record)?);
    Ok(keccak256(bytes))
}

pub fn day_read_attestation_preimage(inputs_hash: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/tribute/day-read-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}
