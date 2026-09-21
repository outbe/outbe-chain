use super::*;

#[derive(Clone)]
struct ProofReturningSource {
    inner: DeterministicProofSource,
    proofs: BTreeMap<B256, FinalizedIntentProofV1>,
}

impl FinalizedInputProofSource for ProofReturningSource {
    fn candidate_for_finalized_observation(
        &self,
        frame: &FinalizedFrame,
        observation: FinalizedRequestObservationV1,
    ) -> Result<CandidatePinV1, RetentionError> {
        self.inner
            .candidate_for_finalized_observation(frame, observation)
    }

    fn terminal_height_at_finalized_frame(
        &self,
        frame: &FinalizedFrame,
        candidate: CandidatePinV1,
        job_id: B256,
    ) -> Result<Option<u64>, RetentionError> {
        self.inner
            .terminal_height_at_finalized_frame(frame, candidate, job_id)
    }

    fn build_finalized_intent_proof(
        &self,
        candidate: CandidatePinV1,
    ) -> Result<FinalizedIntentProofV1, RetentionError> {
        self.proofs
            .get(&candidate.block_hash)
            .cloned()
            .ok_or_else(|| {
                RetentionError::Source("missing finalized-intent proof fixture".to_owned())
            })
    }
}

fn rebind_intent_worldwide_day(intent: &mut JobIntentV1, worldwide_day: u32) {
    intent.wwd = worldwide_day;
    intent.activation_preconditions.tribute.wwd = worldwide_day;
    intent.activation_preconditions.nod.wwd = worldwide_day;
    intent.activation_preconditions.contributors.worldwide_day = worldwide_day;
    intent.activation_preconditions.metadosis.wwd = worldwide_day;
}

fn unverified_proof(candidate: CandidatePinV1, intent: &JobIntentV1) -> FinalizedIntentProofV1 {
    let limits = poc_schema_limits();
    FinalizedIntentProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        fork_id: intent.fork_id,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        canonical_request_header_rlp: ProofBytes(Vec::new()),
        parent_accounting: CertifiedParentAccountingMetadataV2 {
            finalized_block_number: candidate.block_number,
            finalized_block_hash: candidate.block_hash,
            finalized_epoch: 0,
            finalized_view: 0,
            parent_view: 0,
            ordered_committee: Vec::new(),
            signer_bitmap: BoundedBytes(Vec::new()),
            canonical_commonware_finalization_proof: ProofBytes(Vec::new()),
            committee_set_hash: B256::ZERO,
            vrf_material_version: 0,
            vrf_group_public_key_hash: B256::ZERO,
            proof_kind: ParentProofKind::Finalization,
            missed_proposers: Vec::new(),
        },
        historical_committee_membership_proof: ProofBytes(Vec::new()),
        canonical_job_intent: BoundedBytes(
            intent
                .encode_canonical(&limits)
                .expect("fixture intent must encode"),
        ),
        intent_account_proof: ProofBytes(Vec::new()),
        intent_storage_proof: ProofBytes(Vec::new()),
    }
}

#[test]
fn shared_retention_selector_fails_closed_then_delegates_without_rebinding() {
    let selector = SharedOcompRetentionSelector::new();
    let day = WorldwideDay::new(17);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, day),
        Err("OCOMP retention coordinator is not installed".to_owned())
    );

    let request = block(100, B256::repeat_byte(0x31), 0x32);
    let candidate = candidate(&request);
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("first journal root");
    let storage = Arc::new(MemoryStorage::default());
    let coordinator = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage)),
    ));
    selector
        .install(coordinator.clone())
        .expect("first coordinator installs");

    assert!(FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &request).is_ok());
    let selected_day = WorldwideDay::new(candidate.wwd);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, selected_day).unwrap(),
        Some(RetainedTributePin {
            input_lease_id: candidate.input_lease_id,
            worldwide_day: selected_day,
        })
    );

    let replacement_root = tempfile::tempdir().expect("replacement journal root");
    let replacement_storage = Arc::new(MemoryStorage::default());
    let replacement = Arc::new(OcompRetentionCoordinator::open_with_retained_tributes(
        replacement_root.path(),
        Arc::new(DeterministicProofSource::default()),
        Arc::new(RetainedTributeWriter::new(
            replacement_storage.clone(),
            replacement_storage,
        )),
    ));
    assert!(matches!(
        selector.install(replacement),
        Err(RetentionError::RetentionCoordinatorAlreadyInstalled)
    ));
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&selector, selected_day).unwrap(),
        Some(RetainedTributePin {
            input_lease_id: candidate.input_lease_id,
            worldwide_day: selected_day,
        })
    );
}

#[test]
fn discovery_generation_replay_and_export_ack_survive_terminal_and_release() {
    let selector = SharedOcompRetentionSelector::new();
    let request = block(100, B256::repeat_byte(0x41), 0x42);
    let candidate = candidate(&request);
    let job_id = job_id_from_intent_id(
        candidate.intent_id,
        candidate.block_hash,
        candidate.state_root,
    )
    .expect("fixture JobId");
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let coordinator = Arc::new(OcompRetentionCoordinator::open(root.path(), source.clone()));
    selector
        .install(Arc::clone(&coordinator))
        .expect("coordinator installs");

    assert!(FinalizedFrameDriver::admit(source.as_ref(), coordinator.as_ref(), &request).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), coordinator.as_ref(), &request);
    let finalized = selector
        .discovery_job_records(job_id)
        .expect("finalized discovery generation");
    assert_eq!(finalized.len(), 1);
    assert_eq!(finalized[0].1.candidate, candidate);
    let source_generation = finalized[0].0;

    let exported = coordinator
        .record_exported(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("export transition");
    assert_eq!(
        selector
            .discovery_job_records(job_id)
            .expect("exported discovery generation")
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![source_generation]
    );

    coordinator
        .observe_terminal(job_id, exported.generation, 120)
        .expect("terminal transition");
    assert_eq!(
        selector
            .discovery_job_records(job_id)
            .expect("terminal restart candidates")
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![source_generation]
    );
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 10, B256::repeat_byte(0x44))
        .is_err());
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x45))
        .is_err());
    selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("terminal restart confirms the prior durable export ACK");
    coordinator
        .release_due(184)
        .expect("release scan")
        .expect("terminal retention is due");
    selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x44))
        .expect("released restart confirms the prior durable export ACK");
    assert!(selector
        .confirm_export_ack(job_id, source_generation, 9, B256::repeat_byte(0x45))
        .is_err());
}

#[test]
fn export_authority_survives_interleaved_global_registry_generations() {
    let first_request = block(100, B256::repeat_byte(0x51), 0x52);
    let second_request = block(101, B256::repeat_byte(0x53), 0x54);
    let first_candidate = candidate(&first_request);
    let second_candidate = candidate(&second_request);
    let first_job = job_id_from_intent_id(
        first_candidate.intent_id,
        first_candidate.block_hash,
        first_candidate.state_root,
    )
    .unwrap();
    let second_job = job_id_from_intent_id(
        second_candidate.intent_id,
        second_candidate.block_hash,
        second_candidate.state_root,
    )
    .unwrap();
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job),
        (second_candidate, second_job),
    ]));
    let root = tempfile::tempdir().unwrap();
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &first_request).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), &coordinator, &first_request);
    let first_source_generation = coordinator.finalized_job_record(first_job).unwrap().0;

    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &second_request).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), &coordinator, &second_request);
    let exported = coordinator
        .record_exported(
            first_job,
            first_source_generation,
            9,
            B256::repeat_byte(0x57),
        )
        .unwrap();
    assert!(exported.generation > first_source_generation + 1);
    coordinator
        .observe_terminal(first_job, exported.generation, 120)
        .unwrap();

    assert_eq!(
        coordinator
            .discovery_job_records(first_job)
            .unwrap()
            .iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![first_source_generation]
    );
    coordinator
        .confirm_export_ack(
            first_job,
            first_source_generation,
            9,
            B256::repeat_byte(0x57),
        )
        .unwrap();
}

#[test]
fn finalized_intent_export_rejects_a_proof_for_a_different_intent() {
    let request = block(100, B256::repeat_byte(0x7a), 0x7b);
    let intended = production_intent(request.number());
    let limits = poc_schema_limits();
    let intent_id = intended.intent_id(&limits).expect("fixture IntentId");
    let candidate = candidate_for_intent(&request, &intended);
    let job_id = job_id_from_intent_id(intent_id, candidate.block_hash, candidate.state_root)
        .expect("fixture JobId");

    let mut different = intended;
    rebind_intent_worldwide_day(&mut different, 8);
    let inner = DeterministicProofSource::with_jobs([(candidate, job_id)]);
    let source = Arc::new(ProofReturningSource {
        inner: inner.clone(),
        proofs: BTreeMap::from([(
            candidate.block_hash,
            unverified_proof(candidate, &different),
        )]),
    });
    let root = tempfile::tempdir().expect("validator journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);

    FinalizedFrameDriver::admit(&inner, &coordinator, &request)
        .expect("candidate pin must become durable");
    FinalizedFrameDriver::bind(&inner, &coordinator, &request);

    assert!(
        coordinator.build_finalized_intent_proof(job_id).is_err(),
        "the exact live JobId must not export another intent's proof"
    );
}

#[test]
fn ocm_pin_001_export_terminal_release_and_generation_cas_survive_restart() {
    let request = block(100, B256::repeat_byte(0x36), 6);
    let candidate = candidate(&request);
    let job_id = fixture_job_id(candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([(candidate, job_id)]));
    let root = tempfile::tempdir().expect("journal root");
    let storage = Arc::new(MemoryStorage::default());
    let day = WorldwideDay::new(candidate.wwd);
    let tribute_id = WwdEntityId::from_day_and_digest(day, [0x57; 32]);
    let tribute = TributeData {
        tribute_id,
        owner: alloy_primitives::Address::repeat_byte(0x58),
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    };
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    repository.put(&tribute).expect("fixture current Tribute");
    let pin = RetainedTributePin {
        input_lease_id: candidate.input_lease_id,
        worldwide_day: day,
    };
    let retained_reader = RetainedTributeReader::new(storage.clone());
    let retain = retained_reader
        .plan_retain_current(pin, tribute_id)
        .expect("fixture retained copy");
    storage
        .apply_atomic(&retain)
        .expect("fixture retained transaction");
    repository
        .delete(tribute_id)
        .expect("fixture current retirement");
    let retained_writer = Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone()));
    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        retained_writer,
    );

    assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, &request).is_ok());
    FinalizedFrameDriver::bind(source.as_ref(), &coordinator, &request);
    assert_eq!(
        TributeRetentionSelector::active_pin_for(&coordinator, day).unwrap(),
        Some(pin)
    );
    assert_eq!(
        retained_reader
            .list_by_day(pin, None, 10)
            .expect("retained source before release")
            .records
            .len(),
        1
    );
    let finalized = ready_record(&coordinator);
    assert!(!coordinator.is_signable(job_id));
    assert!(coordinator.is_exportable(job_id));
    assert!(matches!(
        coordinator.record_exported(job_id, finalized.generation - 1, 9, B256::repeat_byte(0x65),),
        Err(RetentionError::StaleGeneration { .. })
    ));
    let exported = coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x65))
        .expect("exact exporter CAS");
    assert!(coordinator.is_signable(job_id));
    drop(coordinator);

    let coordinator = OcompRetentionCoordinator::open_with_retained_tributes(
        root.path(),
        source.clone(),
        Arc::new(RetainedTributeWriter::new(storage.clone(), storage.clone())),
    );
    let replayed = coordinator
        .record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x65))
        .expect("lost export response replays after restart");
    assert_eq!(replayed, exported);
    assert!(coordinator.is_signable(job_id));
    assert!(matches!(
        coordinator.record_exported(job_id, finalized.generation, 9, B256::repeat_byte(0x66)),
        Err(RetentionError::StaleGeneration { .. })
    ));
    let terminal = coordinator
        .observe_terminal(job_id, exported.generation, 120)
        .expect("terminal finality");
    assert!(matches!(
        ready_record(&coordinator).state,
        PinStateV1::Terminal {
            terminal_height: 120,
            release_height: 184,
            ..
        }
    ));
    assert!(!coordinator.is_signable(job_id));
    assert_eq!(coordinator.release_due(183).unwrap(), None);
    let released = coordinator
        .release_due(184)
        .expect("release transition")
        .expect("release is due");
    assert_eq!(released.generation, terminal.generation + 2);
    assert!(retained_reader
        .list_by_day(pin, None, 10)
        .expect("exact job retained source after release")
        .records
        .is_empty());
    drop(coordinator);

    let restarted = OcompRetentionCoordinator::open(root.path(), source);
    assert!(!restarted.is_signable(job_id));
    assert!(matches!(
        ready_record(&restarted).state,
        PinStateV1::Released {
            job_id: current,

            ..
        } if current == job_id
    ));
}

#[test]
fn ocm_pin_001_each_exported_job_remains_addressable_after_a_newer_export() {
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let second_request = block(221, B256::repeat_byte(0x32), 2);
    let first_candidate = candidate(&first_request);
    let second_candidate = candidate(&second_request);
    let first_job_id = fixture_job_id(first_candidate);
    let second_job_id = fixture_job_id(second_candidate);
    let source = Arc::new(DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (second_candidate, second_job_id),
    ]));
    let root = tempfile::tempdir().expect("multi-export journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source.clone());

    for (request, job_id, manifest_byte) in [
        (&first_request, first_job_id, 0x61),
        (&second_request, second_job_id, 0x62),
    ] {
        assert!(FinalizedFrameDriver::admit(source.as_ref(), &coordinator, request).is_ok());
        FinalizedFrameDriver::bind(source.as_ref(), &coordinator, request);
        let (generation, _) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(manifest_byte))
            .expect("job reaches exported state");
    }

    for (job_id, manifest_byte) in [(first_job_id, 0x61), (second_job_id, 0x62)] {
        let record = coordinator
            .exported_job_record(job_id)
            .expect("every live export remains addressable by JobId");
        assert!(matches!(
            record.state,
            PinStateV1::Exported {
                job_id: current,
                export,
                ..
            } if current == job_id
                && export.manifest_hash == B256::repeat_byte(manifest_byte)
        ));
    }
}

#[test]
fn ocm_pin_001_exported_record_reloads_every_live_export_by_job_id() {
    let limits = poc_schema_limits();
    let first_request = block(151, B256::repeat_byte(0x31), 1);
    let second_request = block(221, B256::repeat_byte(0x32), 2);
    let first_intent = production_intent(first_request.number());
    let mut second_intent = production_intent(second_request.number());
    rebind_intent_worldwide_day(&mut second_intent, 8);

    let make_candidate = |request: &ConsensusBlock, intent: &JobIntentV1| {
        let intent_id = intent.intent_id(&limits).expect("fixture IntentId");
        CandidatePinV1 {
            block_number: request.number(),
            block_hash: request.block_hash(),
            state_root: request.header().inner.state_root,
            intent_id,
            wwd: intent.wwd,
            ce_sealed_root: intent.ce_sealed_root,
            protocol_bundle_hash: intent.protocol_bundle_hash,
            input_lease_id: intent.input_lease_id().expect("fixture input lease"),
        }
    };
    let first_candidate = make_candidate(&first_request, &first_intent);
    let second_candidate = make_candidate(&second_request, &second_intent);
    let first_job_id = first_intent
        .job_id(
            first_candidate.block_hash,
            first_candidate.state_root,
            &limits,
        )
        .expect("first JobId");
    let second_job_id = second_intent
        .job_id(
            second_candidate.block_hash,
            second_candidate.state_root,
            &limits,
        )
        .expect("second JobId");
    let inner = DeterministicProofSource::with_jobs([
        (first_candidate, first_job_id),
        (second_candidate, second_job_id),
    ]);
    let source = Arc::new(ProofReturningSource {
        inner: inner.clone(),
        proofs: BTreeMap::from([
            (
                first_candidate.block_hash,
                unverified_proof(first_candidate, &first_intent),
            ),
            (
                second_candidate.block_hash,
                unverified_proof(second_candidate, &second_intent),
            ),
        ]),
    });
    let root = tempfile::tempdir().expect("multi-export journal root");
    let coordinator = OcompRetentionCoordinator::open(root.path(), source);

    for (request, job_id, manifest_byte) in [
        (&first_request, first_job_id, 0x61),
        (&second_request, second_job_id, 0x62),
    ] {
        FinalizedFrameDriver::admit(&inner, &coordinator, request)
            .expect("candidate pin must become durable");
        FinalizedFrameDriver::bind(&inner, &coordinator, request);
        let (generation, _) = coordinator
            .finalized_job_record(job_id)
            .expect("job reaches finalized state");
        coordinator
            .record_exported(job_id, generation, 9, B256::repeat_byte(manifest_byte))
            .expect("job reaches exported state");
    }

    for job_id in [first_job_id, second_job_id] {
        let record = coordinator
            .exported_job_record(job_id)
            .expect("every live Exported job must remain independently addressable");
        assert!(matches!(
            record.state,
            PinStateV1::Exported {
                job_id: current,
                ..
            } if current == job_id
        ));
    }
}
