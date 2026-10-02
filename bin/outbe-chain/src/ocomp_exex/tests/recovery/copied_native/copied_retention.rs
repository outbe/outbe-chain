use super::*;
use alloy_primitives::{Address, Log};
use alloy_sol_types::SolEvent as _;
use outbe_compressed_entities::WwdEntityId;
use outbe_consensus::finalization::parent_cert_store::FinalizedParentCertStore;
use outbe_metadosis::{precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT};
use outbe_node::{
    finalized_frame::FinalizedFrame,
    ocomp::retention::{
        inspect_retention_journal, observe_finalized_request, read_ocomp_job_record_at,
        OcompRetentionCoordinator, PinRecordV1, PinStateV1, RethFinalizedInputProofSource,
    },
};
use outbe_ocomp_protocol::{
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::intent_storage_key,
    state::{LysisTerminalV1, OcompFinalizedJobV1, OcompJobRecordV1, OcompTerminalOutcome},
};
use outbe_offchain_storage::{
    AtomicWriteBatch, RocksDbStorage, StorageError, StorageWriter, StorageWriterHandle,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    storage::{
        hashmap::HashMapStorageProvider,
        types::{StorageBytes, StorageKey},
        StorageHandle,
    },
    time::WorldwideDay,
};
use outbe_tribute::{
    RetainedTributePin, RetainedTributeReader, RetainedTributeWriter, TributeData,
    TributeRepositoryWriter,
};
use reth_primitives_traits::StorageEntry;
use std::sync::atomic::{AtomicBool, Ordering};

const H: u64 = 170;

fn journal(root: &Path) -> std::path::PathBuf {
    root.join("ocomp_retention")
}

fn body_store(root: &Path) -> Arc<RocksDbStorage> {
    Arc::new(RocksDbStorage::open(root.join("retained-projection")).unwrap())
}

// The only fault seam forwards the real RocksDB atomic delete, then reports
// an unavailable result once, simulating an ambiguous committed write.
// The coordinator has already durably published GcPending at this point.
struct FailAfterCommittedDelete {
    storage: Arc<RocksDbStorage>,
    armed: AtomicBool,
}

impl StorageWriter for FailAfterCommittedDelete {
    fn apply_atomic(&self, batch: &AtomicWriteBatch) -> Result<(), StorageError> {
        self.storage.apply_atomic(batch)?;
        if self.armed.swap(false, Ordering::SeqCst) {
            return Err(StorageError::Unavailable {
                source: Box::new(std::io::Error::other("injected after committed GC page")),
            });
        }
        Ok(())
    }
}

fn owner(
    root: &Path,
    storage: Arc<RocksDbStorage>,
    writer: StorageWriterHandle,
) -> OcompRetentionCoordinator {
    // Construct the concrete public factory here: the helper's opaque return
    // type does not expose HeaderProvider<Header = OutbeHeader> to this caller.
    let factory = ProviderFactoryBuilder::<outbe_node::OutbeNode>::default()
        .open_read_only(
            chain(),
            ReadOnlyConfig::from_datadir(root).no_watch(),
            reth_ethereum::tasks::Runtime::test(),
        )
        .unwrap();
    assert!(!reth_provider::StorageSettingsCache::cached_storage_settings(&factory).is_v2());
    let source = RethFinalizedInputProofSource::new(
        reth_provider::providers::BlockchainProvider::new(factory).unwrap(),
        FinalizedParentCertStore::new(),
    );
    OcompRetentionCoordinator::open_with_retained_tributes(
        journal(root),
        Arc::new(source),
        Arc::new(RetainedTributeWriter::new(storage, writer)),
    )
}

fn frame(root: &Path, height: u64) -> FinalizedFrame {
    let provider = provider(root);
    let hash = provider.block_hash(height).unwrap().unwrap();
    let source = RethFinalizedFrameSource::new(provider);
    let batch = read_bounded_finalized_frames(&source, height, (height, hash).into())
        .unwrap()
        .unwrap();
    assert_eq!(batch.frames().len(), 1);
    batch.frames()[0].clone()
}

fn current_record(root: &Path, expected: &OcompJobRecordV1, height: u64) -> OcompJobRecordV1 {
    let provider = provider(root);
    let hash = provider.block_hash(height).unwrap().unwrap();
    let actual = read_ocomp_job_record_at(
        &provider,
        hash,
        expected.intent.intent_id(&poc_schema_limits()).unwrap(),
        &poc_schema_limits(),
    )
    .unwrap();
    assert_eq!(&actual, expected);
    actual
}

// Typed owner encodes the records; only these test-native words are seeded.
// They are not EVM-executed and this fixture does not authenticate state roots
// or parent certificates. All subsequent candidate and terminal reads use
// the production RethFinalizedInputProofSource and actual native provider.
fn write_records(root: &Path, records: &[OcompJobRecordV1]) {
    let mut words = HashMapStorageProvider::new_with_chain_identity(
        chain().chain().id(),
        chain().genesis_hash(),
    );
    StorageHandle::enter(&mut words, |storage| {
        for record in records {
            let limits = poc_schema_limits();
            record.validate_semantics(&limits).unwrap();
            let key = intent_storage_key(record.intent.intent_id(&limits).unwrap()).unwrap();
            StorageBytes::new(
                key.mapping_slot(U256::from(OCOMP_JOB_RECORDS_BASE_SLOT)),
                METADOSIS_ADDRESS,
                storage.clone(),
            )
            .write(&record.encode_canonical(&limits).unwrap())
            .unwrap();
        }
    });
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    tx.delete::<tables::PlainStorageState>(METADOSIS_ADDRESS, None)
        .unwrap();
    tx.put::<tables::PlainAccountState>(METADOSIS_ADDRESS, Default::default())
        .unwrap();
    for ((address, slot), value) in words.storage {
        if !value.is_zero() {
            tx.put::<tables::PlainStorageState>(
                address,
                StorageEntry {
                    key: B256::from(slot.to_be_bytes::<32>()),
                    value,
                },
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn intent(count: usize) -> JobIntentV1 {
    let spec = finalized_job_spec(0x41, 1, chain().chain().id(), chain().genesis_hash());
    let mut intent =
        JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &poc_schema_limits()).unwrap();
    let nominal = U256::from(count);
    intent.authenticated_day_count = u32::try_from(count).unwrap();
    intent.authenticated_day_nominal = nominal;
    intent.activation_preconditions.tribute.exact_count = u32::try_from(count).unwrap();
    intent.activation_preconditions.tribute.exact_nominal_total = nominal;
    intent.activation_preconditions.nod.max_nod_count = u32::try_from(count).unwrap();
    intent
        .activation_preconditions
        .contributors
        .max_contributor_count = u32::try_from(count).unwrap();
    intent
        .activation_preconditions
        .contributors
        .max_eligible_nominal_total = nominal;
    intent.frozen_metadosis_values.day_limit = nominal;
    intent.frozen_metadosis_values.lysis_limit_minor = nominal;
    intent.validate_semantics().unwrap();
    intent
}

// Amend only the just-created tip before any successor/C exists, preserving
// the exact receipt commitment. This is fixture construction, not recovery.
fn request_tip(root: &Path, height: u64, intent: &JobIntentV1) -> ProjectionCheckpoint {
    let event = IMetadosis::OffchainJobRequested {
        intentId: intent.intent_id(&poc_schema_limits()).unwrap(),
        wwd: intent.wwd,
        pendingNonce: intent.pending_nonce,
        attempt: intent.attempt,
        activationPreconditionsHash: intent
            .activation_preconditions
            .activation_preconditions_hash(&poc_schema_limits())
            .unwrap(),
    };
    let receipt = OutbeReceipt {
        success: true,
        cumulative_gas_used: 21_000,
        logs: vec![Log {
            address: METADOSIS_ADDRESS,
            data: event.encode_log_data(),
        }],
        ..Default::default()
    };
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    assert_eq!(
        tx.get::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock)
            .unwrap(),
        Some(height)
    );
    let old = tx.get::<tables::CanonicalHeaders>(height).unwrap().unwrap();
    let mut header = tx
        .get::<tables::Headers<OutbeHeader>>(height)
        .unwrap()
        .unwrap();
    header.inner.receipts_root = alloy_consensus::proofs::calculate_receipt_root(&[
        alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
    ]);
    let hash = header.hash_slow();
    tx.delete::<tables::HeaderNumbers>(old, None).unwrap();
    tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
    tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
    // Pinned Reth reads headers from static files, not the MDBX mirror.
    // Replace only this unobserved donor tip during fixture construction.
    let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
        .with_blocks_per_file(1_000)
        .build::<OutbePrimitives>()
        .unwrap();
    {
        let mut headers = files
            .get_writer(height, StaticFileSegment::Headers)
            .unwrap();
        headers.prune_headers(1).unwrap();
    }
    files.commit().unwrap();
    {
        let mut headers = files
            .get_writer(height, StaticFileSegment::Headers)
            .unwrap();
        headers.append_header(&header, &hash).unwrap();
    }
    files.commit().unwrap();
    drop(files);
    tx.put::<tables::Headers<OutbeHeader>>(height, header)
        .unwrap();
    tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
        .unwrap();
    tx.commit().unwrap();
    ProjectionCheckpoint {
        block_number: height,
        block_hash: hash,
    }
}

fn register(root: &Path, records: &mut Vec<OcompJobRecordV1>, intent: JobIntentV1, height: u64) {
    if height == 1 {
        write_frames(root, 0, 1);
    } else {
        write_frames(root, height, height);
    }
    let point = request_tip(root, height, &intent);
    records.push(OcompJobRecordV1 {
        intent,
        intent_height: height,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    });
    write_records(root, records);
    let request = frame(root, height);
    let observation = observe_finalized_request(&request)
        .unwrap()
        .expect("native receipt request");
    {
        let storage = body_store(root);
        let coordinator = owner(root, storage.clone(), storage);
        coordinator
            .reconcile_finalized_frame(&request, Some(observation))
            .unwrap();
    }
    let record = records.last_mut().unwrap();
    let job_id = record
        .intent
        .job_id(point.block_hash, request.state_root(), &poc_schema_limits())
        .unwrap();
    record.status = OcompJobStatus::VotingOpen;
    record.finalized = Some(OcompFinalizedJobV1 {
        job_id,
        finalized_request_block_hash: point.block_hash,
        finalized_request_state_root: request.state_root(),
        finality_recorded_height: height + 1,
        open_height: height + 5,
        deadline_height: 20,
        quorum: None,
    });
    write_records(root, records);
    let record = current_record(root, records.last().unwrap(), height);
    let storage = body_store(root);
    let coordinator = owner(root, storage.clone(), storage);
    coordinator
        .bind_canonical_finalized_job(point.block_hash, &record)
        .unwrap();
}

fn expire(record: &mut OcompJobRecordV1) {
    record.status = OcompJobStatus::Expired;
    record.terminal = Some(LysisTerminalV1 {
        outcome: OcompTerminalOutcome::Expired,
        terminal_height: 20,
        terminal_time: 20,
        completed_binding: None,
    });
}

fn reconcile_tip(root: &Path, height: u64) {
    let finalized = frame(root, height);
    let storage = body_store(root);
    let coordinator = owner(root, storage.clone(), storage);
    coordinator
        .reconcile_finalized_frame(&finalized, None)
        .unwrap();
}

fn pin(record: &OcompJobRecordV1) -> RetainedTributePin {
    RetainedTributePin {
        input_lease_id: record.intent.input_lease_id().unwrap(),
        worldwide_day: WorldwideDay::new(record.intent.wwd),
    }
}

fn retain(root: &Path, record: &OcompJobRecordV1, count: usize) {
    let storage = body_store(root);
    let repository = TributeRepositoryWriter::new(storage.clone(), storage.clone());
    let retained = RetainedTributeReader::new(storage.clone());
    for ordinal in 0..count {
        let mut digest = [0u8; 32];
        digest[..8].copy_from_slice(&(ordinal as u64).to_be_bytes());
        let tribute_id = WwdEntityId::from_day_and_digest(pin(record).worldwide_day, digest);
        repository
            .put(&TributeData {
                tribute_id,
                owner: Address::repeat_byte(0x52),
                worldwide_day: pin(record).worldwide_day,
                issuance_amount_minor: U256::ONE,
                issuance_currency: 840,
                nominal_amount_minor: U256::ONE,
                reference_currency: 978,
                tribute_price_minor: U256::ONE,
                exclude_from_intex_issuance: false,
            })
            .unwrap();
        storage
            .apply_atomic(
                &retained
                    .plan_retain_current(pin(record), tribute_id)
                    .unwrap(),
            )
            .unwrap();
        repository.delete(tribute_id).unwrap();
    }
}

fn remaining(root: &Path, record: &OcompJobRecordV1) -> usize {
    RetainedTributeReader::new(body_store(root))
        .list_by_day(pin(record), None, 1_024)
        .unwrap()
        .records
        .len()
}

fn durable_record(root: &Path, record: &OcompJobRecordV1) -> PinRecordV1 {
    let key = record
        .finalized
        .as_ref()
        .unwrap()
        .finalized_request_block_hash;
    inspect_retention_journal(journal(root))
        .unwrap()
        .records
        .into_iter()
        .find_map(|(candidate, record)| (candidate == key).then_some(record))
        .unwrap()
}

fn close_through(root: &Path, height: u64) -> ProjectionCheckpoint {
    let provider = provider(root);
    let point = ProjectionCheckpoint {
        block_number: height,
        block_hash: provider.block_hash(height).unwrap().unwrap(),
    };
    let mut runtime = runtime(provider, root, bundle());
    let old = runtime.closure_checkpoint.current().unwrap();
    let visited = catch_up(&mut runtime, point);
    assert_eq!(visited, (old.block_number + 1..=height).collect::<Vec<_>>());
    assert_eq!(runtime.closure_checkpoint.current().unwrap(), point);
    point
}

fn assert_c(root: &Path, expected: ProjectionCheckpoint) {
    let runtime = runtime(provider(root), root, bundle());
    assert_eq!(runtime.closure_checkpoint.current().unwrap(), expected);
}

#[test]
fn copied_rocksdb_partial_and_empty_gc_pending_resume_without_reconstructing_bodies() {
    for empty_after_committed_write in [false, true] {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let page = OCOMP_POC_CANDIDATE_LIMITS_V1.max_tributes_per_work_shard as usize;
        let count = if empty_after_committed_write {
            1
        } else {
            page + 1
        };
        let mut records = Vec::new();
        register(donor.path(), &mut records, intent(count), 1);
        retain(donor.path(), &records[0], count);
        assert_eq!(remaining(donor.path(), &records[0]), count);
        write_frames(donor.path(), 2, 100);
        expire(&mut records[0]);
        write_records(donor.path(), &records);
        reconcile_tip(donor.path(), 100);
        close_through(donor.path(), 100);
        // Actual terminal observation at100 makes release due at164.
        assert!(matches!(
            durable_record(donor.path(), &records[0]).state,
            PinStateV1::Terminal {
                terminal_height: 100,
                release_height: 164,
                ..
            }
        ));
        write_frames(donor.path(), 101, H);
        let closed = close_through(donor.path(), H);
        {
            let storage = body_store(donor.path());
            let writer: StorageWriterHandle = if empty_after_committed_write {
                Arc::new(FailAfterCommittedDelete {
                    storage: storage.clone(),
                    armed: AtomicBool::new(true),
                })
            } else {
                storage.clone()
            };
            let coordinator = owner(donor.path(), storage, writer);
            let result = coordinator.release_due(closed.block_number);
            if empty_after_committed_write {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_none());
            }
        }
        assert!(matches!(
            durable_record(donor.path(), &records[0]).state,
            PinStateV1::GcPending { .. }
        ));
        let expected_remaining = usize::from(!empty_after_committed_write);
        assert_eq!(remaining(donor.path(), &records[0]), expected_remaining);
        copy_tree(donor.path(), receiver.path());
        donor.close().unwrap();
        assert!(receiver
            .path()
            .join("retained-projection/CURRENT")
            .is_file());
        assert_c(receiver.path(), closed);
        assert_eq!(remaining(receiver.path(), &records[0]), expected_remaining);
        assert!(matches!(
            durable_record(receiver.path(), &records[0]).state,
            PinStateV1::GcPending { .. }
        ));
        {
            let storage = body_store(receiver.path());
            let coordinator = owner(receiver.path(), storage.clone(), storage);
            assert!(coordinator
                .release_due(closed.block_number)
                .unwrap()
                .is_some());
        }
        assert_eq!(remaining(receiver.path(), &records[0]), 0);
        assert!(matches!(
            durable_record(receiver.path(), &records[0]).state,
            PinStateV1::Released { .. }
        ));
        write_frames(receiver.path(), H + 1, H + 2);
        let k = close_through(receiver.path(), H + 2);
        assert_c(receiver.path(), k);
        {
            let storage = body_store(receiver.path());
            let coordinator = owner(receiver.path(), storage.clone(), storage);
            assert!(coordinator.release_due(k.block_number).unwrap().is_none());
        }
        assert_eq!(remaining(receiver.path(), &records[0]), 0);
        assert!(matches!(
            durable_record(receiver.path(), &records[0]).state,
            PinStateV1::Released { .. }
        ));
    }
}

#[test]
fn copied_rocksdb_shared_live_lease_survives_first_release_and_collects_after_last_job_at_k() {
    exercise_shared_lease_copy(false);
}

#[test]
fn copied_pruned_released_pin_stays_absent_while_shared_lease_remains_live() {
    exercise_shared_lease_copy(true);
}

// Construct an already-compacted native image, not a pressure-compaction
// execution. Keep every surviving record byte and the latest generation.
fn remove_old_released_frame(root: &Path, key: B256) {
    let mut expected = inspect_retention_journal(journal(root)).unwrap();
    assert_ne!(expected.last_updated, key);
    let removed = expected
        .records
        .iter()
        .find(|(id, _)| *id == key)
        .unwrap()
        .1;
    assert!(matches!(removed.state, PinStateV1::Released { .. }));
    expected.records.retain(|(id, _)| *id != key);
    assert!(!expected.records.is_empty());
    let path = journal(root).join("pin.v1");
    let bytes = fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"OUTBPIN1");
    assert_eq!(u16::from_be_bytes(bytes[8..10].try_into().unwrap()), 6);
    let count = u16::from_be_bytes(bytes[50..52].try_into().unwrap());
    let mut output = bytes[..50].to_vec();
    output.extend_from_slice(&(count - 1).to_be_bytes());
    let mut offset = 52;
    let mut removed_count = 0;
    for _ in 0..count {
        let id = B256::from_slice(&bytes[offset..offset + 32]);
        let len = u16::from_be_bytes(bytes[offset + 32..offset + 34].try_into().unwrap()) as usize;
        let end = offset + 34 + len;
        if id == key {
            removed_count += 1;
        } else {
            output.extend_from_slice(&bytes[offset..end]);
        }
        offset = end;
    }
    assert_eq!(removed_count, 1);
    assert_eq!(offset, bytes.len() - 32);
    let checksum = alloy_primitives::keccak256(&output);
    output.extend_from_slice(checksum.as_slice());
    fs::write(path, output).unwrap();
    assert_eq!(inspect_retention_journal(journal(root)).unwrap(), expected);
}

fn exercise_shared_lease_copy(pruned: bool) {
    let donor = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    let first = intent(1);
    let mut second = first.clone();
    // Different output precondition, same authenticated source opening.
    // PoC attempt/pending_nonce remain their required zero values.
    second.activation_preconditions.nod.namespace_root_before = B256::repeat_byte(0x91);
    assert_eq!(
        first.input_lease_id().unwrap(),
        second.input_lease_id().unwrap()
    );
    assert_ne!(
        first.intent_id(&poc_schema_limits()).unwrap(),
        second.intent_id(&poc_schema_limits()).unwrap()
    );
    let mut records = Vec::new();
    register(donor.path(), &mut records, first, 1);
    register(donor.path(), &mut records, second, 2);
    retain(donor.path(), &records[0], 1);
    write_frames(donor.path(), 3, 100);
    expire(&mut records[0]);
    write_records(donor.path(), &records);
    reconcile_tip(donor.path(), 100);
    close_through(donor.path(), 100);
    write_frames(donor.path(), 101, H);
    let closed = close_through(donor.path(), H);
    {
        let storage = body_store(donor.path());
        let coordinator = owner(donor.path(), storage.clone(), storage);
        assert!(coordinator
            .release_due(closed.block_number)
            .unwrap()
            .is_some());
    }
    assert!(matches!(
        durable_record(donor.path(), &records[0]).state,
        PinStateV1::Released { .. }
    ));
    assert!(matches!(
        durable_record(donor.path(), &records[1]).state,
        PinStateV1::Finalized { .. }
    ));
    assert_eq!(remaining(donor.path(), &records[1]), 1);
    let retired_key = records[0]
        .finalized
        .as_ref()
        .unwrap()
        .finalized_request_block_hash;
    let closed = if pruned {
        // A later ordinary transition preserves a live lease and makes
        // the old Released entry eligible for an absent-record fixture.
        write_frames(donor.path(), H + 1, 180);
        expire(&mut records[1]);
        write_records(donor.path(), &records);
        reconcile_tip(donor.path(), 180);
        let point = close_through(donor.path(), 180);
        remove_old_released_frame(donor.path(), retired_key);
        point
    } else {
        closed
    };
    copy_tree(donor.path(), receiver.path());
    donor.close().unwrap();
    assert_c(receiver.path(), closed);
    assert_eq!(remaining(receiver.path(), &records[1]), 1);
    {
        let storage = body_store(receiver.path());
        let coordinator = owner(receiver.path(), storage.clone(), storage);
        assert!(coordinator
            .release_due(closed.block_number)
            .unwrap()
            .is_none());
    }
    assert_eq!(remaining(receiver.path(), &records[1]), 1);
    if !pruned {
        write_frames(receiver.path(), H + 1, 180);
        expire(&mut records[1]);
        write_records(receiver.path(), &records);
        reconcile_tip(receiver.path(), 180);
        close_through(receiver.path(), 180);
    } else {
        assert!(inspect_retention_journal(journal(receiver.path()))
            .unwrap()
            .records
            .iter()
            .all(|(key, _)| *key != retired_key));
    }
    assert!(matches!(
        durable_record(receiver.path(), &records[1]).state,
        PinStateV1::Terminal {
            terminal_height: 180,
            release_height: 244,
            ..
        }
    ));
    write_frames(receiver.path(), 181, 250);
    let k = close_through(receiver.path(), 250);
    {
        let storage = body_store(receiver.path());
        let coordinator = owner(receiver.path(), storage.clone(), storage);
        assert!(coordinator.release_due(k.block_number).unwrap().is_some());
    }
    assert_eq!(remaining(receiver.path(), &records[1]), 0);
    assert_c(receiver.path(), k);
    {
        let storage = body_store(receiver.path());
        let coordinator = owner(receiver.path(), storage.clone(), storage);
        assert!(coordinator.release_due(k.block_number).unwrap().is_none());
    }
    for record in &records[usize::from(pruned)..] {
        assert!(matches!(
            durable_record(receiver.path(), record).state,
            PinStateV1::Released { .. }
        ));
    }
    if pruned {
        assert!(inspect_retention_journal(journal(receiver.path()))
            .unwrap()
            .records
            .iter()
            .all(|(key, _)| *key != retired_key));
    }
    assert_eq!(remaining(receiver.path(), &records[1]), 0);
}
