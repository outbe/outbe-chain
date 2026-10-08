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
    ) -> Result<(SealedTributeInventory, RetainedTributePin), RpcInputExporterErrorV1> {
        let ExportWork {
            finalized,
            expected: expected_input,
            job_id,
            work_root,
            ..
        } = *work;
        let projection_config = ProjectionConfig {
            chain_id: self.config.chain_id,
            genesis_hash: self.config.genesis_hash,
            start_block: self.config.storage.start_block,
        };
        // Keep one caught-up secondary view for the checkpoint and the entire
        // inventory. No read can refresh this session while it is being consumed.
        let storage = self
            .storage_source
            .open_session()
            .map_err(source_open_error)?;
        let tribute_source = FinalizedTributeSource::new(storage, self.config.tribute_page_limit)
            .map_err(|error| stage("open finalized Tribute source", error))?;
        let projection_state = tribute_source
            .projection_state(projection_config)
            .map_err(|error| stage("read finalized projection checkpoint", error))?;
        require_projection_checkpoint(projection_state.as_ref(), &finalized.request)?;
        let pin = RetainedTributePin {
            input_lease_id: finalized
                .intent
                .input_lease_id()
                .map_err(|error| stage("derive Tribute retention pin", error))?,
            worldwide_day: WorldwideDay::new(finalized.intent.wwd),
        };

        let checkpoint = expected_input.checkpoint.clone();
        let inventory_subject = TributeInventorySubjectV1 {
            protocol_bundle_hash: self.config.protocol_bundle_hash,
            job_id,
            attempt: finalized.intent.attempt,
            checkpoint: checkpoint.clone(),
            worldwide_day: WorldwideDay::new(finalized.intent.wwd),
            sealed_tribute_collection_root: finalized.intent.sealed_tribute_collection_root,
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
                build_inventory(
                    &tribute_source,
                    pin,
                    &inventory_root,
                    inventory_subject,
                    on_progress,
                )?
            }
            Err(error) => return Err(stage("reopen Tribute inventory", error)),
        };
        drop(tribute_source);
        on_progress();
        Ok((inventory, pin))
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
                sealed_tribute_collection_key: finalized.intent.sealed_tribute_collection_key,
                sealed_tribute_collection_root: finalized.intent.sealed_tribute_collection_root,
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
                        .map_err(|error| InputArtifactError::OpeningSource(error.to_string()))
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
                    .map_err(|error| OpeningStageError::Resolver(error.to_string()))?;
                    let openings = match self
                        .rpc
                        .lysis_openings(finalized.intent_id, &canonical_request)
                        .and_then(|encoded| {
                            LysisOpeningsProofV1::decode_body(&encoded, &self.config.limits)
                                .map_err(|error| crate::public_rpc::PublicRpcError::Malformed {
                                    method: "outbe_getOcompLysisOpeningsV1",
                                    detail: error.to_string(),
                                })
                        }) {
                        Ok(openings) => openings,
                        Err(error) if is_lysis_opening_capacity_error(&error) => {
                            return Ok(OpeningResolutionV1::Split);
                        }
                        Err(error) => {
                            return Err(OpeningStageError::Resolver(error.to_string()));
                        }
                    };
                    verify_lysis_openings(&openings, finalized, subjects, &self.config.limits)
                        .map_err(|error| OpeningStageError::Resolver(error.to_string()))?;
                    on_progress();
                    let materialized = materialize_authenticated_openings(
                        &openings,
                        self.config.protocol_bundle.bundle(),
                        &self.config.limits,
                    )
                    .map_err(|error| OpeningStageError::Resolver(error.to_string()))?;
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
        pin: RetainedTributePin,
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
            input_lease_id: pin.input_lease_id,
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
    tribute_source: &FinalizedTributeSource,
    pin: RetainedTributePin,
    inventory_root: &std::path::Path,
    inventory_subject: TributeInventorySubjectV1,
    on_progress: &impl Fn(),
) -> Result<SealedTributeInventory, RpcInputExporterErrorV1> {
    let expected_count = inventory_subject.expected_tribute_count;
    let expected_nominal = inventory_subject.expected_nominal_total;
    let mut builder = TributeInventoryBuilder::create(
        inventory_root,
        inventory_subject,
        TributeInventoryWorkConfig::default(),
    )
    .map_err(|error| stage("create Tribute inventory", error))?;
    let mut stream = tribute_source
        .reconstruction_stream(pin, expected_count, expected_nominal)
        .map_err(|error| stage("open sealed Tribute stream", error))?;
    let mut records_since_progress = 0_u64;
    while let Some(record) = stream
        .next_record()
        .map_err(|error| stage("read sealed Tribute stream", error))?
    {
        builder
            .push(TributeInventoryRecordV1 {
                tribute_id: record.tribute_id,
                commitment: Commitment::try_from(record.commitment.0)
                    .map_err(|error| stage("decode Tribute commitment", error))?,
                owner: record.body.owner,
                reference_iso: record.body.reference_currency,
                nominal_amount_minor: record.body.nominal_amount_minor,
                canonical_body: record.canonical_body,
            })
            .map_err(|error| stage("spool Tribute inventory", error))?;
        records_since_progress = records_since_progress.saturating_add(1);
        if records_since_progress == EXPORT_PROGRESS_RECORD_HEARTBEAT {
            on_progress();
            records_since_progress = 0;
        }
    }
    stream
        .finish()
        .map_err(|error| stage("close sealed Tribute stream", error))?;
    builder
        .finish_observing(on_progress)
        .map_err(|error| stage("seal Tribute inventory", error))
}
