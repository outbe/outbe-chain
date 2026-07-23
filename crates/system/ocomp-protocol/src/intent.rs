use alloy_primitives::{B256, U256};

use crate::{
    common::{BoundedBytes, ProofBytes},
    error::ProtocolError,
    hash::hash_framed,
    registry::HashDomain,
    schema::{impl_top_level_codec, require, wire_enum_u8, wire_struct, SchemaLimits},
};

wire_enum_u8! {
    pub enum AuctionEntryPriceSource {
        LastClosedDayVwap = 1,
        CurrentVwapFallback = 2,
    }
}

wire_enum_u8! {
    pub enum DayType {
        Green = 1,
        Red = 2,
    }
}

wire_enum_u8! {
    pub enum DesisExpectedStage {
        None = 0,
    }
}

wire_enum_u8! {
    pub enum PromisOperation {
        CheckedCommutativeAdd = 1,
    }
}

wire_enum_u8! {
    pub enum MetadosisExpectedStatus {
        OffchainPending = 1,
    }
}

wire_enum_u8! {
    pub enum ParentProofKind {
        Finalization = 1,
    }
}

wire_struct! {
    pub struct PreAdmissionEnvelopeV1 {
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub fork_id: B256,
        pub wwd: u32,
        pub sealed_tribute_collection_root: B256,
        pub sealed_tribute_count: u32,
        pub sealed_tribute_canonical_body_bytes: u64,
        pub distinct_owner_count: u32,
        pub distinct_reference_currency_count: u16,
        pub max_fidelity_cohorts_observed: u16,
        pub oracle_wwd_pair_entries_observed: u32,
        pub active_scurve_entries_observed: u32,
        pub auction_entry_price: U256,
        pub auction_entry_price_source: AuctionEntryPriceSource,
        pub auction_entry_price_source_day: u32,
        pub oracle_state_version: u64,
        pub fidelity_opening_upper_bound: u32,
        pub oracle_opening_upper_bound: u32,
        pub input_encoded_bytes_upper_bound: u64,
        pub output_record_upper_bound: u32,
        pub action_stream_bytes_upper_bound: u64,
        pub activation_bytes_upper_bound: u64,
        pub retained_bytes_upper_bound: u64,
        pub correctness_profile_id: B256,
        pub capacity_profile_id: B256,
    }
}
impl_top_level_codec!(PreAdmissionEnvelopeV1, PreAdmissionEnvelopeV1);

wire_struct! {
    pub struct FrozenMetadosisValuesV1 {
        pub day_type: DayType,
        pub metadosis_limit: U256,
        pub previous_vwap: U256,
        pub current_vwap: U256,
        pub gratis_demand: U256,
        pub gratis_supply: U256,
        pub gratis_allocation: U256,
        pub allocation_limit_remainder: U256,
        pub auction_entry_price: U256,
    }
}

wire_struct! {
    pub struct TributePartitionReservationV1 {
        pub wwd: u32,
        pub pending_nonce: u64,
        pub source_generation: u64,
        pub collection_key: B256,
        pub sealed_collection_root: B256,
        pub exact_count: u32,
        pub exact_nominal_total: U256,
        pub state_version: u64,
    }
}

wire_struct! {
    pub struct NodNamespaceReservationV1 {
        pub wwd: u32,
        pub pending_nonce: u64,
        pub target_generation: u64,
        pub namespace_root_before: B256,
        pub max_nod_count: u32,
        pub state_version: u64,
    }
}

wire_struct! {
    pub struct ContributorSeriesReservationV1 {
        pub series_id: u32,
        pub pending_nonce: u64,
        pub expected_series_version: u64,
        pub max_contributor_count: u32,
        pub max_eligible_nominal_total: U256,
    }
}

wire_struct! {
    pub struct DesisBriefReservationV1 {
        pub wwd: u32,
        pub pending_nonce: u64,
        pub expected_stage: DesisExpectedStage,
        pub expected_state_version: u64,
        pub logical_anchor: u64,
        pub max_supply: U256,
    }
}

wire_struct! {
    pub struct PromisDeltaReservationV1 {
        pub accumulator_key: B256,
        pub pending_nonce: u64,
        pub operation: PromisOperation,
        pub max_delta: U256,
        pub state_version: u64,
    }
}

wire_struct! {
    pub struct MetadosisReservationV1 {
        pub wwd: u32,
        pub pending_nonce: u64,
        pub expected_status: MetadosisExpectedStatus,
        pub state_version: u64,
    }
}

wire_struct! {
    pub struct TargetReservationSetV1 {
        pub tribute: TributePartitionReservationV1,
        pub nod: NodNamespaceReservationV1,
        pub contributors: ContributorSeriesReservationV1,
        pub desis: DesisBriefReservationV1,
        pub promis: PromisDeltaReservationV1,
        pub metadosis: MetadosisReservationV1,
    }
}
impl_top_level_codec!(TargetReservationSetV1, TargetReservationSetV1);

wire_struct! {
    pub struct JobIntentV1 {
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub fork_id: B256,
        pub wwd: u32,
        pub pending_nonce: u64,
        pub attempt: u32,
        pub protocol_bundle_hash: B256,
        pub ce_sealed_root: B256,
        pub sealed_tribute_collection_key: B256,
        pub sealed_tribute_collection_root: B256,
        pub authenticated_day_count: u32,
        pub authenticated_day_nominal: U256,
        pub pre_admission_envelope_hash: B256,
        pub source_availability_policy_id: B256,
        pub frozen_metadosis_values: FrozenMetadosisValuesV1,
        pub logical_evaluation_height: u64,
        pub logical_evaluation_time: u64,
        pub target_reservations: TargetReservationSetV1,
        pub result_committee_snapshot_hash: B256,
        pub custody_committee_epoch_hash: Option<B256>,
        pub deadline_height: u64,
    }
    validate = validate_job_intent;
}
impl_top_level_codec!(JobIntentV1, JobIntentV1);

wire_struct! {
    pub struct CertifiedParentAccountingMetadataV2 {
        pub finalized_block_number: u64,
        pub finalized_block_hash: B256,
        pub finalized_epoch: u64,
        pub finalized_view: u64,
        pub parent_view: u64,
        pub ordered_committee: Vec<BoundedBytes>,
        pub signer_bitmap: BoundedBytes,
        pub canonical_commonware_finalization_proof: ProofBytes,
        pub committee_set_hash: B256,
        pub vrf_material_version: u16,
        pub vrf_group_public_key_hash: B256,
        pub proof_kind: ParentProofKind,
        pub missed_proposers: Vec<B256>,
    }
}

wire_struct! {
    pub struct FinalizedIntentProofV1 {
        pub chain_id: u64,
        pub genesis_hash: B256,
        pub fork_id: B256,
        pub protocol_bundle_hash: B256,
        pub canonical_request_header_rlp: ProofBytes,
        pub parent_accounting: CertifiedParentAccountingMetadataV2,
        pub historical_committee_membership_proof: ProofBytes,
        pub canonical_job_intent: BoundedBytes,
        pub intent_account_proof: ProofBytes,
        pub intent_storage_proof: ProofBytes,
    }
}
impl_top_level_codec!(FinalizedIntentProofV1, FinalizedIntentProofV1);

impl PreAdmissionEnvelopeV1 {
    pub fn envelope_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::PreAdmission, &self.encode_canonical(limits)?)
    }
}

impl TargetReservationSetV1 {
    pub fn reservation_set_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        hash_framed(HashDomain::ReservationSet, &self.encode_canonical(limits)?)
    }

    pub fn validate_for_intent(&self, intent: &JobIntentV1) -> Result<(), ProtocolError> {
        require(
            self.tribute.wwd == intent.wwd
                && self.nod.wwd == intent.wwd
                && self.desis.wwd == intent.wwd
                && self.metadosis.wwd == intent.wwd
                && self.contributors.series_id == intent.wwd,
            "reservation owner day binding",
        )?;
        require(
            self.tribute.pending_nonce == intent.pending_nonce
                && self.nod.pending_nonce == intent.pending_nonce
                && self.contributors.pending_nonce == intent.pending_nonce
                && self.desis.pending_nonce == intent.pending_nonce
                && self.promis.pending_nonce == intent.pending_nonce
                && self.metadosis.pending_nonce == intent.pending_nonce,
            "reservation nonce binding",
        )?;
        require(
            self.tribute.exact_count == intent.authenticated_day_count
                && self.tribute.exact_nominal_total == intent.authenticated_day_nominal
                && self.nod.max_nod_count == self.tribute.exact_count
                && self.contributors.max_contributor_count == self.tribute.exact_count
                && self.contributors.max_eligible_nominal_total == self.tribute.exact_nominal_total,
            "reservation source bounds",
        )?;
        require(
            self.desis.max_supply <= intent.frozen_metadosis_values.metadosis_limit
                && self.promis.max_delta <= intent.frozen_metadosis_values.metadosis_limit,
            "reservation monetary bounds",
        )
    }
}

impl JobIntentV1 {
    pub fn intent_id(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics()?;
        hash_framed(HashDomain::Intent, &self.encode_canonical(limits)?)
    }

    pub fn validate_semantics(&self) -> Result<(), ProtocolError> {
        require(
            u64::from(self.attempt) == self.pending_nonce,
            "attempt equals checked pending nonce",
        )?;
        require(
            self.deadline_height > self.logical_evaluation_height,
            "deadline follows logical evaluation height",
        )?;
        self.target_reservations.validate_for_intent(self)
    }

    pub fn job_id(
        &self,
        finalized_request_block_hash: B256,
        finalized_request_state_root: B256,
        limits: &SchemaLimits,
    ) -> Result<B256, ProtocolError> {
        let mut payload = Vec::with_capacity(96);
        payload.extend_from_slice(self.intent_id(limits)?.as_slice());
        payload.extend_from_slice(finalized_request_block_hash.as_slice());
        payload.extend_from_slice(finalized_request_state_root.as_slice());
        hash_framed(HashDomain::Job, &payload)
    }
}

fn validate_job_intent(intent: &JobIntentV1, _limits: &SchemaLimits) -> Result<(), ProtocolError> {
    intent.validate_semantics()
}

impl FinalizedIntentProofV1 {
    pub fn decoded_intent(&self, limits: &SchemaLimits) -> Result<JobIntentV1, ProtocolError> {
        let intent = JobIntentV1::decode_canonical(&self.canonical_job_intent.0, limits)?;
        require(
            intent.chain_id == self.chain_id,
            "finality proof chain binding",
        )?;
        require(
            intent.genesis_hash == self.genesis_hash,
            "finality proof genesis binding",
        )?;
        require(
            intent.fork_id == self.fork_id,
            "finality proof fork binding",
        )?;
        require(
            intent.protocol_bundle_hash == self.protocol_bundle_hash,
            "finality proof bundle binding",
        )?;
        Ok(intent)
    }
}
