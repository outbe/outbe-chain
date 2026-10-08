use super::*;

impl VerifiedPlanSchedule<'_> {
    pub(super) fn worker_request_at(
        &self,
        plan_ordinal: u32,
    ) -> Result<RunUnitV1, ExactLysisPlanError> {
        let position = self.topology.plan_position_at(plan_ordinal)?;
        let spec = self.derive_spec_at(plan_ordinal)?;
        let mut ordered_input_refs = Vec::new();
        for producer in self.topology.required_producers(position)? {
            if let PlannedProducerV1::Unit(producer) = producer {
                let producer_ordinal = self.topology.plan_ordinal_of(producer)?;
                if producer_ordinal >= plan_ordinal {
                    return Err(ExactLysisPlanError::AuthorityMismatch(
                        "worker producer topological order",
                    ));
                }
                ordered_input_refs
                    .push(self.plan_bound_admission_at(producer_ordinal)?.artifact_ref);
            }
        }

        let primary_ordinal = match position {
            PlannedUnitPositionV1::Primary { ordinal, .. } => Some(ordinal),
            _ => None,
        };
        match spec.phase {
            UnitPhase::Enumerate => {
                self.push_primary_input_ref(
                    primary_ordinal.ok_or(ExactLysisPlanError::AuthorityMismatch(
                        "Enumerate primary position",
                    ))?,
                    &mut ordered_input_refs,
                )?;
            }
            UnitPhase::FidelityMap => {
                self.push_primary_input_authority_refs(
                    primary_ordinal.ok_or(ExactLysisPlanError::AuthorityMismatch(
                        "FidelityMap primary position",
                    ))?,
                    &mut ordered_input_refs,
                )?;
                self.push_input_kind_refs(InputChunkKind::Fidelity, &mut ordered_input_refs)?;
            }
            UnitPhase::AmountMap => {
                self.push_primary_input_authority_refs(
                    primary_ordinal.ok_or(ExactLysisPlanError::AuthorityMismatch(
                        "AmountMap primary position",
                    ))?,
                    &mut ordered_input_refs,
                )?;
                self.push_input_kind_refs(InputChunkKind::Oracle, &mut ordered_input_refs)?;
            }
            UnitPhase::FixedReduce
            | UnitPhase::GratisPrefix
            | UnitPhase::GratisPrefixDown
            | UnitPhase::OutputFinalize
            | UnitPhase::OwnerShuffle
            | UnitPhase::BucketShuffle
            | UnitPhase::RootReduce => {}
        }

        let canonical_spec = spec.encode_canonical(self.limits)?;
        let unit_membership_siblings = if spec.phase == UnitPhase::Enumerate {
            try_streaming_ordered_list_membership_proof(
                ListKind::UnitSpecificationsArtifacts,
                self.plan.primary_work_unit_count,
                plan_ordinal,
                (0..self.plan.primary_work_unit_count).map(|ordinal| {
                    self.primary_spec_from_catalog(ordinal)?
                        .encode_canonical(self.limits)
                        .map_err(ExactLysisPlanError::from)
                }),
                self.limits.codec.max_body_bytes,
            )?
        } else {
            Vec::new()
        };
        Ok(RunUnitV1 {
            protocol_bundle_hash: self.plan.protocol_bundle_hash,
            job_id: self.plan.job_id,
            attempt: self.plan.attempt,
            plan_hash: self.plan.plan_hash(self.limits)?,
            unit_index: plan_ordinal,
            canonical_unit_spec: BoundedBytes(canonical_spec),
            unit_membership_siblings,
            plan_ref: self.plan_ref.clone(),
            input_manifest_ref: self.manifest_ref.clone(),
            ordered_input_refs,
        })
    }

    fn plan_bound_admission_at(
        &self,
        plan_ordinal: u32,
    ) -> Result<VerifiedAdmissionRecordV1, ExactLysisPlanError> {
        let admission = self.admissions.read(plan_ordinal)?;
        self.require_admission_authority(&admission)?;
        Ok(admission)
    }

    pub(super) fn derive_spec_at(
        &self,
        plan_ordinal: u32,
    ) -> Result<UnitSpecV1, ExactLysisPlanError> {
        let position = self.topology.plan_position_at(plan_ordinal)?;
        let phase = position.phase();
        let phase_ordinal = plan_ordinal
            .checked_sub(self.topology.phase_offset(phase)?)
            .ok_or(PlannerErrorV1::IntegerOverflow)?;
        let producer_ids = self.producer_unit_ids(position, plan_ordinal)?;

        match phase {
            UnitPhase::Enumerate => self.primary_spec_from_catalog(phase_ordinal),
            UnitPhase::FidelityMap => self
                .planner
                .fidelity_map_unit_at(
                    phase_ordinal,
                    required_unit_id(&producer_ids, 0)?,
                    self.limits,
                )
                .map_err(Into::into),
            UnitPhase::FixedReduce => self
                .planner
                .fixed_reduce_unit_at(phase_ordinal, exact_pair(&producer_ids)?, self.limits)
                .map_err(Into::into),
            UnitPhase::AmountMap => {
                let enumerate_spec = self.primary_spec_from_catalog(phase_ordinal)?;
                self.planner
                    .amount_map_unit_at(
                        phase_ordinal,
                        &enumerate_spec,
                        required_unit_id(&producer_ids, 1)?,
                        required_unit_id(&producer_ids, 2)?,
                        self.limits,
                    )
                    .map_err(Into::into)
            }
            UnitPhase::GratisPrefix => self
                .planner
                .gratis_prefix_unit_at(phase_ordinal, &producer_ids, self.limits)
                .map_err(Into::into),
            UnitPhase::GratisPrefixDown => self
                .planner
                .gratis_prefix_down_unit_at(phase_ordinal, &producer_ids, self.limits)
                .map_err(Into::into),
            UnitPhase::OutputFinalize => {
                let amount_position = PlannedUnitPositionV1::Primary {
                    phase: UnitPhase::AmountMap,
                    ordinal: phase_ordinal,
                };
                let amount_spec =
                    self.derive_spec_at(self.topology.plan_ordinal_of(amount_position)?)?;
                self.planner
                    .output_finalize_unit_at(
                        phase_ordinal,
                        &amount_spec,
                        required_unit_id(&producer_ids, 1)?,
                        self.limits,
                    )
                    .map_err(Into::into)
            }
            UnitPhase::OwnerShuffle | UnitPhase::BucketShuffle => {
                let exact = producer_ids
                    .iter()
                    .copied()
                    .map(|unit_id| {
                        unit_id.ok_or(ExactLysisPlanError::AuthorityMismatch("shuffle producer"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.planner
                    .shuffle_unit_at(phase, phase_ordinal, &exact, self.limits)
                    .map_err(Into::into)
            }
            UnitPhase::RootReduce => self
                .planner
                .root_reduce_unit_at(phase_ordinal, &producer_ids, self.limits)
                .map_err(Into::into),
        }
    }

    pub(super) fn primary_spec_from_catalog(
        &self,
        shard_ordinal: u32,
    ) -> Result<UnitSpecV1, ExactLysisPlanError> {
        let current = self.input_refs.verified_reference_at(
            shard_ordinal,
            self.reader,
            self.bundle.bundle(),
        )?;
        let next = if shard_ordinal + 1 < self.plan.primary_work_unit_count {
            Some(self.input_refs.verified_reference_at(
                shard_ordinal + 1,
                self.reader,
                self.bundle.bundle(),
            )?)
        } else {
            None
        };
        self.primary_spec_from_refs(
            shard_ordinal,
            &current.reference,
            next.as_ref().map(|verified| &verified.reference),
        )
    }

    pub(super) fn primary_spec_from_refs(
        &self,
        shard_ordinal: u32,
        current: &InputChunkRefV1,
        next: Option<&InputChunkRefV1>,
    ) -> Result<UnitSpecV1, ExactLysisPlanError> {
        self.planner
            .primary_unit_at(
                shard_ordinal,
                |ordinal| {
                    if ordinal == shard_ordinal {
                        Some(current.clone())
                    } else if ordinal == shard_ordinal + 1 {
                        next.cloned()
                    } else {
                        None
                    }
                },
                self.limits,
            )
            .map_err(Into::into)
    }

    pub(super) fn push_primary_input_ref(
        &self,
        shard_ordinal: u32,
        output: &mut Vec<CasObjectRefV1>,
    ) -> Result<(), ExactLysisPlanError> {
        let verified = self.input_refs.verified_reference_at(
            shard_ordinal,
            self.reader,
            self.bundle.bundle(),
        )?;
        if verified.reference.kind != InputChunkKind::Tribute {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "primary Tribute input kind",
            ));
        }
        output.push(input_object_ref(&verified.reference));
        Ok(())
    }

    /// Adds the shard plus the bounded one-shard lookahead needed to rederive
    /// the exact half-open primary interval. The lookahead is authenticated
    /// authority only. Phase semantics continue to consume the admitted
    /// Enumerate producer for the current shard.
    pub(super) fn push_primary_input_authority_refs(
        &self,
        shard_ordinal: u32,
        output: &mut Vec<CasObjectRefV1>,
    ) -> Result<(), ExactLysisPlanError> {
        self.push_primary_input_ref(shard_ordinal, output)?;
        let next = shard_ordinal
            .checked_add(1)
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "primary Tribute lookahead ordinal",
            ))?;
        if next < self.plan.primary_work_unit_count {
            self.push_primary_input_ref(next, output)?;
        }
        Ok(())
    }

    pub(super) fn push_input_kind_refs(
        &self,
        kind: InputChunkKind,
        output: &mut Vec<CasObjectRefV1>,
    ) -> Result<(), ExactLysisPlanError> {
        let mut matched = 0_u32;
        for verified in self
            .input_refs
            .exact_verified_cursor(self.reader, self.bundle.bundle())?
        {
            let verified = verified?;
            if verified.reference.kind == kind {
                output.push(input_object_ref(&verified.reference));
                matched = matched
                    .checked_add(1)
                    .ok_or(PlannerErrorV1::IntegerOverflow)?;
            }
        }
        if matched == 0 {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "required authenticated input kind",
            ));
        }
        Ok(())
    }

    pub(super) fn producer_unit_ids(
        &self,
        position: PlannedUnitPositionV1,
        consumer_ordinal: u32,
    ) -> Result<Vec<Option<B256>>, ExactLysisPlanError> {
        self.topology
            .required_producers(position)?
            .into_iter()
            .map(|producer| match producer {
                PlannedProducerV1::CanonicalEmpty { .. } => Ok(None),
                PlannedProducerV1::Unit(producer) => {
                    let producer_ordinal = self.topology.plan_ordinal_of(producer)?;
                    if producer_ordinal >= consumer_ordinal {
                        return Err(ExactLysisPlanError::AuthorityMismatch(
                            "producer topological order",
                        ));
                    }
                    let record = self.admissions.read(producer_ordinal)?;
                    self.require_admission_authority(&record)?;
                    Ok(Some(record.unit_id))
                }
            })
            .collect()
    }

    pub(super) fn require_admission_authority(
        &self,
        record: &VerifiedAdmissionRecordV1,
    ) -> Result<(), ExactLysisPlanError> {
        let authority = self.admissions.plan_authority();
        if (
            record.protocol_bundle_hash,
            record.job_id,
            record.attempt,
            record.plan_hash,
        ) != (
            authority.protocol_bundle_hash,
            authority.job_id,
            authority.attempt,
            authority.plan_hash,
        ) || record.unit_id.is_zero()
        {
            return Err(ExactLysisPlanError::AuthorityMismatch("producer admission"));
        }
        Ok(())
    }
}
