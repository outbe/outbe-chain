use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    input::{InputChunkKind, InputChunkRefV1},
    registry::ListKind,
    unit::{
        CanonicalInputRefV1, EntityIdHalfOpenRange, FidelityIndexHalfOpenRange, InputPurpose,
        InputSourceKind, PlanCommitmentV1, UnitInterval, UnitPhase, UnitSpecV1,
    },
    ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};

use super::{
    LysisPlannerBindingsV1, LysisPlannerV1, PaddedBinaryTreeV1, PlannerErrorV1, PrimaryShardV1,
    PRIMARY_WORK_SHARD_SIZE,
};

fn chunk_first_id(chunk: &InputChunkRefV1, shard_ordinal: u32) -> Result<B256, PlannerErrorV1> {
    let bytes: [u8; 32] = chunk.first_key.0.as_slice().try_into().map_err(|_| {
        PlannerErrorV1::InvalidTributeChunk {
            ordinal: shard_ordinal,
        }
    })?;
    Ok(B256::from(bytes))
}

fn has_zero_binding(bindings: &LysisPlannerBindingsV1) -> bool {
    let zero_hash = [
        bindings.protocol_bundle_hash,
        bindings.job_id,
        bindings.input_manifest_hash,
        bindings.fidelity_opening_root,
        bindings.oracle_opening_root,
        bindings.lysis_program_semantics_hash,
    ]
    .iter()
    .any(B256::is_zero);
    let zero_scalar = bindings.wwd == 0
        || bindings.lysis_limit_minor.is_zero()
        || bindings.logical_evaluation_time == 0
        || bindings.input_manifest_encoded_bytes == 0;
    let zero_version = bindings.planner_spec_version == 0 || bindings.reducer_spec_version == 0;
    zero_hash || zero_scalar || zero_version
}

impl LysisPlannerV1 {
    pub fn new(bindings: LysisPlannerBindingsV1) -> Result<Self, PlannerErrorV1> {
        if bindings.tribute_count == 0 {
            return Err(PlannerErrorV1::EmptyTributePopulation);
        }
        if has_zero_binding(&bindings) {
            return Err(ProtocolError::InvalidInvariant("Lysis planner frozen bindings").into());
        }
        Ok(Self {
            primary_tree: PaddedBinaryTreeV1::for_tribute_count(bindings.tribute_count)?,
            bindings,
        })
    }

    #[must_use]
    pub const fn primary_work_unit_count(self) -> u32 {
        self.primary_tree.primary_leaf_count
    }

    pub fn primary_unit_at<F>(
        self,
        shard_ordinal: u32,
        mut tribute_chunk_at: F,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1>
    where
        F: FnMut(u32) -> Option<InputChunkRefV1>,
    {
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        let tribute_chunk =
            tribute_chunk_at(shard_ordinal).ok_or(PlannerErrorV1::MissingTributeChunk {
                ordinal: shard_ordinal,
            })?;
        let start = chunk_first_id(&tribute_chunk, shard_ordinal)?;
        let end = if shard.end_ordinal < self.bindings.tribute_count {
            let next_ordinal = shard_ordinal
                .checked_add(1)
                .ok_or(PlannerErrorV1::IntegerOverflow)?;
            Some(chunk_first_id(
                &tribute_chunk_at(next_ordinal).ok_or(PlannerErrorV1::MissingTributeChunk {
                    ordinal: next_ordinal,
                })?,
                next_ordinal,
            )?)
        } else {
            None
        };
        self.primary_unit_for_range(shard, start, end, &tribute_chunk, limits)
    }

    pub fn commit_primary_catalog<I>(
        self,
        tribute_chunks: I,
        limits: &SchemaLimits,
    ) -> Result<PlanCommitmentV1, PlannerErrorV1>
    where
        I: IntoIterator<Item = InputChunkRefV1>,
    {
        let mut chunks = tribute_chunks.into_iter().peekable();
        let mut root = StreamingOrderedListRoot::new(
            ListKind::UnitSpecificationsArtifacts,
            self.primary_work_unit_count(),
        )?;

        for shard_ordinal in 0..self.primary_work_unit_count() {
            let tribute_chunk = chunks.next().ok_or(PlannerErrorV1::MissingTributeChunk {
                ordinal: shard_ordinal,
            })?;
            let shard = self.primary_tree.primary_shard(shard_ordinal)?;
            let start = chunk_first_id(&tribute_chunk, shard_ordinal)?;
            let end = chunks
                .peek()
                .map(|next| chunk_first_id(next, shard_ordinal + 1))
                .transpose()?;
            let spec = self.primary_unit_for_range(shard, start, end, &tribute_chunk, limits)?;
            root.push(&spec.encode_canonical(limits)?, limits.codec.max_body_bytes)?;
        }
        if chunks.next().is_some() {
            return Err(PlannerErrorV1::UnexpectedTributeChunk {
                ordinal: self.primary_work_unit_count(),
            });
        }
        let primary_work_unit_root = root.finish()?;
        let plan = PlanCommitmentV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            input_manifest_hash: self.bindings.input_manifest_hash,
            wwd: self.bindings.wwd,
            lysis_limit_minor: self.bindings.lysis_limit_minor,
            logical_evaluation_time: self.bindings.logical_evaluation_time,
            tribute_count: self.bindings.tribute_count,
            max_tributes_per_work_shard: PRIMARY_WORK_SHARD_SIZE,
            primary_work_unit_count: self.primary_work_unit_count(),
            primary_work_unit_root,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        plan.validate_semantics()?;
        Ok(plan)
    }

    pub fn fidelity_map_unit_at(
        self,
        shard_ordinal: u32,
        enumerate_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        if enumerate_unit_id.is_zero() {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
        let max_fidelity_opening_bytes = candidate
            .max_fidelity_openings_per_work_shard
            .checked_mul(candidate.max_opening_bytes)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let fidelity_opening_count_limit =
            u32::try_from(candidate.max_fidelity_openings_per_work_shard)
                .map_err(|_| PlannerErrorV1::IntegerOverflow)?;
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::FidelityMap,
            interval: UnitInterval::FidelityIndexRange(FidelityIndexHalfOpenRange {
                start: shard.start_ordinal,
                end: shard.end_ordinal,
            }),
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::EnumeratedTributes,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: enumerate_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::FidelityOpenings,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.fidelity_opening_root,
                    record_count_limit: fidelity_opening_count_limit,
                    max_encoded_bytes: max_fidelity_opening_bytes,
                    max_decoded_bytes: max_fidelity_opening_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    pub fn amount_map_unit_at(
        self,
        shard_ordinal: u32,
        enumerate_spec: &UnitSpecV1,
        fidelity_unit_id: B256,
        fraction_root_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        if fidelity_unit_id.is_zero()
            || fraction_root_unit_id.is_zero()
            || !self.binds_entity_range_spec(enumerate_spec, UnitPhase::Enumerate)
        {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let enumerate_unit_id = enumerate_spec.unit_id(limits)?;
        let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::AmountMap,
            interval: enumerate_spec.interval.clone(),
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::EnumeratedTributes,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: enumerate_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::FidelityPartials,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: fidelity_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::FiFractionTable,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: fraction_root_unit_id,
                    record_count_limit: self.bindings.tribute_count,
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::OracleOpenings,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.oracle_opening_root,
                    record_count_limit: 1,
                    max_encoded_bytes: candidate.max_opening_bytes,
                    max_decoded_bytes: candidate.max_opening_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    pub fn output_finalize_unit_at(
        self,
        shard_ordinal: u32,
        amount_spec: &UnitSpecV1,
        prefix_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        if prefix_unit_id.is_zero()
            || !self.binds_entity_range_spec(amount_spec, UnitPhase::AmountMap)
        {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        self.output_finalize_unit_from_producers(
            shard_ordinal,
            amount_spec.interval.clone(),
            amount_spec.unit_id(limits)?,
            prefix_unit_id,
            limits,
        )
    }

    pub fn validate_output_finalize_unit(
        self,
        shard_ordinal: u32,
        spec: &UnitSpecV1,
        amount_unit_id: B256,
        prefix_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<(), PlannerErrorV1> {
        if spec.phase != UnitPhase::OutputFinalize
            || !matches!(spec.interval, UnitInterval::EntityIdRange(_))
        {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let expected = self.output_finalize_unit_from_producers(
            shard_ordinal,
            spec.interval.clone(),
            amount_unit_id,
            prefix_unit_id,
            limits,
        )?;
        if expected == *spec {
            Ok(())
        } else {
            Err(PlannerErrorV1::ProducerMembershipMismatch)
        }
    }

    fn output_finalize_unit_from_producers(
        self,
        shard_ordinal: u32,
        interval: UnitInterval,
        amount_unit_id: B256,
        prefix_unit_id: B256,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        let candidate = OCOMP_POC_CANDIDATE_LIMITS_V1;
        let shard = self.primary_tree.primary_shard(shard_ordinal)?;
        if amount_unit_id.is_zero()
            || prefix_unit_id.is_zero()
            || !matches!(interval, UnitInterval::EntityIdRange(_))
        {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::OutputFinalize,
            interval,
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::AmountRecords,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: amount_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::GratisPrefixTable,
                    source_kind: InputSourceKind::UnitOutput,
                    source_id: prefix_unit_id,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: candidate.max_activation_ocb1_bytes,
                    max_decoded_bytes: candidate.max_activation_ocb1_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    fn binds_entity_range_spec(self, spec: &UnitSpecV1, phase: UnitPhase) -> bool {
        let bindings = self.bindings;
        let bound = spec.protocol_bundle_hash == bindings.protocol_bundle_hash
            && spec.job_id == bindings.job_id
            && spec.attempt == bindings.attempt
            && spec.phase == phase;
        let versioned = spec.lysis_program_semantics_hash == bindings.lysis_program_semantics_hash
            && spec.planner_spec_version == bindings.planner_spec_version
            && spec.reducer_spec_version == bindings.reducer_spec_version;
        bound && versioned && matches!(spec.interval, UnitInterval::EntityIdRange(_))
    }

    fn primary_unit_for_range(
        self,
        shard: PrimaryShardV1,
        start: B256,
        end: Option<B256>,
        tribute_chunk: &InputChunkRefV1,
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        tribute_chunk.encode_canonical_record(limits)?;
        let shard_differs = tribute_chunk.kind != InputChunkKind::Tribute
            || tribute_chunk.ordinal != shard.ordinal
            || tribute_chunk.record_count != shard.record_count();
        let range_differs = tribute_chunk.first_key.0.as_slice() != start.as_slice()
            || tribute_chunk.last_key_inclusive.0.len() != 32
            || tribute_chunk.last_key_inclusive.0.as_slice() < start.0.as_slice()
            || end.is_some_and(|end| {
                tribute_chunk.last_key_inclusive.0.as_slice() >= end.0.as_slice()
            });
        if shard_differs || range_differs {
            return Err(PlannerErrorV1::InvalidTributeChunk {
                ordinal: shard.ordinal,
            });
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::Enumerate,
            interval: UnitInterval::EntityIdRange(EntityIdHalfOpenRange { start, end }),
            canonical_ordered_inputs: vec![
                CanonicalInputRefV1 {
                    purpose: InputPurpose::InputManifest,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: self.bindings.input_manifest_hash,
                    record_count_limit: 1,
                    max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
                    max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
                },
                CanonicalInputRefV1 {
                    purpose: InputPurpose::TributeStream,
                    source_kind: InputSourceKind::AuthenticatedRoot,
                    source_id: tribute_chunk.semantic_digest,
                    record_count_limit: shard.record_count(),
                    max_encoded_bytes: tribute_chunk.encoded_bytes,
                    max_decoded_bytes: tribute_chunk.encoded_bytes,
                },
            ],
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }
}
