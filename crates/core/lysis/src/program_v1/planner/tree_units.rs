use alloy_primitives::B256;
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    unit::{
        empty_unit_input_id, BinaryReducerNode, CanonicalInputRefV1, CanonicalRunSpan,
        InputPurpose, InputSourceKind, UnitInterval, UnitPhase, UnitSpecV1,
    },
    SchemaLimits,
};

use super::{
    LysisPlanTopologyV1, LysisPlannerV1, PlannedProducerV1, PlannedUnitPositionV1, PlannerErrorV1,
    PRIMARY_WORK_SHARD_SIZE,
};

impl LysisPlannerV1 {
    pub fn fixed_reduce_unit_at(
        self,
        phase_ordinal: u32,
        producer_unit_ids: [Option<B256>; 2],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        let topology = LysisPlanTopologyV1::new(self.primary_work_unit_count())?;
        let position = topology.phase_position_at(UnitPhase::FixedReduce, phase_ordinal)?;
        let PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::FixedReduce,
            level,
            index,
        } = position
        else {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        };
        let producers = topology.required_producers(position)?;
        let mut canonical_ordered_inputs = vec![CanonicalInputRefV1 {
            purpose: InputPurpose::InputManifest,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: self.bindings.input_manifest_hash,
            record_count_limit: 1,
            max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
            max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
        }];
        for (producer, unit_id) in producers.into_iter().zip(producer_unit_ids) {
            let input = match (producer, unit_id) {
                (PlannedProducerV1::Unit(_), Some(unit_id)) if !unit_id.is_zero() => {
                    CanonicalInputRefV1 {
                        purpose: InputPurpose::FidelityPartials,
                        source_kind: InputSourceKind::UnitOutput,
                        source_id: unit_id,
                        record_count_limit: self.bindings.tribute_count,
                        max_encoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
                        max_decoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
                    }
                }
                (
                    PlannedProducerV1::CanonicalEmpty {
                        purpose: InputPurpose::FidelityPartials,
                        ..
                    },
                    None,
                ) => CanonicalInputRefV1 {
                    purpose: InputPurpose::FidelityPartials,
                    source_kind: InputSourceKind::CanonicalEmpty,
                    source_id: empty_unit_input_id(InputPurpose::FidelityPartials)?,
                    record_count_limit: 0,
                    max_encoded_bytes: 0,
                    max_decoded_bytes: 0,
                },
                _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
            };
            canonical_ordered_inputs.push(input);
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::FixedReduce,
            interval: UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
            canonical_ordered_inputs,
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    pub fn gratis_prefix_unit_at(
        self,
        phase_ordinal: u32,
        producer_unit_ids: &[Option<B256>],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        self.gratis_scan_unit_at(
            UnitPhase::GratisPrefix,
            phase_ordinal,
            producer_unit_ids,
            limits,
        )
    }

    pub fn gratis_prefix_down_unit_at(
        self,
        phase_ordinal: u32,
        producer_unit_ids: &[Option<B256>],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        self.gratis_scan_unit_at(
            UnitPhase::GratisPrefixDown,
            phase_ordinal,
            producer_unit_ids,
            limits,
        )
    }

    pub fn shuffle_unit_at(
        self,
        phase: UnitPhase,
        phase_ordinal: u32,
        producer_unit_ids: &[B256],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        if !matches!(phase, UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle) {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let topology = LysisPlanTopologyV1::new(self.primary_work_unit_count())?;
        let position = topology.phase_position_at(phase, phase_ordinal)?;
        let PlannedUnitPositionV1::RunSpan {
            level,
            start_run,
            end_run,
            ..
        } = position
        else {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        };
        let producers = topology.required_producers(position)?;
        if producers.len() != producer_unit_ids.len() || producer_unit_ids.iter().any(B256::is_zero)
        {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let purpose = if level == 0 {
            InputPurpose::FinalizedOutputRecords
        } else if phase == UnitPhase::OwnerShuffle {
            InputPurpose::OwnerOrderedRecords
        } else {
            InputPurpose::BucketOrderedRecords
        };
        let mut canonical_ordered_inputs = vec![CanonicalInputRefV1 {
            purpose: InputPurpose::InputManifest,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: self.bindings.input_manifest_hash,
            record_count_limit: 1,
            max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
            max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
        }];
        for (producer, unit_id) in producers.into_iter().zip(producer_unit_ids) {
            let PlannedProducerV1::Unit(producer) = producer else {
                return Err(PlannerErrorV1::ProducerMembershipMismatch);
            };
            canonical_ordered_inputs.push(CanonicalInputRefV1 {
                purpose,
                source_kind: InputSourceKind::UnitOutput,
                source_id: *unit_id,
                record_count_limit: self.shuffle_position_record_limit(producer)?,
                max_encoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
                max_decoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
            });
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase,
            interval: UnitInterval::CanonicalRunSpan(CanonicalRunSpan { start_run, end_run }),
            canonical_ordered_inputs,
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    pub fn root_reduce_unit_at(
        self,
        phase_ordinal: u32,
        producer_unit_ids: &[Option<B256>],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        let topology = LysisPlanTopologyV1::new(self.primary_work_unit_count())?;
        let position = topology.phase_position_at(UnitPhase::RootReduce, phase_ordinal)?;
        let PlannedUnitPositionV1::TreeNode {
            phase: UnitPhase::RootReduce,
            level,
            index,
        } = position
        else {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        };
        let producers = topology.required_producers(position)?;
        if producers.len() != producer_unit_ids.len() {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }

        let mut canonical_ordered_inputs = vec![CanonicalInputRefV1 {
            purpose: InputPurpose::InputManifest,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: self.bindings.input_manifest_hash,
            record_count_limit: 1,
            max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
            max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
        }];
        for (producer, unit_id) in producers.into_iter().zip(producer_unit_ids.iter().copied()) {
            let input = match (producer, unit_id) {
                (PlannedProducerV1::Unit(position), Some(unit_id)) if !unit_id.is_zero() => {
                    self.root_reduce_unit_input(position, unit_id)?
                }
                (
                    PlannedProducerV1::CanonicalEmpty {
                        purpose: InputPurpose::RootSummary,
                        ..
                    },
                    None,
                ) => CanonicalInputRefV1 {
                    purpose: InputPurpose::RootSummary,
                    source_kind: InputSourceKind::CanonicalEmpty,
                    source_id: empty_unit_input_id(InputPurpose::RootSummary)?,
                    record_count_limit: 0,
                    max_encoded_bytes: 0,
                    max_decoded_bytes: 0,
                },
                _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
            };
            canonical_ordered_inputs.push(input);
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase: UnitPhase::RootReduce,
            interval: UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
            canonical_ordered_inputs,
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    fn gratis_scan_unit_at(
        self,
        phase: UnitPhase,
        phase_ordinal: u32,
        producer_unit_ids: &[Option<B256>],
        limits: &SchemaLimits,
    ) -> Result<UnitSpecV1, PlannerErrorV1> {
        let topology = LysisPlanTopologyV1::new(self.primary_work_unit_count())?;
        let position = topology.phase_position_at(phase, phase_ordinal)?;
        let PlannedUnitPositionV1::TreeNode {
            phase: position_phase,
            level,
            index,
        } = position
        else {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        };
        if position_phase != phase {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let producers = topology.required_producers(position)?;
        if producers.len() != producer_unit_ids.len() {
            return Err(PlannerErrorV1::ProducerMembershipMismatch);
        }
        let unit_purpose = if phase == UnitPhase::GratisPrefix && level == 0 {
            InputPurpose::AmountRecords
        } else {
            InputPurpose::GratisPrefixTable
        };
        let mut canonical_ordered_inputs = vec![CanonicalInputRefV1 {
            purpose: InputPurpose::InputManifest,
            source_kind: InputSourceKind::AuthenticatedRoot,
            source_id: self.bindings.input_manifest_hash,
            record_count_limit: 1,
            max_encoded_bytes: self.bindings.input_manifest_encoded_bytes,
            max_decoded_bytes: self.bindings.input_manifest_encoded_bytes,
        }];
        for (producer, unit_id) in producers.into_iter().zip(producer_unit_ids.iter().copied()) {
            let input = match (producer, unit_id) {
                (PlannedProducerV1::Unit(_), Some(unit_id)) if !unit_id.is_zero() => {
                    CanonicalInputRefV1 {
                        purpose: unit_purpose,
                        source_kind: InputSourceKind::UnitOutput,
                        source_id: unit_id,
                        record_count_limit: self.bindings.tribute_count,
                        max_encoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
                        max_decoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
                    }
                }
                (
                    PlannedProducerV1::CanonicalEmpty {
                        purpose: empty_purpose,
                        ..
                    },
                    None,
                ) if empty_purpose == unit_purpose => CanonicalInputRefV1 {
                    purpose: unit_purpose,
                    source_kind: InputSourceKind::CanonicalEmpty,
                    source_id: empty_unit_input_id(unit_purpose)?,
                    record_count_limit: 0,
                    max_encoded_bytes: 0,
                    max_decoded_bytes: 0,
                },
                _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
            };
            canonical_ordered_inputs.push(input);
        }
        let spec = UnitSpecV1 {
            protocol_bundle_hash: self.bindings.protocol_bundle_hash,
            job_id: self.bindings.job_id,
            attempt: self.bindings.attempt,
            phase,
            interval: UnitInterval::BinaryReducerNode(BinaryReducerNode { level, index }),
            canonical_ordered_inputs,
            lysis_program_semantics_hash: self.bindings.lysis_program_semantics_hash,
            planner_spec_version: self.bindings.planner_spec_version,
            reducer_spec_version: self.bindings.reducer_spec_version,
        };
        spec.validate_semantics(limits)?;
        Ok(spec)
    }

    fn root_reduce_unit_input(
        self,
        position: PlannedUnitPositionV1,
        unit_id: B256,
    ) -> Result<CanonicalInputRefV1, PlannerErrorV1> {
        let purpose = match position {
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::OutputFinalize,
                ..
            } => InputPurpose::FinalizedOutputRecords,
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::OwnerShuffle,
                ..
            } => InputPurpose::OwnerOrderedRecords,
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::BucketShuffle,
                ..
            } => InputPurpose::BucketOrderedRecords,
            PlannedUnitPositionV1::TreeNode {
                phase: UnitPhase::RootReduce,
                ..
            } => InputPurpose::RootSummary,
            _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
        };
        let record_count_limit = if purpose == InputPurpose::RootSummary {
            self.bindings.tribute_count
        } else {
            self.shuffle_position_record_limit(position)?
        };
        Ok(CanonicalInputRefV1 {
            purpose,
            source_kind: InputSourceKind::UnitOutput,
            source_id: unit_id,
            record_count_limit,
            max_encoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
            max_decoded_bytes: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_ocb1_bytes,
        })
    }

    fn shuffle_position_record_limit(
        self,
        position: PlannedUnitPositionV1,
    ) -> Result<u32, PlannerErrorV1> {
        let (start_run, end_run) = match position {
            PlannedUnitPositionV1::Primary {
                phase: UnitPhase::OutputFinalize,
                ordinal,
            } => {
                let shard = self.primary_tree.primary_shard(ordinal)?;
                return Ok(shard.record_count());
            }
            PlannedUnitPositionV1::RunSpan {
                phase: UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle,
                start_run,
                end_run,
                ..
            } => (start_run, end_run),
            _ => return Err(PlannerErrorV1::ProducerMembershipMismatch),
        };
        let start = start_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let end = end_run
            .checked_mul(PRIMARY_WORK_SHARD_SIZE)
            .ok_or(PlannerErrorV1::IntegerOverflow)?
            .min(self.bindings.tribute_count);
        end.checked_sub(start)
            .ok_or(PlannerErrorV1::IntegerOverflow)
    }
}
