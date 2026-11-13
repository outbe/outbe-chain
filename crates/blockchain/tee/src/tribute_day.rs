//! Pure request bindings for enclave-owned encrypted Tribute day arithmetic.

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::{
    nod_encryption::EncryptedNodGroupLoadV1,
    time::WorldwideDay,
    tribute_day_encryption::{
        EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1, EncryptedTributeLeagueAmountV1,
    },
    tribute_encryption::EncryptedTributeV2,
    wwd_entity_id::WwdEntityId,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TributeLeagueOpRequestV1 {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub league: u16,
    pub previous: Option<EncryptedTributeLeagueAmountV1>,
    pub public_state_hash: B256,
    pub operation: TributeDayOperationV2,
}

pub fn league_operation_inputs_hash(
    request: &TributeLeagueOpRequestV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/league-inputs/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(request)?);
    Ok(keccak256(bytes))
}

pub fn league_operation_attestation_preimage(
    inputs_hash: B256,
    result: &EncryptedTributeLeagueAmountV1,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/league-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(result)?);
    Ok(bytes)
}

pub fn league_read_inputs_hash(
    record: &EncryptedTributeLeagueAmountV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/league-read/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(record)?);
    Ok(keccak256(bytes))
}

pub fn league_read_attestation_preimage(inputs_hash: B256, amount: U256) -> Vec<u8> {
    let mut bytes = b"outbe/tribute/league-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&amount.to_be_bytes::<32>());
    bytes
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisTributeInputV1 {
    pub league: u16,
    pub series_scope: Option<B256>,
    pub tribute: EncryptedTributeV2,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisSeriesLoadOutputV1 {
    pub scope: B256,
    pub unclaimed_tribute_count: u64,
    pub unclaimed_tribute_load: EncryptedNodGroupLoadV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisFractionsRequestV1 {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub completed_at: u64,
    pub metadosis_limit_minor: U256,
    /// A Tribute without a series scope is unpriced: it is left out of the groups and the nominal.
    pub tributes: Vec<LysisTributeInputV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisFractionsResultV1 {
    pub day_fractions: EncryptedTributeDayFractionsV1,
    /// Nominal of the priced Tributes.
    pub total_nominal_minor: U256,
    pub gratis_demand_minor: U256,
    pub lysis_limit_minor: U256,
    pub allocated_minor: U256,
    pub zero_load_tribute_ids: Vec<WwdEntityId>,
    pub series_loads: Vec<LysisSeriesLoadOutputV1>,
}

pub fn lysis_fractions_inputs_hash(
    request: &LysisFractionsRequestV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/lysis-fractions-inputs/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(request)?);
    Ok(keccak256(bytes))
}

pub fn lysis_fractions_attestation_preimage(
    inputs_hash: B256,
    result: &LysisFractionsResultV1,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/lysis-fractions-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(result)?);
    Ok(bytes)
}

pub fn day_fractions_read_inputs_hash(
    record: &EncryptedTributeDayFractionsV1,
) -> Result<B256, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-fractions-read/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(record)?);
    Ok(keccak256(bytes))
}

pub fn day_fractions_read_attestation_preimage(
    inputs_hash: B256,
    fractions: &BTreeMap<u16, u64>,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = b"outbe/tribute/day-fractions-read-result/v1".to_vec();
    bytes.extend_from_slice(inputs_hash.as_slice());
    bytes.extend_from_slice(&serde_json::to_vec(fractions)?);
    Ok(bytes)
}
