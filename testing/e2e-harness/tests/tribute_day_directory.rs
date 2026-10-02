//! Day directories through the projection observer.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v1, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_e2e_harness::world::projection::ProjectionFixture;
use outbe_nod::{NodContract, NodItemState, NodRepositoryWriter};
use outbe_offchain_data::{
    DayDatabaseRoute, FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection,
    ProjectionConfig, TributeRetentionSelector,
};
use outbe_offchain_storage::DayDatabases;
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    canonical_body, precompile::ITribute, RetainedTributePin, RetainedTributeWriter, TributeData,
};

struct Store {
    databases: Arc<DayDatabases>,
    shared: Arc<outbe_offchain_storage::RocksDbStorage>,
}

fn open(root: &std::path::Path) -> Store {
    let databases = Arc::new(DayDatabases::open(root).unwrap());
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    Store { databases, shared }
}

fn projection(store: &Store, pin: Option<RetainedTributePin>) -> OffchainDataProjection {
    let config = ProjectionConfig {
        chain_id: 91,
        genesis_hash: B256::repeat_byte(0x91),
        start_block: 5,
    };
    let mut projection = match pin {
        Some(pin) => OffchainDataProjection::open_with_retention_selector(
            config,
            store.shared.clone(),
            store.shared.clone(),
            Arc::new(FixedPin(pin)),
        )
        .unwrap(),
        None => OffchainDataProjection::open(config, store.shared.clone(), store.shared.clone())
            .unwrap(),
    };
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

fn stored_log(body: &TributeData) -> FinalizedLog {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    let commitment = B256::from(
        *body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .unwrap()
        .as_bytes(),
    );
    FinalizedLog {
        log_index: 0,
        emitter: TRIBUTE_ADDRESS,
        data: ITribute::TributeBodyStored {
            tributeId: body.tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: commitment,
            canonicalPayload: Bytes::from(payload),
        }
        .encode_log_data(),
    }
}

fn block(number: u64, tx: u8, logs: Vec<FinalizedLog>) -> FinalizedBlock {
    FinalizedBlock {
        number,
        hash: B256::repeat_byte(number as u8),
        receipts: vec![FinalizedReceipt {
            tx_hash: B256::repeat_byte(tx),
            transaction_index: 0,
            success: true,
            logs,
        }],
    }
}

fn tx(byte: u8) -> String {
    format!("{:#x}", B256::repeat_byte(byte))
}

fn nod(owner: Address, day: u32) -> NodItemState {
    let worldwide_day = WorldwideDay::new(day);
    let entry = U256::from(13u64);
    NodItemState {
        is_settled: false,
        nod_id: NodContract::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        gratis_load_minor: U256::from(11u64),
        worldwide_day,
        league_id: 4,
        floor_price_minor: NodContract::floor_price_minor(entry).unwrap(),
        bucket_key: NodContract::bucket_key(worldwide_day, entry, 840),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
    }
}

#[test]
fn two_days_retire_the_first_and_keep_its_nod() {
    let root = tempfile::tempdir().unwrap();
    let store = open(root.path());
    let mut projection = projection(&store, None);
    let first = tribute(Address::repeat_byte(0x11), 7);
    let second = tribute(Address::repeat_byte(0x12), 9);
    projection
        .project_block(&block(5, 0x44, vec![stored_log(&first)]))
        .unwrap();
    projection
        .project_block(&block(6, 0x45, vec![stored_log(&second)]))
        .unwrap();
    NodRepositoryWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
    .put_nod(&nod(Address::repeat_byte(0x11), 7))
    .unwrap();
    projection
        .project_block(&block(
            7,
            0x46,
            vec![FinalizedLog {
                log_index: 0,
                emitter: TRIBUTE_ADDRESS,
                data: ITribute::TributePartitionRetired { worldwideDay: 7 }.encode_log_data(),
            }],
        ))
        .unwrap();

    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert!(store.databases.directory().tribute_day_path(9).exists());
    assert!(store.databases.directory().nod_day_path(7).exists());
    let observed = ProjectionFixture::observe_offchain_tribute(root.path(), &tx(0x45)).unwrap();
    assert_eq!(observed.raw_id, second.tribute_id);
    assert!(ProjectionFixture::observe_offchain_tribute(root.path(), &tx(0x44)).is_err());
}

#[test]
fn restart_with_drop_pending_finishes_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let store = open(root.path());
    let mut projector = projection(&store, None);
    let body = tribute(Address::repeat_byte(0x13), 8);
    projector
        .project_block(&block(5, 0x47, vec![stored_log(&body)]))
        .unwrap();
    outbe_tribute::write_tribute_day_mark(
        store.shared.as_ref(),
        8,
        outbe_tribute::TributeDayMark::DropPending,
    )
    .unwrap();
    drop(projector);
    drop(store);

    let store = open(root.path());
    let _projector = projection(&store, None);
    assert!(!store.databases.directory().tribute_day_path(8).exists());
    assert!(ProjectionFixture::observe_offchain_tribute(root.path(), &tx(0x47)).is_err());
}

#[test]
fn pinned_day_survives_until_lease_release() {
    let root = tempfile::tempdir().unwrap();
    let store = open(root.path());
    let day = WorldwideDay::new(7);
    let pin = RetainedTributePin {
        input_lease_id: B256::repeat_byte(0x51),
        worldwide_day: day,
    };
    let mut projection = projection(&store, Some(pin));
    let body = tribute(Address::repeat_byte(0x16), 7);
    projection
        .project_block(&block(5, 0x48, vec![stored_log(&body)]))
        .unwrap();
    projection
        .project_block(&block(
            6,
            0x49,
            vec![FinalizedLog {
                log_index: 0,
                emitter: TRIBUTE_ADDRESS,
                data: ITribute::TributePartitionRetired { worldwideDay: 7 }.encode_log_data(),
            }],
        ))
        .unwrap();
    assert!(store.databases.directory().tribute_day_path(7).exists());
    let observed = ProjectionFixture::observe_offchain_tribute(root.path(), &tx(0x48)).unwrap();
    assert_eq!(observed.raw_id, body.tribute_id);

    let released = RetainedTributeWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
    .release_input_lease_page(pin.input_lease_id)
    .unwrap();
    assert!(released);
    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert!(ProjectionFixture::observe_offchain_tribute(root.path(), &tx(0x48)).is_err());
}
