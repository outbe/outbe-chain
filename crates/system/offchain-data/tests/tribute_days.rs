use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_nod::{NodContract, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::{
    DayDatabaseRoute, FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection,
    ProjectionConfig, ProjectionError, TributeRetentionSelector, PROJECTION_STATE_KEY,
    PROJECTION_STATE_NAMESPACE,
};
use outbe_offchain_storage::{
    DayDatabases, Key, Namespace, RocksDbStorage, ScanRequest, StorageReader, StorageWriter,
};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    canonical_body, precompile::ITribute, read_tribute_day_mark, write_tribute_day_mark,
    RetainedTributePin, RetainedTributeReader, RetainedTributeWriter, TributeData, TributeDayMark,
    TributeRepositoryReader,
};

struct Store {
    _dir: tempfile::TempDir,
    databases: Arc<DayDatabases>,
    shared: Arc<RocksDbStorage>,
}

fn open() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let databases = Arc::new(DayDatabases::open(dir.path()).unwrap());
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    Store {
        _dir: dir,
        databases,
        shared,
    }
}

fn projection(store: &Store, start: u64) -> OffchainDataProjection {
    let mut projection = OffchainDataProjection::open(
        ProjectionConfig {
            chain_id: 91,
            genesis_hash: B256::repeat_byte(0x91),
            start_block: start,
        },
        store.shared.clone(),
        store.shared.clone(),
    )
    .unwrap();
    projection
        .set_day_route(DayDatabaseRoute {
            databases: store.databases.clone(),
            durable_reader: store.shared.clone(),
            durable_writer: store.shared.clone(),
        })
        .unwrap();
    projection
}

fn projection_with_pin(store: &Store, pin: RetainedTributePin) -> OffchainDataProjection {
    let mut projection = OffchainDataProjection::open_with_retention_selector(
        ProjectionConfig {
            chain_id: 91,
            genesis_hash: B256::repeat_byte(0x91),
            start_block: 5,
        },
        store.shared.clone(),
        store.shared.clone(),
        Arc::new(FixedPin(pin)),
    )
    .unwrap();
    projection
        .set_day_route(DayDatabaseRoute {
            databases: store.databases.clone(),
            durable_reader: store.shared.clone(),
            durable_writer: store.shared.clone(),
        })
        .unwrap();
    projection
}

struct FixedPin(RetainedTributePin);

impl TributeRetentionSelector for FixedPin {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        Ok((self.0.worldwide_day == worldwide_day).then_some(self.0))
    }
}

fn tribute(owner: Address, day: u32) -> TributeData {
    let tribute_id = derive_poseidon_entity_id(owner, WorldwideDay::new(day)).unwrap();
    TributeData {
        tribute_id,
        owner,
        worldwide_day: WorldwideDay::new(day),
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: true,
    }
}

fn commitment(body: &TributeData) -> B256 {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .unwrap()
        .as_bytes(),
    )
}

fn stored_log(body: &TributeData) -> FinalizedLog {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    FinalizedLog {
        log_index: 0,
        emitter: TRIBUTE_ADDRESS,
        data: ITribute::TributeBodyStored {
            tributeId: body.tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: commitment(body),
            canonicalPayload: Bytes::from(payload),
        }
        .encode_log_data(),
    }
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

fn nod(owner: Address, day: u32) -> NodItemState {
    let worldwide_day = WorldwideDay::new(day);
    let floor = U256::from(13u64);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11u64),
        worldwide_day,
        league_id: 4,
        floor_price_minor: floor,
        bucket_key: NodContract::bucket_key(worldwide_day, floor, 840),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
    }
}

#[test]
fn retirement_drops_tribute_day_and_keeps_nod_day() {
    let store = open();
    let mut projection = projection(&store, 5);
    let owner = Address::repeat_byte(0x11);
    let body = tribute(owner, 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    NodRepositoryWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
    .put_nod(&nod(owner, 7))
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
    let mut first = projection(&store, 5);
    first
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    StorageWriter::delete(
        store.shared.as_ref(),
        Namespace::new(PROJECTION_STATE_NAMESPACE).unwrap(),
        &Key::new(PROJECTION_STATE_KEY.to_vec()).unwrap(),
    )
    .unwrap();

    let mut replay = projection(&store, 5);
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
    let mut projection = projection(&store, 5);
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
    let mut projection = projection(&store, 5);
    let body = tribute(Address::repeat_byte(0x14), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    write_tribute_day_mark(store.shared.as_ref(), 7, TributeDayMark::DropPending).unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());

    let mut restarted = OffchainDataProjection::open(
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
    let mut projection = projection(&store, 5);
    let body = tribute(Address::repeat_byte(0x15), 7);
    projection
        .project_block(&block(5, vec![stored_log(&body)]))
        .unwrap();
    projection.project_block(&block(6, Vec::new())).unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(), None);
}

#[test]
fn pinned_day_stays_until_lease_release() {
    let store = open();
    let day = WorldwideDay::new(7);
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: day,
    };
    let mut projection = projection_with_pin(&store, pin);
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
    let mut projection = projection(&store, 5);
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
    let mut projection = projection(&store, 5);
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
            consumedNominalTotal: U256::ZERO,
            retiredGeneration: 2,
            stateEventDigest: B256::repeat_byte(0x46),
        }
        .encode_log_data(),
    };
    projection
        .project_block(&block(6, vec![certified]))
        .unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());
    assert_eq!(read_tribute_day_mark(store.shared.as_ref(), 7).unwrap(), None);
    assert_eq!(projection.state().checkpoint.unwrap().block_number, 6);
}
