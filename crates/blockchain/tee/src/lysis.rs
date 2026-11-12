//! Confidential Lysis request, result and attestation preimages.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::EncryptedNodV1, time::WorldwideDay, tribute_encryption::EncryptedTributeV2,
};
use serde::{Deserialize, Serialize};

use crate::{
    protocol::FidelitySnapshotRequest,
    response_auth::{attestation, hash},
    TransportError,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisRequest {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    /// The UTC day whose VWAP prices the Nods; it names their series.
    pub emission_day: u32,
    pub timestamp: u64,
    pub lysis_limit_minor: U256,
    pub tributes: Vec<EncryptedTributeV2>,
    pub fidelity: FidelitySnapshotRequest,
    pub entry_prices: Vec<(u16, Option<U256>)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisNod {
    pub nod: EncryptedNodV1,
    pub settlement_cost_minor: U256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisResult {
    pub nods: Vec<LysisNod>,
    pub total_nominal_minor: U256,
    pub unused_lysis_limit_minor: U256,
}

pub fn inputs_hash(request: &LysisRequest) -> Result<B256, TransportError> {
    hash(b"outbe/lysis/inputs", request).map_err(TransportError::codec)
}

pub fn result_preimage(inputs: B256, result: &LysisResult) -> Result<Vec<u8>, TransportError> {
    attestation(b"outbe/lysis/result", inputs, result).map_err(TransportError::codec)
}

pub fn rejection_preimage(inputs: B256) -> Vec<u8> {
    let mut bytes = b"outbe/lysis/calculation-rejected".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes
}
