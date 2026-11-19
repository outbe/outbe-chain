//! Pure request bindings for enclave-owned encrypted Tribute day arithmetic.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, B256, U256};
use outbe_core_common::{
    nod_encryption::EncryptedNodGroupLoadV1,
    tribute_day_encryption::{EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1},
    tribute_encryption::EncryptedTributeV2,
    wwd_entity_id::WwdEntityId,
};
use outbe_system_common::time::{EmissionDate, WorldwideDay};
use serde::{Deserialize, Serialize};

/// Operation applied to an encrypted Tribute day or league total.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TributeDayOperationV2 {
    /// Add or subtract an encrypted Tribute record's nominal amount.
    Adjust {
        /// Encrypted Tribute record supplying the delta.
        tribute: Box<EncryptedTributeV2>,
        /// True to add, false to subtract.
        add: bool,
    },
    /// Existing calculation views may supply a transient delta during the
    /// compatibility period. Canonical offer creation uses encrypted Tribute.
    AdjustTransient {
        /// Transient plaintext delta in minor units.
        nominal_amount_minor: U256,
        /// True to add, false to subtract.
        add: bool,
    },
    /// Freeze the day accumulator against further adjustments.
    Freeze,
}

/// Request to transform an encrypted Tribute day accumulator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeDayOpRequestV2 {
    /// Chain identifier binding the record.
    pub chain_id: u64,
    /// UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// Previous encrypted day record, or `None` for the first update of the day.
    pub previous: Option<EncryptedTributeDayAmountV2>,
    /// Digest of the exact previous public day metadata and intended transition.
    pub public_state_hash: B256,
    /// Arithmetic or freeze operation to perform.
    pub operation: TributeDayOperationV2,
}

/// Canonical commitment over a [`TributeDayOpRequestV2`].
pub fn day_operation_inputs_hash(
    request: &TributeDayOpRequestV2,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-inputs/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(request)?);
    Ok(keccak256(bytes))
}

/// Domain-separated preimage signed by the enclave for `TributeDayOpAppliedV2`.
pub fn day_operation_attestation_preimage(
    inputs_hash: B256,
    result: &EncryptedTributeDayAmountV2,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(result)?);
    Ok(bytes)
}

/// Canonical commitment over a `ReadTributeDayAmountV2` input record.
pub fn day_read_inputs_hash(
    record: &EncryptedTributeDayAmountV2,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-read/v2".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(record)?);
    Ok(keccak256(bytes))
}

/// Domain-separated preimage signed by the enclave for `TributeDayAmountReadV2`.
#[must_use]
pub fn day_read_attestation_preimage(inputs_hash: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/tribute/day-read-result/v2".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

/// One Tribute entry supplied to confidential Lysis fraction computation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisTributeInputV1 {
    /// Fidelity league assigned to the Tribute owner.
    pub league: u16,
    /// Optional NOD series scope for unclaimed load tracking.
    pub series_scope: Option<B256>,
    /// Canonical encrypted Tribute record.
    pub tribute: EncryptedTributeV2,
}

/// Encrypted unclaimed load aggregate produced for one NOD series scope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisSeriesLoadOutputV1 {
    /// NOD series scope digest.
    pub scope: B256,
    /// Number of unclaimed Tributes contributing to `unclaimed_tribute_load`.
    pub unclaimed_tribute_count: u64,
    /// Encrypted aggregate load for `scope`.
    pub unclaimed_tribute_load: EncryptedNodGroupLoadV1,
}

/// Request for confidential per-league Lysis fraction settlement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisFractionsRequestV1 {
    /// Chain identifier.
    pub chain_id: u64,
    /// Settled UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// Emission day (`YYYYMMDD`) binding the Lysis result.
    pub emission_day: EmissionDate,
    /// Settlement block timestamp.
    pub completed_at: u64,
    /// Metadosis limit in minor units for the emission day.
    pub metadosis_limit_minor: U256,
    /// The frozen nominal of every Tribute of the day, priced or not.
    pub day_nominal: EncryptedTributeDayAmountV2,
    /// Every Tribute of the day by ascending id. One without a series scope is unpriced: it is
    /// left out of the groups and the nominal.
    pub tributes: Vec<LysisTributeInputV1>,
}

/// Result of confidential per-league Lysis fraction settlement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisFractionsResultV1 {
    /// Encrypted per-league Lysis fractions record.
    pub day_fractions: EncryptedTributeDayFractionsV1,
    /// Nominal of the priced Tributes.
    pub total_nominal_minor: U256,
    /// Gratis demand in minor units.
    pub gratis_demand_minor: U256,
    /// Computed Lysis limit in minor units.
    pub lysis_limit_minor: U256,
    /// Total nominal minor units allocated under `lysis_limit_minor`.
    pub allocated_minor: U256,
    /// Tribute identifiers whose computed load rounded to zero.
    pub zero_load_tribute_ids: Vec<WwdEntityId>,
    /// Per-series unclaimed load outputs.
    pub series_loads: Vec<LysisSeriesLoadOutputV1>,
}

/// Canonical commitment over a [`LysisFractionsRequestV1`].
pub fn lysis_fractions_inputs_hash(
    request: &LysisFractionsRequestV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/lysis-fractions-inputs/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(request)?);
    Ok(keccak256(bytes))
}

/// Domain-separated preimage signed by the enclave for `LysisFractionsExecutedV1`.
pub fn lysis_fractions_attestation_preimage(
    inputs_hash: B256,
    result: &LysisFractionsResultV1,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/lysis-fractions-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(result)?);
    Ok(bytes)
}

/// Canonical commitment over a `ReadTributeDayFractionsV1` input record.
pub fn day_fractions_read_inputs_hash(
    record: &EncryptedTributeDayFractionsV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-fractions-read/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(record)?);
    Ok(keccak256(bytes))
}

/// Domain-separated preimage signed by the enclave for `TributeDayFractionsReadV1`.
pub fn day_fractions_read_attestation_preimage(
    inputs_hash: B256,
    fractions: &BTreeMap<u16, u64>,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-fractions-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(fractions)?);
    Ok(bytes)
}
