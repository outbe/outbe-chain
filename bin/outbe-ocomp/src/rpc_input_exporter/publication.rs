use super::*;

pub(super) struct ExportWork<'a> {
    pub discovery: &'a DiscoveryRecord,
    pub finalized: &'a VerifiedFinalizedIntentV1,
    pub expected: &'a ExpectedInputAuthorityV1,
    pub job_id: B256,
    pub input_ref_catalog_root: &'a std::path::Path,
    pub work_root: &'a std::path::Path,
}

pub(super) struct PublicationReadHandles {
    _fidelity_cursor: crate::opening_stage::FidelityOpeningCursor,
    _opening_stage: DurableOpeningStage,
    _bodies: crate::input_inventory::TributeBodySpoolReader,
}

impl RpcInputExporterV1 {
    pub(super) fn replay_receipt(
        &self,
        work: &ExportWork<'_>,
        on_progress: &impl Fn(),
    ) -> Result<Option<VerifiedExportReceipt>, RpcInputExporterErrorV1> {
        let ExportWork {
            discovery,
            finalized,
            expected: expected_input,
            job_id,
            input_ref_catalog_root,
            work_root,
        } = *work;
        if let Some(reader) =
            ExportReceiptReader::try_open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("inspect input receipt", error))?
        {
            match reader.load_exact(&self.reader) {
                Ok(receipt) => {
                    require_receipt_generation(&receipt, discovery.generation)?;
                    require_replayed_input_authority(
                        expected_input,
                        receipt.checkpoint(),
                        receipt.manifest(),
                    )?;
                    let catalog = VerifiedInputChunkRefCatalog::reopen(
                        input_ref_catalog_root,
                        &self.reader,
                        self.config.limits,
                        poc_input_list_limits(),
                    )
                    .map_err(|error| stage("reload input-ref catalog", error))?;
                    catalog
                        .require_manifest_authority(&receipt.manifest_ref(), receipt.manifest())
                        .map_err(|error| stage("bind input-ref catalog to receipt", error))?;
                    validate_verified_input_manifest_semantics_observing(
                        &catalog,
                        InputManifestVerification {
                            reader: &self.reader,
                            bundle: self.config.protocol_bundle.bundle(),
                            manifest: receipt.manifest(),
                            limits: &self.config.limits,
                        },
                        on_progress,
                    )
                    .map_err(|error| stage("verify replayed input semantics", error))?;
                    verify_replayed_finalized_inputs(
                        ReplayAuthority {
                            catalog: &catalog,
                            reader: &self.reader,
                            work_root,
                            finalized,
                            expected: expected_input,
                            bundle: self.config.protocol_bundle.bundle(),
                            limits: &self.config.limits,
                        },
                        on_progress,
                    )?;
                    on_progress();
                    return Ok(Some(receipt));
                }
                Err(
                    ExportReceiptError::MissingPreparation | ExportReceiptError::MissingReceipt,
                ) => {}
                Err(error) => return Err(stage("reload input receipt", error)),
            }
        }

        Ok(None)
    }
    pub(super) fn prepare_inventory(
        &self,
        work: &ExportWork<'_>,
        on_progress: &impl Fn(),
    ) -> Result<(SealedTributeInventory, B256), RpcInputExporterErrorV1> {
        let ExportWork {
            finalized,
            expected: expected_input,
            job_id,
            work_root,
            ..
        } = *work;
        let input_lease_id = finalized
            .intent
            .input_lease_id()
            .map_err(|error| stage("derive input lease identity", error))?;

        let checkpoint = expected_input.checkpoint.clone();
        let inventory_subject = TributeInventorySubjectV1 {
            protocol_bundle_hash: self.config.protocol_bundle_hash,
            job_id,
            attempt: finalized.intent.attempt,
            checkpoint: checkpoint.clone(),
            chain_id: finalized.intent.chain_id,
            worldwide_day: WorldwideDay::new(finalized.intent.wwd),
            expected_tribute_count: finalized.intent.authenticated_day_count,
            expected_nominal_total: finalized.intent.authenticated_day_nominal,
        };
        let inventory_root = work_root.join("inventory");
        let inventory = match crate::input_inventory::open_sealed_inventory_observing(
            &inventory_root,
            inventory_subject.clone(),
            on_progress,
        ) {
            Ok(inventory) => inventory,
            Err(TributeInventoryError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let sources = archive_state_tribute_day(
                    &self.rpc,
                    &inventory_root.join(crate::input_inventory::SOURCE_PROOF_ARCHIVE_DIRECTORY),
                    inventory_subject.state_source(),
                    on_progress,
                )?;
                build_inventory(&sources, &inventory_root, inventory_subject, on_progress)?
            }
            Err(error) => return Err(stage("reopen Tribute inventory", error)),
        };
        on_progress();
        Ok((inventory, input_lease_id))
    }
    pub(super) fn publish_inventory(
        &self,
        work: &ExportWork<'_>,
        inventory: &SealedTributeInventory,
        on_progress: &impl Fn(),
    ) -> Result<(PublishedStreamingInputArtifacts, PublicationReadHandles), RpcInputExporterErrorV1>
    {
        let ExportWork {
            finalized,
            expected: expected_input,
            job_id,
            input_ref_catalog_root,
            ..
        } = *work;
        let checkpoint = expected_input.checkpoint.clone();
        let mut publisher = DurableInputArtifactPublisher::open(
            InputArtifactContext {
                cas: &self.cas,
                bundle: self.config.protocol_bundle.bundle(),
                limits: self.config.limits,
                list_limits: poc_input_list_limits(),
            },
            &self.reader,
            input_ref_catalog_root,
            InputArtifactIdentity {
                job_id,
                attempt: finalized.intent.attempt,
                checkpoint: checkpoint.clone(),
                wwd: finalized.intent.wwd,
            },
        )
        .map_err(|error| stage("open durable input publisher", error))?;
        let mut bodies = inventory
            .tribute_bodies()
            .map_err(|error| stage("open Tribute body spool", error))?;
        let mut bodies_since_progress = 0_u64;
        while let Some(body) = bodies
            .next_body(self.config.limits.max_bounded_bytes)
            .map_err(|error| stage("read Tribute body spool", error))?
        {
            publisher
                .publish_tribute(body)
                .map_err(|error| stage("publish Tribute input chunk", error))?;
            bodies_since_progress = bodies_since_progress.saturating_add(1);
            if bodies_since_progress == EXPORT_PROGRESS_RECORD_HEARTBEAT {
                on_progress();
                bodies_since_progress = 0;
            }
        }
        publisher
            .finish_tributes()
            .map_err(|error| stage("finish Tribute input chunks", error))?;

        let (opening_stage, opening_report) =
            self.acquire_openings(work, inventory, &mut publisher, on_progress)?;
        on_progress();
        publisher
            .publish_oracle_opening(opening_report.oracle)
            .map_err(|error| stage("publish Oracle opening", error))?;
        let mut fidelity_cursor =
            opening_stage.fidelity_cursor(opening_report.fidelity_opening_count);
        let published = publisher
            .finish_observing(
                ExpectedInputCounts {
                    tribute_count: finalized.intent.authenticated_day_count,
                    tribute_nominal_total: finalized.intent.authenticated_day_nominal,
                    fidelity_openings: opening_report.fidelity_opening_count,
                },
                || {
                    fidelity_cursor
                        .next_opening()
                        .map_err(InputArtifactError::opening_source)
                },
                on_progress,
            )
            .map_err(|error| stage("seal input artifacts", error))?;
        Ok((
            published,
            PublicationReadHandles {
                _fidelity_cursor: fidelity_cursor,
                _opening_stage: opening_stage,
                _bodies: bodies,
            },
        ))
    }
    fn acquire_openings(
        &self,
        work: &ExportWork<'_>,
        inventory: &SealedTributeInventory,
        publisher: &mut DurableInputArtifactPublisher<'_>,
        on_progress: &impl Fn(),
    ) -> Result<(DurableOpeningStage, OpeningStageReportV1), RpcInputExporterErrorV1> {
        let ExportWork {
            finalized,
            job_id,
            work_root,
            expected,
            ..
        } = *work;
        let checkpoint = expected.checkpoint.clone();
        let mut opening_stage = DurableOpeningStage::open_or_resume(
            work_root.join("openings"),
            OpeningStageSubjectV1 {
                protocol_bundle_hash: self.config.protocol_bundle_hash,
                job_id,
                attempt: finalized.intent.attempt,
                checkpoint: checkpoint.clone(),
                worldwide_day: finalized.intent.wwd,
                inventory_authority_digest: inventory.authority_digest(),
            },
            self.config.limits,
        )
        .map_err(|error| stage("open durable opening stage", error))?;
        let opening_report = opening_stage
            .run(
                inventory,
                |subjects| {
                    on_progress();
                    let canonical_request = BuildLysisOpeningsV1 {
                        job_id,
                        subjects: subjects.clone(),
                    }
                    .encode_body(&self.config.limits)
                    .map_err(OpeningStageError::resolver)?;
                    let openings = match self
                        .rpc
                        .lysis_openings(finalized.intent_id, &canonical_request)
                        .and_then(|encoded| {
                            LysisOpeningsProofV1::decode_body(&encoded, &self.config.limits)
                                .map_err(|error| {
                                    crate::public_rpc::PublicRpcError::malformed(
                                        "outbe_getOcompLysisOpeningsV1",
                                        error,
                                    )
                                })
                        }) {
                        Ok(openings) => openings,
                        Err(error) if is_lysis_opening_capacity_error(&error) => {
                            return Ok(OpeningResolutionV1::Split);
                        }
                        Err(error) => {
                            return Err(OpeningStageError::resolver(error));
                        }
                    };
                    verify_lysis_openings(&openings, finalized, subjects, &self.config.limits)
                        .map_err(OpeningStageError::resolver)?;
                    on_progress();
                    let materialized = materialize_authenticated_openings(
                        &openings,
                        self.config.protocol_bundle.bundle(),
                        &self.config.limits,
                    )
                    .map_err(OpeningStageError::resolver)?;
                    Ok(OpeningResolutionV1::Complete(Box::new(materialized)))
                },
                |subjects, fidelity, oracle| {
                    on_progress();
                    verify_durable_lysis_openings(
                        fidelity,
                        oracle,
                        finalized,
                        subjects,
                        (self.config.protocol_bundle.bundle(), &self.config.limits),
                    )
                },
                |opening| {
                    on_progress();
                    publisher
                        .publish_fidelity_opening(opening)
                        .map_err(OpeningStageError::from)
                },
            )
            .map_err(|error| stage("acquire and publish Lysis openings", error))?;
        Ok((opening_stage, opening_report))
    }
    pub(super) fn commit_publication(
        &self,
        work: &ExportWork<'_>,
        input_lease_id: B256,
        published: PublishedStreamingInputArtifacts,
        on_progress: &impl Fn(),
    ) -> Result<VerifiedExportReceipt, RpcInputExporterErrorV1> {
        let ExportWork {
            discovery,
            expected: expected_input,
            job_id,
            ..
        } = *work;
        let checkpoint = expected_input.checkpoint.clone();
        // This is an OCOMP-local publication record, not a node acknowledgement.
        // The legacy envelope remains versioned for crash-safe compatibility.
        let handoff = SnapshotHandoffV1 {
            job_id,
            input_lease_id,
            pin_generation: discovery.generation,
            lease_generation: 1,
            checkpoint,
            canonical_lease_offer: BoundedBytes(local_publication_lease(job_id)),
        };
        let mut receipt_store =
            ExportReceiptStore::open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("open input receipt", error))?;
        let (_, prepared) = receipt_store
            .prepare(
                &self.cas,
                &self.reader,
                ExportReceiptPreparation {
                    handoff: &handoff,
                    manifest_ref: &published.manifest_ref,
                    manifest_hash: published.manifest_hash,
                },
            )
            .map_err(|error| stage("prepare input receipt", error))?;
        let committed = SnapshotExportCommittedV1 {
            job_id,
            pin_generation: committed_pin_generation(discovery.generation)?,
            record_hash: keccak256(
                [
                    b"OCOMP_RPC_INPUT_COMMIT_V1".as_slice(),
                    job_id.as_slice(),
                    published.manifest_hash.as_slice(),
                ]
                .concat(),
            ),
        };
        receipt_store
            .record_committed(&self.cas, &self.reader, &prepared, &committed)
            .map_err(|error| stage("commit input receipt", error))?;
        on_progress();
        drop(receipt_store);
        let receipt =
            ExportReceiptReader::open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("reopen committed input receipt", error))?
                .load_exact(&self.reader)
                .map_err(|error| stage("reload committed input receipt", error))?;
        require_receipt_generation(&receipt, discovery.generation)?;
        Ok(receipt)
    }
}

fn build_inventory(
    sources: &crate::input_inventory::StateTributeProofArchive,
    inventory_root: &std::path::Path,
    inventory_subject: TributeInventorySubjectV1,
    on_progress: &impl Fn(),
) -> Result<SealedTributeInventory, RpcInputExporterErrorV1> {
    let mut builder = TributeInventoryBuilder::create(
        inventory_root,
        inventory_subject,
        TributeInventoryWorkConfig::default(),
        sources,
    )
    .map_err(|error| stage("create Tribute inventory", error))?;
    sources
        .for_each_canonical(
            |source| {
                let record = outbe_tribute::record::from_encrypted(source.tribute);
                let canonical_body = record
                    .stored_body()
                    .map_err(|_| TributeInventoryError::Authority("canonical encrypted Tribute"))?
                    .payload()
                    .to_vec();
                builder.push(TributeInventoryRecordV1 {
                    tribute_id: record.tribute_id,
                    owner: record.owner,
                    reference_iso: record.reference_currency,
                    canonical_body,
                })
            },
            on_progress,
        )
        .map_err(|error| stage("spool proven Tribute inventory", error))?;
    builder
        .finish_observing(on_progress)
        .map_err(|error| stage("seal Tribute inventory", error))
}
