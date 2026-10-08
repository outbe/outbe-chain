use super::*;

impl Iterator for LysisPlanAuditCursorV1<'_> {
    type Item = Result<LysisPlanAuditStepV1, ExactLysisPlanError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.stage == LysisPlanAuditStageV1::Complete {
            return None;
        }
        let result = match self.stage {
            LysisPlanAuditStageV1::InputCatalog => self.advance_input_catalog(),
            LysisPlanAuditStageV1::Artifacts => self.advance_artifacts(),
            LysisPlanAuditStageV1::AdmissionCatalog => self.advance_admission_catalog(),
            LysisPlanAuditStageV1::Complete => return None,
        };
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

impl LysisPlanAuditCursorV1<'_> {
    fn advance_input_catalog(&mut self) -> Result<LysisPlanAuditStepV1, ExactLysisPlanError> {
        if self.owner_tribute_search.is_some()
            || self.next_fidelity_owner < self.pending_fidelity_owners.len()
        {
            return self.advance_fidelity_owner_membership();
        }
        if !self.pending_fidelity_owners.is_empty() {
            self.pending_fidelity_owners.clear();
            self.next_fidelity_owner = 0;
        }
        let step = self
            .input_catalog
            .as_mut()
            .and_then(Iterator::next)
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "input catalog closure",
            ))??;
        match step {
            InputRefCatalogClosureStepV1::Reference(reference) => {
                self.observe_input_reference(&reference)?;
                let verified = self.audit.schedule.input_refs.verify_reference(
                    reference.clone(),
                    self.audit.schedule.reader,
                    self.audit.schedule.bundle.bundle(),
                )?;
                self.observe_verified_input_chunk(&verified)?;
                Ok(LysisPlanAuditStepV1::InputChecked {
                    ordinal: reference.ordinal,
                    kind: reference.kind,
                })
            }
            InputRefCatalogClosureStepV1::ReferencesClosed => {
                Ok(LysisPlanAuditStepV1::InputReferenceListClosed)
            }
            InputRefCatalogClosureStepV1::DirectoryEntryChecked => {
                Ok(LysisPlanAuditStepV1::InputCatalogEntryChecked)
            }
            InputRefCatalogClosureStepV1::Complete => {
                self.close_plan_and_prepare_input_roots()?;
                self.close_input_artifacts()?;
                self.stage = LysisPlanAuditStageV1::Artifacts;
                Ok(LysisPlanAuditStepV1::InputsClosed)
            }
        }
    }

    fn advance_fidelity_owner_membership(
        &mut self,
    ) -> Result<LysisPlanAuditStepV1, ExactLysisPlanError> {
        if self.owner_tribute_search.is_none() {
            let owner = *self
                .pending_fidelity_owners
                .get(self.next_fidelity_owner)
                .ok_or(ExactLysisPlanError::AuthorityMismatch(
                    "pending Fidelity owner",
                ))?;
            let tribute_id = derive_poseidon_entity_id(
                owner,
                WorldwideDay::new(self.audit.schedule.manifest.wwd),
            )
            .map_err(|_| ExactLysisPlanError::AuthorityMismatch("Tribute owner identity"))?;
            self.owner_tribute_search = Some(OwnerTributeSearchV1 {
                owner,
                tribute_id: tribute_id.to_vec(),
                low: 0,
                high: self.audit.schedule.plan.primary_work_unit_count,
            });
        }

        let search =
            self.owner_tribute_search
                .as_mut()
                .ok_or(ExactLysisPlanError::AuthorityMismatch(
                    "Fidelity owner search state",
                ))?;
        if search.low >= search.high {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity owner Tribute membership",
            ));
        }
        let middle = search.low + (search.high - search.low) / 2;
        let verified = self.audit.schedule.input_refs.verified_reference_at(
            middle,
            self.audit.schedule.reader,
            self.audit.schedule.bundle.bundle(),
        )?;
        if verified.reference.kind != InputChunkKind::Tribute {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity owner Tribute search range",
            ));
        }
        let target = search.tribute_id.as_slice();
        if target < verified.reference.first_key.0.as_slice() {
            search.high = middle;
            return Ok(LysisPlanAuditStepV1::FidelityOwnerMembershipProbe {
                owner: search.owner,
            });
        }
        if target > verified.reference.last_key_inclusive.0.as_slice() {
            search.low = middle
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "Fidelity owner search lower bound",
                })?;
            return Ok(LysisPlanAuditStepV1::FidelityOwnerMembershipProbe {
                owner: search.owner,
            });
        }

        let owner = search.owner;
        let found = verified
            .chunk
            .canonical_records_or_openings
            .iter()
            .map(|record| outbe_tribute::record::decode_canonical(&record.0))
            .find_map(|result| match result {
                Ok(tribute) if tribute.tribute_id.as_slice() == target => Some(Ok(tribute)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .transpose()?
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity owner Tribute membership",
            ))?;
        if found.owner != owner || found.worldwide_day.value() != self.audit.schedule.manifest.wwd {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity owner Tribute binding",
            ));
        }
        self.owner_tribute_search = None;
        self.next_fidelity_owner =
            self.next_fidelity_owner
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "Fidelity owner cursor",
                })?;
        self.fidelity_owner_count =
            self.fidelity_owner_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "Fidelity owner count",
                })?;
        Ok(LysisPlanAuditStepV1::FidelityOwnerMembershipChecked { owner })
    }

    fn observe_input_reference(
        &mut self,
        reference: &InputChunkRefV1,
    ) -> Result<(), ExactLysisPlanError> {
        match reference.kind {
            InputChunkKind::Tribute => {
                if let Some(previous) = self.pending_tribute_ref.replace(reference.clone()) {
                    self.push_primary_spec(previous, Some(reference))?;
                }
            }
            InputChunkKind::Fidelity => {
                self.flush_last_primary_spec()?;
                self.fidelity_opening_count = self
                    .fidelity_opening_count
                    .checked_add(reference.record_count)
                    .ok_or(ProtocolError::IntegerOverflow {
                        what: "Fidelity opening count",
                    })?;
            }
            InputChunkKind::Oracle => {
                self.flush_last_primary_spec()?;
                self.oracle_opening_count = self
                    .oracle_opening_count
                    .checked_add(reference.record_count)
                    .ok_or(ProtocolError::IntegerOverflow {
                        what: "Oracle opening count",
                    })?;
            }
        }
        Ok(())
    }

    fn push_primary_spec(
        &mut self,
        current: InputChunkRefV1,
        next: Option<&InputChunkRefV1>,
    ) -> Result<(), ExactLysisPlanError> {
        let spec =
            self.audit
                .schedule
                .primary_spec_from_refs(self.primary_spec_count, &current, next)?;
        self.primary_root
            .as_mut()
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "primary plan root state",
            ))?
            .push(
                &spec.encode_canonical(self.audit.schedule.limits)?,
                self.audit.schedule.limits.codec.max_body_bytes,
            )?;
        self.primary_spec_count =
            self.primary_spec_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "primary spec count",
                })?;
        Ok(())
    }

    fn flush_last_primary_spec(&mut self) -> Result<(), ExactLysisPlanError> {
        if let Some(previous) = self.pending_tribute_ref.take() {
            self.push_primary_spec(previous, None)?;
        }
        Ok(())
    }

    fn close_plan_and_prepare_input_roots(&mut self) -> Result<(), ExactLysisPlanError> {
        self.flush_last_primary_spec()?;
        let root = self
            .primary_root
            .take()
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "primary plan root state",
            ))?
            .finish()?;
        if self.primary_spec_count != self.audit.schedule.plan.primary_work_unit_count
            || root != self.audit.schedule.plan.primary_work_unit_root
            || self.fidelity_opening_count == 0
            || self.oracle_opening_count != 1
        {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "rederived plan and opening counts",
            ));
        }
        Ok(())
    }

    fn observe_verified_input_chunk(
        &mut self,
        verified: &VerifiedInputChunkRefV1,
    ) -> Result<(), ExactLysisPlanError> {
        let reference = &verified.reference;
        if reference.kind == InputChunkKind::Tribute
            && self
                .previous_tribute_last_key
                .as_ref()
                .is_some_and(|last| last.as_slice() >= reference.first_key.0.as_slice())
        {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Tribute cross-chunk key order",
            ));
        }
        if reference.kind == InputChunkKind::Tribute {
            self.previous_tribute_last_key = Some(reference.last_key_inclusive.0.clone());
        } else if verified.chunk.canonical_records_or_openings.len() != 1 {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "one opening record per input chunk",
            ));
        }

        for record in &verified.chunk.canonical_records_or_openings {
            match reference.kind {
                InputChunkKind::Tribute => self.observe_tribute(&record.0)?,
                InputChunkKind::Fidelity => self.observe_fidelity_opening(&record.0)?,
                InputChunkKind::Oracle => self.observe_oracle_opening(&record.0)?,
            }
        }
        Ok(())
    }

    fn observe_tribute(&mut self, encoded: &[u8]) -> Result<(), ExactLysisPlanError> {
        let tribute = outbe_tribute::record::decode_canonical(encoded)?.calculation_view()?;
        if tribute.worldwide_day.value() != self.audit.schedule.manifest.wwd {
            return Err(ExactLysisPlanError::AuthorityMismatch("Tribute WWD"));
        }
        self.tribute_count =
            self.tribute_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "Tribute count",
                })?;
        self.tribute_nominal_total = self
            .tribute_nominal_total
            .checked_add(tribute.nominal_amount_minor)
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "Tribute nominal total overflow",
            ))?;
        self.tribute_isos.insert(tribute.reference_currency);
        if self.tribute_isos.len() > MAX_SETTLEMENT_ISOS {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "settlement ISO bound",
            ));
        }
        Ok(())
    }

    fn has_pending_fidelity_membership(&self) -> bool {
        !self.pending_fidelity_owners.is_empty() || self.owner_tribute_search.is_some()
    }

    fn observe_fidelity_opening(&mut self, encoded: &[u8]) -> Result<(), ExactLysisPlanError> {
        let opening =
            AuthenticatedOpeningV1::decode_canonical_record(encoded, self.audit.schedule.limits)?;
        if opening.source_kind != OpeningSourceKind::Fidelity {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity opening source",
            ));
        }
        opening.validate_against_bundle(
            self.audit.schedule.bundle.bundle(),
            self.audit.schedule.limits,
        )?;
        let _ = opening.decode_and_validate_raw_opening(
            self.audit.schedule.manifest.checkpoint.finalized_state_root,
            self.audit.schedule.limits,
        )?;
        self.fidelity_root
            .as_mut()
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity root state",
            ))?
            .push(encoded, self.audit.schedule.limits.max_bounded_bytes)?;
        let owners = decode_fidelity_subject_key(&opening.canonical_subject_key.0)?;
        if owners.is_empty()
            || owners.len()
                > usize::try_from(outbe_lysis::program_v1::planner::PRIMARY_WORK_SHARD_SIZE)
                    .map_err(|_| {
                        ExactLysisPlanError::AuthorityMismatch("Fidelity owner batch bound")
                    })?
            || self.has_pending_fidelity_membership()
        {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity owner batch state",
            ));
        }
        for owner in &owners {
            if self
                .previous_fidelity_owner
                .is_some_and(|previous| previous >= *owner)
            {
                return Err(ExactLysisPlanError::AuthorityMismatch(
                    "Fidelity owner order",
                ));
            }
            self.previous_fidelity_owner = Some(*owner);
        }
        self.pending_fidelity_owners = owners;
        self.next_fidelity_owner = 0;
        Ok(())
    }

    fn observe_oracle_opening(&mut self, encoded: &[u8]) -> Result<(), ExactLysisPlanError> {
        if self.oracle_subject_isos.is_some() {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "exactly one Oracle opening",
            ));
        }
        let opening =
            AuthenticatedOpeningV1::decode_canonical_record(encoded, self.audit.schedule.limits)?;
        if opening.source_kind != OpeningSourceKind::Oracle {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "Oracle opening source",
            ));
        }
        opening.validate_against_bundle(
            self.audit.schedule.bundle.bundle(),
            self.audit.schedule.limits,
        )?;
        let _ = opening.decode_and_validate_raw_opening(
            self.audit.schedule.manifest.checkpoint.finalized_state_root,
            self.audit.schedule.limits,
        )?;
        let (wwd, isos) = decode_oracle_subject_key(&opening.canonical_subject_key.0)?;
        if wwd != self.audit.schedule.manifest.wwd {
            return Err(ExactLysisPlanError::AuthorityMismatch("Oracle opening WWD"));
        }
        self.oracle_root
            .as_mut()
            .ok_or(ExactLysisPlanError::AuthorityMismatch("Oracle root state"))?
            .push(encoded, self.audit.schedule.limits.max_bounded_bytes)?;
        self.oracle_subject_isos = Some(isos);
        Ok(())
    }

    fn close_input_artifacts(&mut self) -> Result<(), ExactLysisPlanError> {
        let fidelity_root = self
            .fidelity_root
            .take()
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "Fidelity root state",
            ))?
            .finish()?;
        let oracle_root = self
            .oracle_root
            .take()
            .ok_or(ExactLysisPlanError::AuthorityMismatch("Oracle root state"))?
            .finish()?;
        let expected_isos = self.tribute_isos.iter().copied().collect::<Vec<_>>();
        let tribute_conservation_matches = (
            self.tribute_count,
            self.tribute_nominal_total,
            self.fidelity_owner_count,
        ) == (
            self.audit.schedule.manifest.tribute_count,
            self.audit.schedule.manifest.tribute_nominal_total,
            self.audit.schedule.manifest.tribute_count,
        );
        let opening_roots_match = (fidelity_root, oracle_root)
            == (
                self.audit.schedule.manifest.fidelity_opening_root,
                self.audit.schedule.manifest.oracle_opening_root,
            );
        let currency_coverage_matches = self.oracle_subject_isos.as_ref() == Some(&expected_isos);
        if !tribute_conservation_matches || !opening_roots_match || !currency_coverage_matches {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "complete input artifact semantics",
            ));
        }
        Ok(())
    }

    fn advance_artifacts(&mut self) -> Result<LysisPlanAuditStepV1, ExactLysisPlanError> {
        if self.next_artifact_ordinal < self.audit.schedule.topology.total_unit_count() {
            let plan_ordinal = self.next_artifact_ordinal;
            self.next_artifact_ordinal += 1;
            return Ok(LysisPlanAuditStepV1::Artifact(Box::new(
                self.audit.verified_artifact_at(plan_ordinal)?,
            )));
        }
        self.admission_catalog = Some(self.audit.schedule.admissions.bounded_directory_cursor()?);
        self.stage = LysisPlanAuditStageV1::AdmissionCatalog;
        Ok(LysisPlanAuditStepV1::ArtifactsClosed)
    }

    fn advance_admission_catalog(&mut self) -> Result<LysisPlanAuditStepV1, ExactLysisPlanError> {
        let step = self
            .admission_catalog
            .as_mut()
            .and_then(Iterator::next)
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "admission catalog closure",
            ))??;
        if step == AdmissionDirectoryStepV1::Complete {
            self.stage = LysisPlanAuditStageV1::Complete;
            Ok(LysisPlanAuditStepV1::Complete)
        } else {
            Ok(LysisPlanAuditStepV1::AdmissionCatalogEntryChecked)
        }
    }
}
