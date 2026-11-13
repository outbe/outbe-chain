//! Confidential Lysis request, result and attestation preimages.

use alloy_primitives::{B256, U256};
use outbe_core_common::{nod_encryption::EncryptedNodV1, tribute_encryption::EncryptedTributeV2};
use outbe_primitives::time::WorldwideDay;
use outbe_tee::TransportError;
use serde::{Deserialize, Serialize};

use super::{
    protocol::FidelitySnapshotRequest,
    response_auth::{attestation, hash},
};

/// Confidential Lysis batch settlement request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisRequest {
    /// Chain identifier.
    pub chain_id: u64,
    /// Settled UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// The UTC day whose VWAP prices the Nods; it names their series.
    pub emission_day: u32,
    /// Settlement block timestamp.
    pub timestamp: u64,
    /// Maximum minor units available for Lysis allocation.
    pub lysis_limit_minor: U256,
    /// Encrypted Tributes participating in Lysis.
    pub tributes: Vec<EncryptedTributeV2>,
    /// Fidelity league snapshot input batch.
    pub fidelity: FidelitySnapshotRequest,
    /// Per-currency entry prices (`(iso_4217, optional_vwap_minor)`).
    pub entry_prices: Vec<(u16, Option<U256>)>,
}

/// One issued encrypted NOD and its plaintext settlement cost from Lysis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisNod {
    /// Encrypted NOD record.
    pub nod: EncryptedNodV1,
    /// Settlement cost in minor units.
    pub settlement_cost_minor: U256,
}

/// Result of a confidential Lysis batch execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisResult {
    /// Issued encrypted NOD records in Tribute order.
    pub nods: Vec<LysisNod>,
    /// Total nominal Tribute amount across `tributes`.
    pub total_nominal_minor: U256,
    /// Remaining unallocated `lysis_limit_minor`.
    pub unused_lysis_limit_minor: U256,
}

/// Canonical commitment over a [`LysisRequest`].
pub fn inputs_hash(request: &LysisRequest) -> Result<B256, TransportError> {
    hash(b"outbe/lysis/inputs", request).map_err(TransportError::codec)
}

/// Domain-separated preimage signed by the enclave for `LysisExecuted`.
pub fn result_preimage(inputs: B256, result: &LysisResult) -> Result<Vec<u8>, TransportError> {
    attestation(b"outbe/lysis/result", inputs, result).map_err(TransportError::codec)
}

/// Domain-separated preimage signed by the enclave for `LysisRejected`.
#[must_use]
pub fn rejection_preimage(inputs: B256) -> Vec<u8> {
    let mut bytes = b"outbe/lysis/calculation-rejected".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes
}
