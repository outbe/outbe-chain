use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;

use super::super::validation::Incomplete;

use super::super::{
    native::RethReadOnlyView,
    validation::{
        canonical_state::CanonicalState, evm::verify_current_evm, headers::verify_retained_headers,
        ocomp::CanonicalInventory,
    },
};
use crate::OutbeHeader;
use alloy_consensus::Sealable;
use alloy_primitives::B256;
use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    tables,
    transaction::{DbTx, DbTxMut},
};
const DAY: WorldwideDay = WorldwideDay::new(20_200_101);
fn with_owner_storage(
    version: u32,
    owner: HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    with_prepared_owner_storage(version, 101, |_| owner, check);
}

fn with_prepared_owner_storage(
    version: u32,
    execution_height: u64,
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    with_prepared_owner_storage_setup(version, execution_height, |_| {}, prepare, check);
}

fn with_prepared_owner_storage_setup(
    version: u32,
    execution_height: u64,
    setup: impl FnOnce(&crate::snapshot::config::NativeLayout),
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(&CanonicalState<'_>, &RethReadOnlyView),
) {
    use crate::snapshot::tests::native_state;
    let (_source, layout, _) = super::evm::state_fixture(version);
    setup(&layout);
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx().unwrap();
    let request = tx
        .get::<tables::Headers<OutbeHeader>>(100)
        .unwrap()
        .unwrap();
    let before = native_state::read_plain(&tx);
    let mut template = tx
        .get::<tables::Headers<OutbeHeader>>(101)
        .unwrap()
        .unwrap();
    drop(tx);
    drop(db);
    let owner = prepare(&request);
    let mut current = before.clone();
    native_state::merge_owner(&mut current, &owner);
    native_state::persist(
        &layout,
        version,
        &before,
        &current,
        if execution_height > 101 { 102 } else { 101 },
    );
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    for header in [
        layout.chain.genesis_header().clone(),
        request.clone(),
        template.clone(),
    ] {
        tx.put::<tables::HeaderNumbers>(header.hash_slow(), header.inner.number)
            .unwrap();
    }
    let mut parent = if execution_height == 101 {
        request.hash_slow()
    } else {
        template.hash_slow()
    };
    template.inner.timestamp = 1_010;
    for number in (if execution_height == 101 { 101 } else { 102 })..=execution_height {
        let mut header = template.clone();
        header.inner.parent_hash = parent;
        header.inner.state_root = native_state::root(&current);
        header.inner.number = number;
        let hash = header.hash_slow();
        tx.put::<tables::CanonicalHeaders>(number, hash).unwrap();
        tx.put::<tables::HeaderNumbers>(hash, number).unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(number, header)
            .unwrap();
        parent = hash;
    }
    for stage in ["Execution", "Finish"] {
        let mut checkpoint = tx
            .get::<tables::StageCheckpoints>(stage.into())
            .unwrap()
            .unwrap();
        checkpoint.block_number = execution_height;
        tx.put::<tables::StageCheckpoints>(stage.into(), checkpoint)
            .unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    native_state::publish_history(&layout);
    let before_files = super::headers::fingerprint(_source.path());
    let scratch = tempfile::tempdir().unwrap();
    let source = RethReadOnlyView::open(&layout).unwrap();
    let verified = verify_current_evm(&source, scratch.path()).unwrap();
    let headers = verify_retained_headers(&source, &[99]).unwrap();
    assert_eq!(headers.required_missing, vec![99]);
    let state = CanonicalState::new(&verified, &source, &headers);
    check(&state, &source);
    assert_eq!(super::headers::fingerprint(_source.path()), before_files);
}

fn seed_nod_generation(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
    sequence: u64,
) -> NodCertifiedGenerationProjection {
    let generation = NodCertifiedGenerationProjection {
        source_checkpoint: outbe_ocomp_protocol::intent::FinalizedRequestBindingV1 {
            block_number: 1,
            block_hash: B256::repeat_byte(0x71),
            state_root: B256::repeat_byte(0x72),
        },
        worldwide_day: day,
        generation: sequence,
        job_id: B256::repeat_byte(1),
        program_semantics_hash: B256::repeat_byte(2),
        protocol_bundle_hash: B256::repeat_byte(3),
        nod_root: B256::repeat_byte(4),
        bucket_root: B256::repeat_byte(5),
        output_manifest_root: B256::repeat_byte(6),
        tribute_count: 3,
        nod_count: 3,
        bucket_count: 1,
        nod_amount_total: U256::from(99),
        lysis_allocation_minor: U256::from(77),
        nod_gratis_load_max: U256::from(33),
        issued_at: 1_000,
        next_nod_ordinal: 1,
        last_progress_height: 90,
    };
    let nod = NodContract::new(storage);
    nod.ocomp_source_block_number
        .write(&day, generation.source_checkpoint.block_number)
        .unwrap();
    nod.ocomp_source_block_hash
        .write(&day, generation.source_checkpoint.block_hash)
        .unwrap();
    nod.ocomp_source_state_root
        .write(&day, generation.source_checkpoint.state_root)
        .unwrap();
    nod.ocomp_target_generation
        .write(&day, generation.generation)
        .unwrap();
    nod.ocomp_namespace_root
        .write(&day, generation.nod_root)
        .unwrap();
    nod.ocomp_bucket_root
        .write(&day, generation.bucket_root)
        .unwrap();
    nod.ocomp_output_manifest_root
        .write(&day, generation.output_manifest_root)
        .unwrap();
    nod.ocomp_generation_metadata
        .write(&day, generation.metadata_word())
        .unwrap();
    nod.ocomp_nod_amount_total
        .write(&day, generation.nod_amount_total)
        .unwrap();
    nod.ocomp_lysis_allocation_minor
        .write(&day, generation.lysis_allocation_minor)
        .unwrap();
    nod.ocomp_nod_gratis_load_max
        .write(&day, generation.nod_gratis_load_max)
        .unwrap();
    nod.ocomp_materialization_job_id
        .write(&day, generation.job_id)
        .unwrap();
    nod.ocomp_materialization_protocol_bundle_hash
        .write(&day, generation.protocol_bundle_hash)
        .unwrap();
    nod.ocomp_materialization_program_semantics_hash
        .write(&day, generation.program_semantics_hash)
        .unwrap();
    nod.ocomp_materialization_next_nod_ordinal
        .write(&day, generation.next_nod_ordinal)
        .unwrap();
    nod.ocomp_materialization_last_progress_height
        .write(&day, generation.last_progress_height)
        .unwrap();
    nod.ocomp_materialization_queue_wwd
        .write(&sequence, day)
        .unwrap();
    generation
}

fn queued_owner(count: u64) -> HashMapStorageProvider {
    let mut owner = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut owner, |storage| {
        let nod = NodContract::new(storage.clone());
        nod.ocomp_materialization_head_sequence.write(1).unwrap();
        nod.ocomp_materialization_tail_sequence
            .write(count + 1)
            .unwrap();
        for sequence in 1..=count {
            let day = WorldwideDay::new(DAY.value() + sequence as u32 - 1);
            seed_nod_generation(storage.clone(), day, sequence);
            nod.ocomp_materialization_job_id
                .write(&day, B256::repeat_byte(sequence as u8))
                .unwrap();
        }
    });
    owner
}

#[test]
fn canonical_inventory_discovers_later_nod_jobs_without_any_local_job_files() {
    with_owner_storage(2, queued_owner(2), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        assert_eq!(
            (
                inventory.bounds.nod_head,
                inventory.bounds.nod_tail,
                inventory.bounds.nod_entries
            ),
            (1, 3, 2)
        );
        let mut jobs = vec![];
        inventory
            .visit_nod(&mut |sequence, projection| {
                jobs.push((sequence, projection.job_id));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            jobs,
            vec![(1, B256::repeat_byte(1)), (2, B256::repeat_byte(2))]
        );
    });
}

#[test]
fn canonical_fifo_rejects_later_holes_duplicates_complete_entries_and_bad_bounds() {
    for damage in [
        "hole",
        "duplicate-day",
        "duplicate-job",
        "complete",
        "tail",
        "bounds",
    ] {
        let mut owner = queued_owner(2);
        StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage);
            let second = WorldwideDay::new(DAY.value() + 1);
            match damage {
                "hole" => nod
                    .ocomp_materialization_queue_wwd
                    .write(&2, WorldwideDay::new(0))
                    .unwrap(),
                "duplicate-day" => nod.ocomp_materialization_queue_wwd.write(&2, DAY).unwrap(),
                "duplicate-job" => nod
                    .ocomp_materialization_job_id
                    .write(&second, B256::repeat_byte(1))
                    .unwrap(),
                "complete" => nod
                    .ocomp_materialization_next_nod_ordinal
                    .write(&second, 3)
                    .unwrap(),
                "tail" => nod.ocomp_materialization_queue_wwd.write(&3, DAY).unwrap(),
                "bounds" => nod.ocomp_materialization_head_sequence.write(4).unwrap(),
                _ => unreachable!(),
            }
        });
        with_owner_storage(2, owner, |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            assert!(
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).is_err(),
                "{damage}"
            );
        });
    }
}

#[test]
fn canonical_scan_budget_and_scratch_overlap_fail_without_source_changes() {
    with_owner_storage(2, queued_owner(2), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let error = CanonicalInventory::scan(state, scratch.path(), &source.protected, Some(1))
            .err()
            .unwrap();
        assert!(error.downcast_ref::<Incomplete>().is_some());
        assert!(error.to_string().contains("1/2"));
        let protected = outbe_snapshot::layout::ProtectedPaths(vec![scratch.path().to_path_buf()]);
        assert!(CanonicalInventory::scan(state, scratch.path(), &protected, None).is_err());
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    });
}

mod active_inventory;

mod frames_inventory;
mod nod_inventory;
// Append as a child module of snapshot::tests::ocomp.
mod lease_inventory;
mod pin_authority;

mod export_inventory;

// The existing owner fixture supplies current-E root verification and
// whole-source fingerprints.
fn with_canonical_frontiers(
    version: u32,
    setup_public: impl FnOnce(&crate::snapshot::config::RequestedLayout),
    prepare: impl FnOnce(&OutbeHeader) -> HashMapStorageProvider,
    check: impl FnOnce(
        &CanonicalState<'_>,
        &RethReadOnlyView,
        &crate::snapshot::config::RequestedLayout,
        &std::path::Path,
    ),
) {
    use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
    use outbe_primitives::projection::ProjectionCheckpoint;
    use reth_ethereum::provider::db::models::StoredBlockBodyIndices;
    use std::cell::RefCell;
    let requested = RefCell::new(None);
    with_prepared_owner_storage_setup(
        version,
        400,
        |native| {
            let layout = native.clone();
            {
                let db = init_db(native.chain_root.join("db"), DatabaseArguments::test()).unwrap();
                let tx = db.tx_mut().unwrap();
                tx.put::<tables::BlockBodyIndices>(
                    100,
                    StoredBlockBodyIndices {
                        first_tx_num: 0,
                        tx_count: 0,
                    },
                )
                .unwrap();
                tx.commit().unwrap();
            }
            setup_public(&layout);
            let db = init_db(native.chain_root.join("db"), DatabaseArguments::test()).unwrap();
            let tx = db.tx().unwrap();
            let header = tx
                .get::<tables::Headers<OutbeHeader>>(100)
                .unwrap()
                .unwrap();
            drop(tx);
            drop(db);
            let point = ProjectionCheckpoint {
                block_number: 100,
                block_hash: header.hash_slow(),
            };
            let baseline = ProjectionCheckpoint {
                block_number: 0,
                block_hash: layout.chain.genesis_hash(),
            };
            let path = layout
                .ocomp_root
                .join("exporter-v1/discovery/closure-checkpoint-v1");
            if !path.exists() {
                let closure = ContiguousCheckpointStoreV1::open(&path, baseline).unwrap();
                closure.compare_and_advance_to(baseline, point).unwrap();
            }
            *requested.borrow_mut() = Some(layout);
        },
        prepare,
        |state, source| {
            let requested = requested.borrow();
            let layout = requested.as_ref().unwrap();
            let scratch = tempfile::tempdir().unwrap();
            check(state, source, layout, scratch.path());
            assert_eq!(
                std::fs::read_dir(scratch.path()).unwrap().count(),
                0,
                "canonical composition must release its scratch on success and error"
            );
        },
    );
}

mod canonical_composition;

mod present_cas;

mod present_join;
