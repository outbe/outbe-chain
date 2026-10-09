#[path = "../../../../testing/fixtures/nod_day_item.rs"]
mod nod_day_item;
#[path = "../../../../testing/fixtures/tribute_day_projection.rs"]
mod tribute_day_projection;
use nod_day_item::nod_day_item;
use tribute_day_projection::tribute_body::tribute_commitment as commitment;
use tribute_day_projection::{projection, stored_log, tribute, Store};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_offchain_data::{
    DayDatabaseRoute, FinalizedBlock, FinalizedLog, FinalizedReceipt, ProjectionConfig,
    ProjectionError, PROJECTION_STATE_KEY, PROJECTION_STATE_NAMESPACE,
};
use outbe_offchain_storage::{Key, Namespace, ScanRequest, StorageReader, StorageWriter};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    precompile::ITribute, read_tribute_day_mark, write_tribute_day_mark, RetainedTributePin,
    RetainedTributeReader, RetainedTributeWriter, TributeDayMark, TributeRepositoryReader,
};

fn open() -> Store<tempfile::TempDir> {
    tribute_day_projection::open(tempfile::tempdir().unwrap())
}

fn retired_log(day: u32, index: u64) -> FinalizedLog {
    FinalizedLog {
        log_index: index,
        emitter: TRIBUTE_ADDRESS,
        data: ITribute::TributePartitionRetired { worldwideDay: day }.encode_log_data(),
    }
}

fn block(number: u64, logs: Vec<FinalizedLog>) -> FinalizedBlock {
    FinalizedBlock {
        number,
        hash: B256::repeat_byte(number as u8),
        receipts: vec![FinalizedReceipt {
            tx_hash: B256::repeat_byte(0x44),
            transaction_index: 0,
            success: true,
            logs,
        }],
    }
}

#[test]
fn retirement_drops_tribute_day_and_keeps_nod_day() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let owner = Address::repeat_byte(0x11);
    let body = tribute(owner, 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    outbe_nod::nod_writer(store.shared.clone(), store.shared.clone())
        .with_days(store.databases.clone())
        .put_nod(&nod_day_item(owner, 7, U256::from(5)))
        .unwrap();

    projection
        .project_block(&block(6, vec![retired_log(7, 0)]))
        .unwrap();

    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert!(store.databases.directory().nod_day_path(7).exists());
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 6);
    let page = StorageReader::scan_prefix(
        store.shared.as_ref(),
        Namespace::new("tributes").unwrap(),
        ScanRequest::new(&[], None, 10).unwrap(),
    )
    .unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        Some(TributeDayMark::Retired)
    );
}

#[test]
fn replay_after_day_commit_rewrites_the_same_day() {
    let store = open();
    let owner = Address::repeat_byte(0x12);
    let body = tribute(owner, 7);
    let mut first = projection(&store, 5, None);
    first
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    StorageWriter::delete(
        store.shared.as_ref(),
        Namespace::new(PROJECTION_STATE_NAMESPACE).unwrap(),
        &Key::new(PROJECTION_STATE_KEY.to_vec()).unwrap(),
    )
    .unwrap();

    let mut replay = projection(&store, 5, None);
    replay
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();

    let day = store.databases.tribute_if_present(7).unwrap().unwrap();
    let loaded = TributeRepositoryReader::new(day)
        .get(body.tribute_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.tribute_id, body.tribute_id);
    assert_eq!(replay.state().checkpoint.unwrap().block_number, 5);
}

#[test]
fn replay_after_drop_reaches_checkpoint() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let body = tribute(Address::repeat_byte(0x13), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    write_tribute_day_mark(store.shared.as_ref(), 7, TributeDayMark::DropPending).unwrap();
    store.databases.forget_tribute_day(7).unwrap();
    store.databases.directory().drop_tribute_day(7).unwrap();

    projection
        .project_block(&block(6, vec![retired_log(7, 0)]))
        .unwrap();

    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 6);
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        Some(TributeDayMark::Retired)
    );
}

#[test]
fn open_sweeps_drop_pending_directory() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let body = tribute(Address::repeat_byte(0x14), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    write_tribute_day_mark(store.shared.as_ref(), 7, TributeDayMark::DropPending).unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());

    let mut restarted = outbe_offchain_data::open_projection(
        ProjectionConfig {
            chain_id: 91,
            genesis_hash: B256::repeat_byte(0x91),
            start_block: 5,
        },
        store.shared.clone(),
        store.shared.clone(),
    )
    .unwrap();
    restarted
        .set_day_route(DayDatabaseRoute {
            databases: store.databases.clone(),
            durable_reader: store.shared.clone(),
            durable_writer: store.shared.clone(),
        })
        .unwrap();

    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        Some(TributeDayMark::DropPending)
    );
    assert_eq!(restarted.state().checkpoint.unwrap().block_number, 5);
}

#[test]
fn unmarked_directory_stays() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let body = tribute(Address::repeat_byte(0x15), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    projection.project_block(&block(6, Vec::new())).unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        None
    );
}

#[test]
fn pinned_day_stays_until_lease_release() {
    let store = open();
    let day = WorldwideDay::new(7);
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: day,
    };
    let mut projection = projection(&store, 5, Some(pin));
    let body = tribute(Address::repeat_byte(0x16), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    projection
        .project_block(&block(6, vec![retired_log(7, 0)]))
        .unwrap();

    assert!(store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        Some(TributeDayMark::Retained(pin.input_lease_id))
    );
    let retained = RetainedTributeReader::with_days(store.shared.clone(), store.databases.clone())
        .get_current_or_retained(pin, body.tribute_id, commitment(&body))
        .unwrap()
        .unwrap();
    assert!(!retained.encode().is_empty());

    let released = RetainedTributeWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
    .release_input_lease_page(pin.input_lease_id)
    .unwrap();
    assert!(released);
    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        Some(TributeDayMark::Retired)
    );
    assert!(
        RetainedTributeReader::with_days(store.shared.clone(), store.databases.clone())
            .get_current_or_retained(pin, body.tribute_id, commitment(&body))
            .unwrap()
            .is_none()
    );
}

#[test]
fn store_after_retirement_in_the_same_block_does_not_checkpoint() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let first = tribute(Address::repeat_byte(0x17), 7);
    projection
        .project_block(&block(5, vec![stored_log(&first)]))
        .unwrap();
    let second = tribute(Address::repeat_byte(0x18), 7);
    let mut later = stored_log(&second);
    later.log_index = 1;
    let error = projection
        .project_block(&block(6, vec![retired_log(7, 0), later]))
        .unwrap_err();
    assert!(matches!(
        error,
        ProjectionError::TributeStoredAfterDayRetirement { tribute_id }
            if tribute_id == second.tribute_id
    ));
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 5);
    assert!(store.databases.directory().tribute_day_path(7).exists());
}

#[test]
fn certified_event_alone_does_not_drop() {
    let store = open();
    let mut projection = projection(&store, 5, None);
    let body = tribute(Address::repeat_byte(0x19), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    let certified = FinalizedLog {
        log_index: 0,
        emitter: TRIBUTE_ADDRESS,
        data: ITribute::CertifiedTributePartitionRetired {
            activationCallId: B256::repeat_byte(0x44),
            worldwideDay: 7,
            sourceGeneration: 1,
            sealedCollectionRoot: B256::repeat_byte(0x45),
            consumedCount: 0,
            consumedNominalTotalMinor: Vec::new().into(),
            retiredGeneration: 2,
            stateEventDigest: B256::repeat_byte(0x46),
        }
        .encode_log_data(),
    };
    projection
        .project_block(&block(6, vec![certified]))
        .unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(
        read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(),
        None
    );
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 6);
}
