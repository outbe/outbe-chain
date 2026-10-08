//! Closed, storage-independent Lysis V1 result finalization.
//!
//! The process host supplies bounded exact-order cursors over objects it has
//! already reopened and authenticated. This module owns every semantic result
//! field: callers cannot provide a result root, conservation scalar or
//! arithmetic commitment.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_ocomp_protocol::{
    hash_framed,
    input::InputManifestV1,
    intent::JobIntentV1,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisResultV1,
        MetadosisCompletionSummaryV1, OutputManifestEntryV1, ResultChunkV1, ResultRootsV1,
    },
    shuffle::ShuffleBucketRecordV1,
    unit::{PlanCommitmentV1, UnitArtifactV1, UnitPhase},
    CanonicalWriter, ListKind, ObjectKind, ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};

use super::{
    artifacts::{
        decode_fixed_reduce_output, decode_gratis_prefix_down_output, GratisPrefixDownOutputV1,
        LysisArtifactErrorV1,
    },
    phases::{GratisLeafPrefixV1, NodBucketKeyV1},
    planner::{LysisPlanTopologyV1, PlannedUnitPositionV1, PlannerErrorV1},
    result::{
        decode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1,
        RootReduceSummaryV1,
    },
    LeagueFractionV1,
};

#[derive(Debug)]
pub enum LysisFinalizationErrorV1 {
    Authority(&'static str),
    Protocol(ProtocolError),
    Artifact(LysisArtifactErrorV1),
    Planner(PlannerErrorV1),
}

impl core::fmt::Display for LysisFinalizationErrorV1 {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Authority(invariant) => {
                write!(
                    formatter,
                    "Lysis finalization authority mismatch: {invariant}"
                )
            }
            Self::Protocol(error) => write!(formatter, "Lysis finalization protocol: {error}"),
            Self::Artifact(error) => write!(formatter, "Lysis finalization artifact: {error}"),
            Self::Planner(error) => write!(formatter, "Lysis finalization planner: {error}"),
        }
    }
}

impl std::error::Error for LysisFinalizationErrorV1 {}

impl From<ProtocolError> for LysisFinalizationErrorV1 {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl From<LysisArtifactErrorV1> for LysisFinalizationErrorV1 {
    fn from(error: LysisArtifactErrorV1) -> Self {
        Self::Artifact(error)
    }
}

impl From<PlannerErrorV1> for LysisFinalizationErrorV1 {
    fn from(error: PlannerErrorV1) -> Self {
        Self::Planner(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizationUnitArtifactV1 {
    pub plan_ordinal: u32,
    pub position: PlannedUnitPositionV1,
    pub artifact: UnitArtifactV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizationResultChunkV1 {
    pub chunk_ordinal: u32,
    pub summary: RootReduceSummaryV1,
    pub output_manifest_entry: OutputManifestEntryV1,
    pub canonical_chunk_bytes: Vec<u8>,
}

pub struct VerifiedLysisFinalizationInputsV1<'a, U, C, B> {
    pub finalized_job_id: B256,
    pub intent: &'a JobIntentV1,
    pub input_manifest: &'a InputManifestV1,
    pub plan: &'a PlanCommitmentV1,
    pub root_reduce_summary: &'a RootReduceSummaryV1,
    pub unit_artifacts: U,
    pub result_chunks: C,
    pub bucket_records: B,
}

pub fn finalize_v1<U, C, B>(
    inputs: VerifiedLysisFinalizationInputsV1<'_, U, C, B>,
    limits: &SchemaLimits,
) -> Result<LysisResultV1, LysisFinalizationErrorV1>
where
    U: IntoIterator<Item = Result<FinalizationUnitArtifactV1, LysisFinalizationErrorV1>>,
    C: IntoIterator<Item = Result<FinalizationResultChunkV1, LysisFinalizationErrorV1>>,
    B: IntoIterator<Item = Result<ShuffleBucketRecordV1, LysisFinalizationErrorV1>>,
{
    validate_authority(&inputs, limits)?;
    let topology = LysisPlanTopologyV1::new(inputs.plan.primary_work_unit_count)?;
    let plan_hash = inputs.plan.plan_hash(limits)?;
    let binding = ResultBindingV1 {
        job_id: inputs.finalized_job_id,
        plan: inputs.plan,
        plan_hash,
    };
    let mut unit_artifacts = UnitArtifactStreamV1::new(binding, topology, limits)?;
    for item in inputs.unit_artifacts {
        unit_artifacts.push(item?)?;
    }
    let unit_roots = unit_artifacts.finish(inputs.root_reduce_summary)?;

    let streamed = stream_result_chunks(
        binding,
        inputs.root_reduce_summary,
        inputs.result_chunks,
        limits,
    )?;
    let bucket_root =
        stream_bucket_records(inputs.root_reduce_summary, inputs.bucket_records, limits)?;
    if streamed.reduced_summary != *inputs.root_reduce_summary {
        return Err(LysisFinalizationErrorV1::Authority(
            "independently reduced ROOT_REDUCE summary",
        ));
    }
    require_global_nominal_conservation(
        streamed.eligible_nominal_total,
        streamed.tribute_nominal_total,
    )?;

    let frozen = &inputs.intent.frozen_metadosis_values;
    let unused_lysis_limit_minor = frozen
        .lysis_limit_minor
        .checked_sub(streamed.lysis_allocation_minor)
        .ok_or(LysisFinalizationErrorV1::Authority(
            "Lysis allocation within the frozen limit",
        ))?;
    let counts = ExactCountsV1 {
        tribute_count: streamed.tribute_count,
        nod_count: streamed.nod_count,
        bucket_count: streamed.bucket_count,
        contributor_count: streamed.contributor_count,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: streamed.tribute_nominal_total,
        eligible_nominal_total: streamed.eligible_nominal_total,
        day_limit: frozen.day_limit,
        gratis_demand: frozen.gratis_demand,
        day_gratis_limit_minor: frozen.day_gratis_limit_minor,
        lysis_limit_minor: frozen.lysis_limit_minor,
        desis_limit_minor: frozen.desis_limit_minor,
        lysis_allocation_minor: streamed.lysis_allocation_minor,
        unused_lysis_limit_minor,
        carry_over_credit: unused_lysis_limit_minor,
        nod_cost_total: streamed.nod_cost_total,
    };
    let roots = ResultRootsV1 {
        nod_root: streamed.nod_root,
        bucket_root,
        contributor_root: streamed.contributor_root,
        output_manifest_root: streamed.output_manifest_root,
    };
    let carry_over_credit = CarryOverCreditActionV1 {
        source_wwd: inputs.intent.wwd,
        reason: CarryOverReason::UnusedLysis,
        amount: unused_lysis_limit_minor,
    };
    let metadosis_completion_summary = MetadosisCompletionSummaryV1 {
        wwd: inputs.intent.wwd,
        pending_nonce: inputs.intent.pending_nonce,
        day_type: frozen.day_type,
        tribute_nominal_total: streamed.tribute_nominal_total,
        day_limit: frozen.day_limit,
        gratis_demand: frozen.gratis_demand,
        day_gratis_limit_minor: frozen.day_gratis_limit_minor,
        lysis_limit_minor: frozen.lysis_limit_minor,
        desis_limit_minor: frozen.desis_limit_minor,
        lysis_allocation_minor: streamed.lysis_allocation_minor,
        unused_lysis_limit_minor,
        carry_over_credit: unused_lysis_limit_minor,
        status: CompletionStatus::Completed,
        logical_evaluation_height: inputs.intent.logical_evaluation_height,
        logical_evaluation_time: inputs.intent.logical_evaluation_time,
    };
    let mut result = LysisResultV1 {
        protocol_bundle_hash: inputs.plan.protocol_bundle_hash,
        job_id: inputs.finalized_job_id,
        attempt: inputs.plan.attempt,
        input_manifest_hash: inputs.plan.input_manifest_hash,
        plan_hash,
        unit_artifact_root: unit_roots.unit_artifact_root,
        fidelity_fraction_root: unit_roots.fidelity_fraction_root,
        gratis_prefix_root: unit_roots.gratis_prefix_root,
        result_chunk_count: inputs.plan.primary_work_unit_count,
        result_chunk_list_root: streamed.result_chunk_list_root,
        carry_over_credit,
        metadosis_completion_summary,
        tribute_count: streamed.tribute_count,
        tribute_nominal_total: streamed.tribute_nominal_total,
        unused_lysis_limit_minor,
        roots,
        counts,
        conservation,
        arithmetic_commitment: B256::ZERO,
        event_summary_hash: lysis_v1_empty_semantic_event_root()?,
    };
    result.arithmetic_commitment = hash_framed(
        outbe_ocomp_protocol::HashDomain::LysisArithmetic,
        &result.arithmetic_summary().encode_canonical(limits)?,
    )?;
    result.validate_semantics(limits)?;
    result.validate_finalized_intent(inputs.intent)?;
    Ok(result)
}

fn validate_authority<U, C, B>(
    inputs: &VerifiedLysisFinalizationInputsV1<'_, U, C, B>,
    limits: &SchemaLimits,
) -> Result<(), LysisFinalizationErrorV1> {
    inputs.intent.validate_semantics()?;
    let manifest_hash = inputs.input_manifest.manifest_hash(limits)?;
    let plan_hash = inputs.plan.plan_hash(limits)?;
    let (intent, manifest, plan) = (inputs.intent, inputs.input_manifest, inputs.plan);
    let job_differs = inputs.finalized_job_id.is_zero()
        || inputs.finalized_job_id != plan.job_id
        || inputs.finalized_job_id != manifest.job_id;
    let bundle_differs = plan.protocol_bundle_hash != intent.protocol_bundle_hash
        || plan.protocol_bundle_hash != manifest.protocol_bundle_hash
        || plan.attempt != intent.attempt
        || plan.attempt != manifest.attempt;
    let day_differs = plan.wwd != intent.wwd
        || plan.wwd != manifest.wwd
        || plan.lysis_limit_minor != intent.frozen_metadosis_values.lysis_limit_minor
        || plan.logical_evaluation_time != intent.logical_evaluation_time;
    let population_differs = plan.input_manifest_hash != manifest_hash
        || plan.tribute_count != intent.authenticated_day_count
        || plan.tribute_count != manifest.tribute_count
        || manifest.tribute_nominal_total != intent.authenticated_day_nominal;
    let summary = inputs.root_reduce_summary;
    let summary_differs = summary.protocol_bundle_hash != plan.protocol_bundle_hash
        || summary.job_id != inputs.finalized_job_id
        || summary.attempt != plan.attempt
        || summary.plan_hash != plan_hash;
    if [
        job_differs,
        bundle_differs,
        day_differs,
        population_differs,
        summary_differs,
    ]
    .contains(&true)
    {
        return Err(LysisFinalizationErrorV1::Authority(
            "finalized intent, manifest, plan and root summary",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ResultBindingV1<'a> {
    job_id: B256,
    plan: &'a PlanCommitmentV1,
    plan_hash: B256,
}

#[derive(Clone, Copy)]
struct SummaryBindingV1 {
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    plan_hash: B256,
}

struct UnitArtifactRootsV1 {
    unit_artifact_root: B256,
    gratis_prefix_root: B256,
    fidelity_fraction_root: B256,
}

struct UnitArtifactStreamV1<'a> {
    binding: ResultBindingV1<'a>,
    topology: LysisPlanTopologyV1,
    limits: &'a SchemaLimits,
    exact_unit_count: u32,
    final_unit_ordinal: u32,
    unit_artifact_root: StreamingOrderedListRoot,
    gratis_prefix_root: StreamingOrderedListRoot,
    final_fractions: Option<Vec<LeagueFractionV1>>,
    final_summary: Option<RootReduceSummaryV1>,
    next_unit_ordinal: u32,
}

impl<'a> UnitArtifactStreamV1<'a> {
    fn new(
        binding: ResultBindingV1<'a>,
        topology: LysisPlanTopologyV1,
        limits: &'a SchemaLimits,
    ) -> Result<Self, LysisFinalizationErrorV1> {
        let exact_unit_count = topology.total_unit_count();
        let final_unit_ordinal = exact_unit_count
            .checked_sub(1)
            .ok_or(LysisFinalizationErrorV1::Authority("non-empty exact plan"))?;
        let unit_artifact_root = StreamingOrderedListRoot::new(
            ListKind::UnitSpecificationsArtifacts,
            final_unit_ordinal,
        )?;
        let gratis_prefix_root = StreamingOrderedListRoot::new(
            ListKind::LysisGratisLeafPrefixes,
            binding.plan.primary_work_unit_count,
        )?;
        Ok(Self {
            binding,
            topology,
            limits,
            exact_unit_count,
            final_unit_ordinal,
            unit_artifact_root,
            gratis_prefix_root,
            final_fractions: None,
            final_summary: None,
            next_unit_ordinal: 0,
        })
    }

    fn push(&mut self, item: FinalizationUnitArtifactV1) -> Result<(), LysisFinalizationErrorV1> {
        if item.plan_ordinal != self.next_unit_ordinal || item.plan_ordinal >= self.exact_unit_count
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "exact plan artifact order",
            ));
        }
        let expected_position = self.topology.plan_position_at(item.plan_ordinal)?;
        let plan = self.binding.plan;
        let artifact = &item.artifact;
        let binding_differs = artifact.protocol_bundle_hash != plan.protocol_bundle_hash
            || artifact.job_id != self.binding.job_id
            || artifact.attempt != plan.attempt;
        if item.position != expected_position
            || binding_differs
            || artifact.phase != expected_position.phase()
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "plan-bound unit artifact",
            ));
        }
        artifact.validate_semantics(self.limits)?;

        if item.plan_ordinal == self.final_unit_ordinal {
            self.final_summary = Some(require_final_root_output(
                artifact,
                plan.primary_work_unit_count,
                self.limits,
            )?);
        } else {
            self.unit_artifact_root.push(
                artifact.artifact_digest(self.limits)?.as_slice(),
                B256::len_bytes(),
            )?;
        }

        if is_fixed_reduce_root(item.position, self.topology) {
            let output =
                decode_fixed_reduce_output(artifact.phase_payload(self.limits)?, self.limits)?;
            self.final_fractions = Some(output.ordered_fractions);
        }
        if let PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::GratisPrefixDown,
            level: 0,
            index,
        } = item.position
        {
            self.push_gratis_prefix(artifact, index)?;
        }

        self.next_unit_ordinal =
            self.next_unit_ordinal
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "finalization unit cursor",
                })?;
        Ok(())
    }

    fn push_gratis_prefix(
        &mut self,
        artifact: &UnitArtifactV1,
        index: u32,
    ) -> Result<(), LysisFinalizationErrorV1> {
        let GratisPrefixDownOutputV1::Leaf(prefix) =
            decode_gratis_prefix_down_output(artifact.phase_payload(self.limits)?, self.limits)?
        else {
            return Err(LysisFinalizationErrorV1::Authority(
                "GratisPrefixDown leaf payload",
            ));
        };
        if prefix.segment_ordinal != index {
            return Err(LysisFinalizationErrorV1::Authority(
                "GratisPrefixDown segment ordinal",
            ));
        }
        self.gratis_prefix_root.push(
            &encode_gratis_prefix_record(&prefix, self.limits)?,
            self.limits.max_bounded_bytes,
        )?;
        Ok(())
    }

    fn finish(
        self,
        root_reduce_summary: &RootReduceSummaryV1,
    ) -> Result<UnitArtifactRootsV1, LysisFinalizationErrorV1> {
        if self.next_unit_ordinal != self.exact_unit_count {
            return Err(LysisFinalizationErrorV1::Authority(
                "complete exact plan artifacts",
            ));
        }
        let final_summary = self
            .final_summary
            .ok_or(LysisFinalizationErrorV1::Authority(
                "final ROOT_REDUCE artifact",
            ))?;
        if &final_summary != root_reduce_summary {
            return Err(LysisFinalizationErrorV1::Authority(
                "final ROOT_REDUCE summary input",
            ));
        }
        let unit_artifact_root = self.unit_artifact_root.finish()?;
        let gratis_prefix_root = self.gratis_prefix_root.finish()?;
        let fractions = self
            .final_fractions
            .ok_or(LysisFinalizationErrorV1::Authority(
                "final Fidelity fraction table",
            ))?;
        Ok(UnitArtifactRootsV1 {
            unit_artifact_root,
            gratis_prefix_root,
            fidelity_fraction_root: fraction_root(&fractions, self.limits)?,
        })
    }
}

fn is_fixed_reduce_root(position: PlannedUnitPositionV1, topology: LysisPlanTopologyV1) -> bool {
    matches!(
        position,
        PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level,
            index: 0,
        } if level == topology.tree().height()
    )
}

fn require_final_root_output(
    artifact: &UnitArtifactV1,
    primary_count: u32,
    limits: &SchemaLimits,
) -> Result<RootReduceSummaryV1, LysisFinalizationErrorV1> {
    match (
        primary_count,
        decode_root_reduce_output(artifact.phase_payload(limits)?, limits)?,
    ) {
        (
            1,
            RootReduceOutputV1::Leaf {
                summary,
                output_manifest_entry: _,
            },
        ) => Ok(summary),
        (count, RootReduceOutputV1::Node { summary }) if count > 1 => Ok(summary),
        _ => Err(LysisFinalizationErrorV1::Authority(
            "final ROOT_REDUCE LEAF/NODE shape",
        )),
    }
}

fn fraction_root(
    fractions: &[LeagueFractionV1],
    limits: &SchemaLimits,
) -> Result<B256, LysisFinalizationErrorV1> {
    let count = u32::try_from(fractions.len()).map_err(|_| ProtocolError::IntegerOverflow {
        what: "Fidelity fraction count",
    })?;
    let mut root = StreamingOrderedListRoot::new(ListKind::LysisLeagueFractions, count)?;
    let mut previous = None;
    for fraction in fractions {
        if previous.is_some_and(|league| league >= fraction.league) {
            return Err(LysisFinalizationErrorV1::Authority(
                "strict Fidelity fraction league order",
            ));
        }
        previous = Some(fraction.league);
        let mut record = CanonicalWriter::new(limits.codec);
        record.write_u16(fraction.league)?;
        record.write_u256(fraction.fraction)?;
        root.push(&record.into_bytes(), limits.max_bounded_bytes)?;
    }
    root.finish().map_err(Into::into)
}

fn encode_gratis_prefix_record(
    prefix: &GratisLeafPrefixV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, ProtocolError> {
    let mut record = CanonicalWriter::new(limits.codec);
    record.write_u32(prefix.segment_ordinal)?;
    record.write_u256(prefix.incoming_remaining)?;
    record.write_u256(prefix.outgoing_remaining)?;
    record.write_option(prefix.first_error_ordinal.as_ref(), |writer, ordinal| {
        writer.write_u32(*ordinal)
    })?;
    Ok(record.into_bytes())
}

struct StreamedResultV1 {
    nod_root: B256,
    contributor_root: B256,
    output_manifest_root: B256,
    result_chunk_list_root: B256,
    reduced_summary: RootReduceSummaryV1,
    tribute_count: u32,
    nod_count: u32,
    bucket_count: u32,
    contributor_count: u32,
    tribute_nominal_total: U256,
    eligible_nominal_total: U256,
    lysis_allocation_minor: U256,
    nod_cost_total: U256,
}

fn stream_result_chunks<C>(
    binding: ResultBindingV1<'_>,
    final_summary: &RootReduceSummaryV1,
    chunks: C,
    limits: &SchemaLimits,
) -> Result<StreamedResultV1, LysisFinalizationErrorV1>
where
    C: IntoIterator<Item = Result<FinalizationResultChunkV1, LysisFinalizationErrorV1>>,
{
    let mut stream = ResultChunkStreamV1::new(binding, final_summary, limits)?;
    for item in chunks {
        stream.push(item?)?;
    }
    stream.finish()
}

struct ResultChunkStreamV1<'a> {
    binding: ResultBindingV1<'a>,
    limits: &'a SchemaLimits,
    nod_root: StreamingOrderedListRoot,
    contributor_root: StreamingOrderedListRoot,
    output_manifest_root: StreamingOrderedListRoot,
    result_chunk_list_root: StreamingOrderedListRoot,
    summary_frontier: SummaryFrontierV1,
    next_chunk: u32,
    next_nod_ordinal: u32,
    previous_tribute: Option<B256>,
    previous_contributor: Option<(Address, B256)>,
    tribute_count: u32,
    nod_count: u32,
    contributor_count: u32,
    tribute_nominal_total: U256,
    eligible_nominal_total: U256,
    lysis_allocation_minor: U256,
    nod_cost_total: U256,
}

type EncodedRecords = Vec<Vec<u8>>;

struct LeafRecordsV1<'a> {
    chunk_ordinal: u32,
    nod_records: &'a [Vec<u8>],
    bucket_records: &'a [Vec<u8>],
    contributor_records: &'a [Vec<u8>],
    manifest_record: &'a [u8],
    result_chunk_hash: B256,
    eligible_nominal_total: U256,
    chunk: &'a ResultChunkV1,
}

impl<'a> ResultChunkStreamV1<'a> {
    fn new(
        binding: ResultBindingV1<'a>,
        final_summary: &RootReduceSummaryV1,
        limits: &'a SchemaLimits,
    ) -> Result<Self, LysisFinalizationErrorV1> {
        let primary_count = binding.plan.primary_work_unit_count;
        Ok(Self {
            binding,
            limits,
            nod_root: StreamingOrderedListRoot::new(ListKind::NodActions, final_summary.nod_count)?,
            contributor_root: StreamingOrderedListRoot::new(
                ListKind::ContributorActions,
                final_summary.contributor_count,
            )?,
            output_manifest_root: StreamingOrderedListRoot::new(
                ListKind::CompleteOutputManifest,
                primary_count,
            )?,
            result_chunk_list_root: StreamingOrderedListRoot::new(
                ListKind::ResultChunkHashes,
                primary_count,
            )?,
            summary_frontier: SummaryFrontierV1::new(),
            next_chunk: 0,
            next_nod_ordinal: 0,
            previous_tribute: None,
            previous_contributor: None,
            tribute_count: 0,
            nod_count: 0,
            contributor_count: 0,
            tribute_nominal_total: U256::ZERO,
            eligible_nominal_total: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            nod_cost_total: U256::ZERO,
        })
    }

    fn push(&mut self, item: FinalizationResultChunkV1) -> Result<(), LysisFinalizationErrorV1> {
        if item.chunk_ordinal != self.next_chunk
            || self.next_chunk >= self.binding.plan.primary_work_unit_count
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "exact result chunk order",
            ));
        }
        let chunk = ResultChunkV1::decode_canonical(&item.canonical_chunk_bytes, self.limits)?;
        self.require_chunk_descriptor(&item, &chunk)?;
        self.require_primary_coverage(&chunk)?;
        let (nod_records, bucket_record_bytes) = self.stream_nod_actions(&chunk)?;
        let (contributor_records, chunk_eligible) = self.stream_contributors(&chunk)?;
        self.eligible_nominal_total = checked_add(
            self.eligible_nominal_total,
            chunk_eligible,
            "global eligible nominal total",
        )?;

        let manifest_record = item
            .output_manifest_entry
            .encode_canonical_record(self.limits)?;
        self.output_manifest_root
            .push(&manifest_record, self.limits.max_bounded_bytes)?;
        self.result_chunk_list_root.push(
            item.output_manifest_entry.result_chunk_hash.as_slice(),
            B256::len_bytes(),
        )?;
        require_leaf_summary(
            &item.summary,
            self.binding,
            &LeafRecordsV1 {
                chunk_ordinal: self.next_chunk,
                nod_records: &nod_records,
                bucket_records: &bucket_record_bytes,
                contributor_records: &contributor_records,
                manifest_record: &manifest_record,
                result_chunk_hash: item.output_manifest_entry.result_chunk_hash,
                eligible_nominal_total: chunk_eligible,
                chunk: &chunk,
            },
            self.limits,
        )?;
        let chunk_tribute_nominal_total = item.summary.tribute_nominal_total;
        self.summary_frontier.push(item.summary)?;
        self.advance(&chunk, chunk_tribute_nominal_total)
    }

    fn require_chunk_descriptor(
        &self,
        item: &FinalizationResultChunkV1,
        chunk: &ResultChunkV1,
    ) -> Result<(), LysisFinalizationErrorV1> {
        let plan = self.binding.plan;
        let entry = &item.output_manifest_entry;
        let binding_differs = chunk.protocol_bundle_hash != plan.protocol_bundle_hash
            || chunk.job_id != self.binding.job_id
            || chunk.attempt != plan.attempt;
        let order_differs = chunk.chunk_ordinal != self.next_chunk
            || chunk.first_nod_ordinal != self.next_nod_ordinal
            || entry.chunk_ordinal != self.next_chunk;
        if binding_differs
            || order_differs
            || entry.result_chunk_ref.expected_ocb1_kind != Some(ObjectKind::ResultChunkV1.tag())
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "exact result chunk descriptor",
            ));
        }
        let encoded_bytes = u64::try_from(item.canonical_chunk_bytes.len()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "canonical ResultChunkV1 bytes",
            }
        })?;
        if entry.result_chunk_ref.encoded_bytes != encoded_bytes
            || entry.result_chunk_ref.transport_digest != keccak256(&item.canonical_chunk_bytes)
            || entry.result_chunk_hash != chunk.result_chunk_hash(self.limits)?
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "exact result chunk descriptor",
            ));
        }
        Ok(())
    }

    fn require_primary_coverage(
        &self,
        chunk: &ResultChunkV1,
    ) -> Result<(), LysisFinalizationErrorV1> {
        let plan = self.binding.plan;
        let expected_start = self
            .next_chunk
            .checked_mul(plan.max_tributes_per_work_shard)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "result chunk start ordinal",
            })?;
        let expected_end = expected_start
            .saturating_add(plan.max_tributes_per_work_shard)
            .min(plan.tribute_count);
        let expected_nod_count =
            expected_end
                .checked_sub(expected_start)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "result chunk exact Nod count",
                })?;
        if chunk.first_nod_ordinal != expected_start
            || u32::try_from(chunk.ordered_nod_actions.len()).ok() != Some(expected_nod_count)
        {
            return Err(LysisFinalizationErrorV1::Authority(
                "result chunk primary shard coverage",
            ));
        }
        Ok(())
    }

    fn stream_nod_actions(
        &mut self,
        chunk: &ResultChunkV1,
    ) -> Result<(EncodedRecords, EncodedRecords), LysisFinalizationErrorV1> {
        let mut nod_records = Vec::with_capacity(chunk.ordered_nod_actions.len());
        let mut bucket_records = Vec::with_capacity(chunk.ordered_nod_actions.len());
        for action in &chunk.ordered_nod_actions {
            if self
                .previous_tribute
                .is_some_and(|tribute| tribute >= action.tribute_id)
            {
                return Err(LysisFinalizationErrorV1::Authority(
                    "global Nod Tribute order",
                ));
            }
            self.previous_tribute = Some(action.tribute_id);
            if !outbe_nod::pricing::is_issuable_entry(action.entry_price_minor) {
                return Err(LysisFinalizationErrorV1::Authority("Nod entry price bound"));
            }
            let record = action.encode_canonical_record(self.limits)?;
            self.nod_root.push(&record, self.limits.max_bounded_bytes)?;
            nod_records.push(record);
            bucket_records.push(ShuffleBucketRecordV1 {
                bucket_key: action.bucket_key(),
                raw_ordinal: action.raw_ordinal,
                tribute_id: action.tribute_id,
                nod_id: action.nod_id,
            });
            self.lysis_allocation_minor = checked_add(
                self.lysis_allocation_minor,
                action.gratis_load_minor,
                "Lysis allocation",
            )?;
            self.nod_cost_total = checked_add(
                self.nod_cost_total,
                action.settlement_cost_minor,
                "Nod cost total",
            )?;
        }
        bucket_records.sort_by_key(|record| (record.bucket_key, record.raw_ordinal));
        let bucket_record_bytes = bucket_records
            .iter()
            .map(|record| record.encode_canonical_record(self.limits))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((nod_records, bucket_record_bytes))
    }

    fn stream_contributors(
        &mut self,
        chunk: &ResultChunkV1,
    ) -> Result<(EncodedRecords, U256), LysisFinalizationErrorV1> {
        let mut contributor_records = Vec::with_capacity(chunk.ordered_eligible_contributors.len());
        let mut chunk_eligible = U256::ZERO;
        for action in &chunk.ordered_eligible_contributors {
            let key = (action.owner, action.source_tribute_id);
            if self
                .previous_contributor
                .is_some_and(|previous| previous >= key)
            {
                return Err(LysisFinalizationErrorV1::Authority(
                    "global contributor order",
                ));
            }
            self.previous_contributor = Some(key);
            let record = action.encode_canonical_record(self.limits)?;
            self.contributor_root
                .push(&record, self.limits.max_bounded_bytes)?;
            contributor_records.push(record);
            chunk_eligible = checked_add(
                chunk_eligible,
                action.nominal_amount_minor,
                "eligible nominal total",
            )?;
        }
        Ok((contributor_records, chunk_eligible))
    }

    fn advance(
        &mut self,
        chunk: &ResultChunkV1,
        chunk_tribute_nominal_total: U256,
    ) -> Result<(), LysisFinalizationErrorV1> {
        let chunk_nod_count = u32::try_from(chunk.ordered_nod_actions.len()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "chunk Nod count",
            }
        })?;
        let chunk_contributor_count = u32::try_from(chunk.ordered_eligible_contributors.len())
            .map_err(|_| ProtocolError::IntegerOverflow {
                what: "chunk contributor count",
            })?;
        self.tribute_count = self.tribute_count.checked_add(chunk_nod_count).ok_or(
            ProtocolError::IntegerOverflow {
                what: "Tribute count",
            },
        )?;
        self.nod_count = self
            .nod_count
            .checked_add(chunk_nod_count)
            .ok_or(ProtocolError::IntegerOverflow { what: "Nod count" })?;
        self.contributor_count = self
            .contributor_count
            .checked_add(chunk_contributor_count)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "contributor count",
            })?;
        self.tribute_nominal_total = checked_add(
            self.tribute_nominal_total,
            chunk_tribute_nominal_total,
            "Tribute nominal total",
        )?;
        self.next_nod_ordinal = self.next_nod_ordinal.checked_add(chunk_nod_count).ok_or(
            ProtocolError::IntegerOverflow {
                what: "next Nod ordinal",
            },
        )?;
        self.next_chunk = self
            .next_chunk
            .checked_add(1)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "next result chunk",
            })?;
        Ok(())
    }

    fn finish(self) -> Result<StreamedResultV1, LysisFinalizationErrorV1> {
        let plan = self.binding.plan;
        if self.next_chunk != plan.primary_work_unit_count {
            return Err(LysisFinalizationErrorV1::Authority(
                "complete result chunk catalog",
            ));
        }
        let reduced_summary = self.summary_frontier.finish(
            plan.primary_work_unit_count,
            SummaryBindingV1 {
                protocol_bundle_hash: plan.protocol_bundle_hash,
                job_id: self.binding.job_id,
                attempt: plan.attempt,
                plan_hash: self.binding.plan_hash,
            },
        )?;
        Ok(StreamedResultV1 {
            nod_root: self.nod_root.finish()?,
            contributor_root: self.contributor_root.finish()?,
            output_manifest_root: self.output_manifest_root.finish()?,
            result_chunk_list_root: self.result_chunk_list_root.finish()?,
            reduced_summary,
            tribute_count: self.tribute_count,
            nod_count: self.nod_count,
            bucket_count: self.nod_count,
            contributor_count: self.contributor_count,
            tribute_nominal_total: self.tribute_nominal_total,
            eligible_nominal_total: self.eligible_nominal_total,
            lysis_allocation_minor: self.lysis_allocation_minor,
            nod_cost_total: self.nod_cost_total,
        })
    }
}

fn require_leaf_summary(
    summary: &RootReduceSummaryV1,
    binding: ResultBindingV1<'_>,
    leaf: &LeafRecordsV1<'_>,
    limits: &SchemaLimits,
) -> Result<(), LysisFinalizationErrorV1> {
    let expected_nod = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::NodActions,
        leaf.chunk_ordinal,
        leaf.nod_records,
        limits.max_bounded_bytes,
    )?;
    let expected_bucket = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::BucketRecords,
        leaf.chunk_ordinal,
        leaf.bucket_records,
        limits.max_bounded_bytes,
    )?;
    let expected_contributor = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::ContributorActions,
        leaf.chunk_ordinal,
        leaf.contributor_records,
        limits.max_bounded_bytes,
    )?;
    let expected_manifest = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::CompleteOutputManifest,
        leaf.chunk_ordinal,
        &[leaf.manifest_record],
        limits.max_bounded_bytes,
    )?;
    let expected_result_hash = LysisListSubtreeCarrierV1::from_primary_page(
        ListKind::ResultChunkHashes,
        leaf.chunk_ordinal,
        &[leaf.result_chunk_hash.as_slice()],
        B256::len_bytes(),
    )?;
    let lysis_allocation_minor = leaf
        .chunk
        .ordered_nod_actions
        .iter()
        .try_fold(U256::ZERO, |total, action| {
            checked_add(total, action.gratis_load_minor, "leaf Nod Gratis")
        })?;
    let nod_cost_total = leaf
        .chunk
        .ordered_nod_actions
        .iter()
        .try_fold(U256::ZERO, |total, action| {
            checked_add(total, action.settlement_cost_minor, "leaf Nod cost")
        })?;
    let nod_count = u32::try_from(leaf.nod_records.len()).unwrap_or(u32::MAX);
    let expected = RootReduceSummaryV1 {
        protocol_bundle_hash: binding.plan.protocol_bundle_hash,
        job_id: binding.plan.job_id,
        attempt: binding.plan.attempt,
        plan_hash: binding.plan_hash,
        covered_primary_start: leaf.chunk_ordinal,
        covered_primary_count: 1,
        nod_actions: expected_nod,
        bucket_records: expected_bucket,
        contributor_actions: expected_contributor,
        output_manifest_entries: expected_manifest,
        result_chunk_hashes: expected_result_hash,
        tribute_count: nod_count,
        nod_count,
        bucket_count: u32::try_from(leaf.bucket_records.len()).unwrap_or(u32::MAX),
        contributor_count: u32::try_from(leaf.contributor_records.len()).unwrap_or(u32::MAX),
        tribute_nominal_total: summary.tribute_nominal_total,
        eligible_nominal_total: leaf.eligible_nominal_total,
        lysis_allocation_minor,
        nod_cost_total,
        first_error_ordinal: None,
    };
    if *summary != expected {
        return Err(LysisFinalizationErrorV1::Authority(
            "ROOT_REDUCE leaf summary from exact chunk",
        ));
    }
    Ok(())
}

fn stream_bucket_records<B>(
    final_summary: &RootReduceSummaryV1,
    records: B,
    limits: &SchemaLimits,
) -> Result<B256, LysisFinalizationErrorV1>
where
    B: IntoIterator<Item = Result<ShuffleBucketRecordV1, LysisFinalizationErrorV1>>,
{
    let mut root =
        StreamingOrderedListRoot::new(ListKind::BucketRecords, final_summary.bucket_count)?;
    let mut previous = None;
    let mut count = 0_u32;
    for record in records {
        let record = record?;
        let key = (record.bucket_key, record.raw_ordinal);
        if previous.is_some_and(|previous| previous >= key) {
            return Err(LysisFinalizationErrorV1::Authority(
                "global Bucket record order",
            ));
        }
        previous = Some(key);
        root.push(
            &record.encode_canonical_record(limits)?,
            limits.max_bounded_bytes,
        )?;
        count = count.checked_add(1).ok_or(ProtocolError::IntegerOverflow {
            what: "Bucket record count",
        })?;
    }
    if count != final_summary.bucket_count {
        return Err(LysisFinalizationErrorV1::Authority(
            "complete Bucket record catalog",
        ));
    }
    root.finish().map_err(Into::into)
}

struct SummaryFrontierV1 {
    levels: [Option<RootReduceSummaryV1>; 33],
}

impl SummaryFrontierV1 {
    fn new() -> Self {
        Self {
            levels: std::array::from_fn(|_| None),
        }
    }

    fn push(&mut self, mut summary: RootReduceSummaryV1) -> Result<(), LysisFinalizationErrorV1> {
        let mut level = usize::from(summary.result_chunk_hashes.subtree_height);
        let mut index = summary.result_chunk_hashes.subtree_index;
        loop {
            if index & 1 == 0 {
                if self.levels[level].replace(summary).is_some() {
                    return Err(LysisFinalizationErrorV1::Authority(
                        "summary frontier duplicate left node",
                    ));
                }
                return Ok(());
            }
            let left = self.levels[level]
                .take()
                .ok_or(LysisFinalizationErrorV1::Authority(
                    "summary frontier missing left node",
                ))?;
            summary = left.merge_adjacent(summary)?;
            index >>= 1;
            level = level
                .checked_add(1)
                .filter(|next| *next < self.levels.len())
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "summary frontier level",
                })?;
        }
    }

    fn finish(
        mut self,
        primary_count: u32,
        binding: SummaryBindingV1,
    ) -> Result<RootReduceSummaryV1, LysisFinalizationErrorV1> {
        let padded_count = if primary_count == 1 {
            1
        } else {
            primary_count
                .checked_next_power_of_two()
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "ROOT_REDUCE padded primary count",
                })?
        };
        for padded_ordinal in primary_count..padded_count {
            self.push(canonical_empty_summary(binding, padded_ordinal)?)?;
        }
        let root_level = usize::try_from(padded_count.trailing_zeros()).map_err(|_| {
            ProtocolError::IntegerOverflow {
                what: "ROOT_REDUCE root level",
            }
        })?;
        let root = self.levels[root_level]
            .take()
            .ok_or(LysisFinalizationErrorV1::Authority(
                "complete ROOT_REDUCE summary frontier",
            ))?;
        if self.levels.into_iter().any(|entry| entry.is_some()) {
            return Err(LysisFinalizationErrorV1::Authority(
                "single ROOT_REDUCE summary frontier root",
            ));
        }
        Ok(root)
    }
}

fn canonical_empty_summary(
    binding: SummaryBindingV1,
    padded_ordinal: u32,
) -> Result<RootReduceSummaryV1, LysisFinalizationErrorV1> {
    Ok(RootReduceSummaryV1 {
        protocol_bundle_hash: binding.protocol_bundle_hash,
        job_id: binding.job_id,
        attempt: binding.attempt,
        plan_hash: binding.plan_hash,
        covered_primary_start: padded_ordinal,
        covered_primary_count: 0,
        nod_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::NodActions,
            padded_ordinal,
        )?,
        bucket_records: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::BucketRecords,
            padded_ordinal,
        )?,
        contributor_actions: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ContributorActions,
            padded_ordinal,
        )?,
        output_manifest_entries: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::CompleteOutputManifest,
            padded_ordinal,
        )?,
        result_chunk_hashes: LysisListSubtreeCarrierV1::canonical_empty_primary_page(
            ListKind::ResultChunkHashes,
            padded_ordinal,
        )?,
        tribute_count: 0,
        nod_count: 0,
        bucket_count: 0,
        contributor_count: 0,
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        nod_cost_total: U256::ZERO,
        first_error_ordinal: None,
    })
}

fn checked_add(
    left: U256,
    right: U256,
    what: &'static str,
) -> Result<U256, LysisFinalizationErrorV1> {
    left.checked_add(right)
        .ok_or(ProtocolError::IntegerOverflow { what }.into())
}

fn require_global_nominal_conservation(
    eligible_nominal_total: U256,
    tribute_nominal_total: U256,
) -> Result<(), LysisFinalizationErrorV1> {
    if eligible_nominal_total > tribute_nominal_total {
        return Err(LysisFinalizationErrorV1::Authority(
            "global eligible nominal within Tribute nominal",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
