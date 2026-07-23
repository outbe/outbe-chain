use std::collections::BTreeSet;

use alloy_primitives::{Address, B256, U256};

use crate::{
    common::EntityId36,
    error::ProtocolError,
    hash::hash_framed,
    intent::{DayType, PromisOperation},
    registry::HashDomain,
    schema::{impl_top_level_codec, require, wire_enum_u8, wire_struct, SchemaLimits},
};

wire_enum_u8! {
    pub enum CompletionStatus {
        Completed = 1,
    }
}

wire_struct! {
    pub struct NodActionV1 {
        pub raw_ordinal: u32,
        pub tribute_id: EntityId36,
        pub nod_id: EntityId36,
        pub owner: Address,
        pub wwd: u32,
        pub league_id: u16,
        pub floor_price_minor: U256,
        pub gratis_load_minor: U256,
        pub entry_price_minor: U256,
        pub cost_amount_minor: U256,
        pub issuance_currency: u16,
        pub reference_currency: u16,
        pub issued_at: u64,
        pub bucket_key: B256,
    }
}

wire_struct! {
    pub struct ContributorActionV1 {
        pub owner: Address,
        pub source_tribute_id: EntityId36,
        pub nominal_amount_minor: U256,
    }
}

wire_struct! {
    pub struct AuctionBriefActionV1 {
        pub wwd: u32,
        pub supply: U256,
        pub entry_price: U256,
        pub is_green: bool,
        pub logical_anchor: u64,
        pub expected_accepted: bool,
    }
}

wire_struct! {
    pub struct PromisDeltaActionV1 {
        pub accumulator_key: B256,
        pub operation: PromisOperation,
        pub applied_delta: U256,
    }
}

wire_struct! {
    pub struct MetadosisCompletionSummaryV1 {
        pub wwd: u32,
        pub pending_nonce: u64,
        pub day_type: DayType,
        pub tribute_nominal_total: U256,
        pub gratis_demand: U256,
        pub gratis_supply: U256,
        pub gratis_allocation: U256,
        pub remaining_gratis: U256,
        pub net_gratis_allocation: U256,
        pub post_lysis_remainder: U256,
        pub promis_delta: U256,
        pub status: CompletionStatus,
        pub logical_evaluation_height: u64,
        pub logical_evaluation_time: u64,
    }
}

wire_struct! {
    pub struct ActionStreamV1 {
        pub ordered_nod_actions: Vec<NodActionV1>,
        pub ordered_eligible_contributors: Vec<ContributorActionV1>,
        pub auction_brief_action: AuctionBriefActionV1,
        pub promis_delta: PromisDeltaActionV1,
        pub metadosis_completion_summary: MetadosisCompletionSummaryV1,
    }
    validate = validate_action_stream;
}
impl_top_level_codec!(ActionStreamV1, ActionStreamV1);

wire_struct! {
    pub struct ExactCountsV1 {
        pub tribute_count: u32,
        pub nod_count: u32,
        pub bucket_count: u32,
        pub contributor_count: u32,
        pub semantic_event_count: u32,
    }
}

wire_struct! {
    pub struct ConservationTotalsV1 {
        pub tribute_nominal_total: U256,
        pub eligible_nominal_total: U256,
        pub metadosis_limit: U256,
        pub gratis_demand: U256,
        pub gratis_supply: U256,
        pub gratis_allocation: U256,
        pub nod_gratis_consumed: U256,
        pub remaining_gratis: U256,
        pub allocation_limit_remainder: U256,
        pub post_lysis_remainder: U256,
        pub desis_supply: U256,
        pub promis_delta: U256,
        pub nod_cost_total: U256,
    }
}

wire_struct! {
    pub struct ResultRootsV1 {
        pub nod_root: B256,
        pub bucket_root: B256,
        pub contributor_root: B256,
        pub output_manifest_root: B256,
    }
}

wire_struct! {
    pub struct LysisArithmeticSummaryV1 {
        pub input_manifest_hash: B256,
        pub plan_hash: B256,
        pub unit_artifact_root: B256,
        pub fidelity_fraction_root: B256,
        pub gratis_prefix_root: B256,
        pub roots: ResultRootsV1,
        pub counts: ExactCountsV1,
        pub conservation: ConservationTotalsV1,
        pub first_error_ordinal: Option<u32>,
    }
}
impl_top_level_codec!(LysisArithmeticSummaryV1, LysisArithmeticSummaryV1);

wire_struct! {
    pub struct BoundedLysisResultV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub input_manifest_hash: B256,
        pub plan_hash: B256,
        pub unit_artifact_root: B256,
        pub fidelity_fraction_root: B256,
        pub gratis_prefix_root: B256,
        pub action_stream: ActionStreamV1,
        pub tribute_count: u32,
        pub tribute_nominal_total: U256,
        pub remaining_gratis: U256,
        pub roots: ResultRootsV1,
        pub counts: ExactCountsV1,
        pub conservation: ConservationTotalsV1,
        pub arithmetic_commitment: B256,
        pub event_summary_hash: B256,
    }
    validate = validate_bounded_result;
}
impl_top_level_codec!(BoundedLysisResultV1, BoundedLysisResultV1);

wire_struct! {
    pub struct ActivationPayloadV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub action_stream_hash: Option<B256>,
        pub roots: ResultRootsV1,
        pub counts: ExactCountsV1,
        pub conservation: ConservationTotalsV1,
        pub arithmetic_commitment: B256,
        pub event_summary_hash: B256,
        pub da_encoding_commitment: Option<B256>,
    }
    validate = validate_activation_payload;
}
impl_top_level_codec!(ActivationPayloadV1, ActivationPayloadV1);

impl ActionStreamV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.ordered_nod_actions.len() <= limits.max_action_items
                && self.ordered_eligible_contributors.len() <= limits.max_action_items,
            "action stream item cap",
        )?;
        let mut tribute_ids = BTreeSet::new();
        let mut nod_ids = BTreeSet::new();
        let mut owners = BTreeSet::new();
        for (index, action) in self.ordered_nod_actions.iter().enumerate() {
            require(
                usize::try_from(action.raw_ordinal).ok() == Some(index),
                "nod raw ordinals gap-free from zero",
            )?;
            require(tribute_ids.insert(action.tribute_id), "unique tribute id")?;
            require(nod_ids.insert(action.nod_id), "unique nod id")?;
            require(owners.insert(action.owner), "unique nod owner")?;
            if let Some(previous) = index
                .checked_sub(1)
                .and_then(|previous| self.ordered_nod_actions.get(previous))
            {
                require(
                    (previous.raw_ordinal, previous.tribute_id)
                        < (action.raw_ordinal, action.tribute_id),
                    "nod actions strictly ordered",
                )?;
            }
        }
        for pair in self.ordered_eligible_contributors.windows(2) {
            require(
                (pair[0].owner, pair[0].source_tribute_id)
                    < (pair[1].owner, pair[1].source_tribute_id),
                "contributors strictly ordered",
            )?;
        }
        Ok(())
    }

    pub fn action_stream_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(
            HashDomain::BoundedLysisActions,
            &self.encode_canonical(limits)?,
        )
    }
}

impl BoundedLysisResultV1 {
    #[must_use]
    pub fn arithmetic_summary(&self) -> LysisArithmeticSummaryV1 {
        LysisArithmeticSummaryV1 {
            input_manifest_hash: self.input_manifest_hash,
            plan_hash: self.plan_hash,
            unit_artifact_root: self.unit_artifact_root,
            fidelity_fraction_root: self.fidelity_fraction_root,
            gratis_prefix_root: self.gratis_prefix_root,
            roots: self.roots.clone(),
            counts: self.counts.clone(),
            conservation: self.conservation.clone(),
            first_error_ordinal: None,
        }
    }

    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        self.action_stream.validate_semantics(limits)?;
        let nod_count =
            u32::try_from(self.action_stream.ordered_nod_actions.len()).map_err(|_| {
                ProtocolError::IntegerOverflow {
                    what: "nod action count",
                }
            })?;
        let contributor_count = u32::try_from(
            self.action_stream.ordered_eligible_contributors.len(),
        )
        .map_err(|_| ProtocolError::IntegerOverflow {
            what: "contributor action count",
        })?;
        require(
            self.tribute_count == self.counts.tribute_count
                && self.tribute_count == self.counts.nod_count
                && nod_count == self.counts.nod_count
                && contributor_count == self.counts.contributor_count
                && self.counts.contributor_count <= self.tribute_count
                && self.counts.bucket_count <= self.tribute_count,
            "result exact counts",
        )?;
        require(
            self.tribute_nominal_total == self.conservation.tribute_nominal_total
                && self.remaining_gratis == self.conservation.remaining_gratis,
            "result scalar conservation binding",
        )?;
        let gratis_sum = self
            .conservation
            .nod_gratis_consumed
            .checked_add(self.conservation.remaining_gratis)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "gratis conservation",
            })?;
        require(
            gratis_sum == self.conservation.gratis_allocation,
            "gratis conservation",
        )?;
        let post_lysis = self
            .conservation
            .remaining_gratis
            .checked_add(self.conservation.allocation_limit_remainder)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "post lysis remainder",
            })?;
        require(
            post_lysis == self.conservation.post_lysis_remainder,
            "post lysis remainder conservation",
        )?;
        let arithmetic = self.arithmetic_summary();
        let commitment = hash_framed(
            HashDomain::LysisArithmetic,
            &arithmetic.encode_canonical(limits)?,
        )?;
        require(
            commitment == self.arithmetic_commitment,
            "arithmetic commitment",
        )
    }

    pub fn activation_payload(
        &self,
        limits: &SchemaLimits,
    ) -> Result<ActivationPayloadV1, ProtocolError> {
        self.validate_semantics(limits)?;
        Ok(ActivationPayloadV1 {
            protocol_bundle_hash: self.protocol_bundle_hash,
            job_id: self.job_id,
            attempt: self.attempt,
            action_stream_hash: Some(self.action_stream.action_stream_hash(limits)?),
            roots: self.roots.clone(),
            counts: self.counts.clone(),
            conservation: self.conservation.clone(),
            arithmetic_commitment: self.arithmetic_commitment,
            event_summary_hash: self.event_summary_hash,
            da_encoding_commitment: None,
        })
    }
}

impl ActivationPayloadV1 {
    pub fn result_digest(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        require(
            self.action_stream_hash.is_some(),
            "PoC action stream commitment present",
        )?;
        require(
            self.da_encoding_commitment.is_none(),
            "PoC DA commitment absent",
        )?;
        hash_framed(HashDomain::Result, &self.encode_canonical(limits)?)
    }
}

fn validate_action_stream(
    stream: &ActionStreamV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    stream.validate_semantics(limits)
}

fn validate_bounded_result(
    result: &BoundedLysisResultV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    result.validate_semantics(limits)
}

fn validate_activation_payload(
    payload: &ActivationPayloadV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        payload.action_stream_hash.is_some(),
        "PoC action stream commitment present",
    )?;
    require(
        payload.da_encoding_commitment.is_none(),
        "PoC DA commitment absent",
    )
}
