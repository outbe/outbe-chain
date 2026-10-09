//! Finalized projection through the legacy day-directory adapter.

#[path = "../../fixtures/tribute_day_projection.rs"]
mod tribute_day_projection;
use tribute_day_projection::{open, projection, stored_log, tribute, Store};

use std::path::Path;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_nod::{NodItemState, NodRepositoryWriter};
use outbe_offchain_data::{FinalizedBlock, FinalizedLog, FinalizedReceipt};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    precompile::ITribute, RetainedTributePin, RetainedTributeWriter, TributeData,
    TributeRepositoryReader,
};

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

fn reader(store: &Store<&Path>) -> TributeRepositoryReader {
    TributeRepositoryReader::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
}

fn assert_projected(store: &Store<&Path>, body: &TributeData, transaction: u8) {
    let (record, metadata) = reader(store)
        .get_with_metadata(body.tribute_id)
        .unwrap()
        .expect("projected Tribute");
    assert_eq!(record.tribute_id, body.tribute_id);
    assert_eq!(record.owner, body.owner);
    assert_eq!(record.worldwide_day, body.worldwide_day);
    assert_eq!(
        metadata.unwrap().get("tx_hash"),
        Some(tx(transaction).as_str())
    );
}

fn nod(owner: Address, day: u32) -> NodItemState {
    let worldwide_day = WorldwideDay::new(day);
    let entry = U256::from(13u64);
    NodItemState {
        is_settled: false,
        nod_id: outbe_nod::identity::generate_nod_id(owner, worldwide_day).unwrap(),
        owner,
        encrypted: outbe_nod::test_support::encrypted_fixture(
            &outbe_nod::NodIssueParams {
                owner,
                gratis_load_minor: U256::from(11u64),
                worldwide_day,
                league_id: 4,
                entry_price_minor: entry,
                issuance_currency: 840,
                reference_currency: 840,
            },
            91,
        ),
        worldwide_day,
        league_id: 4,
        bucket_key: outbe_nod::identity::bucket_key(worldwide_day, entry, 840),
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_752_534_000,
    }
}

#[test]
fn two_days_retire_the_first_and_keep_its_nod() {
    let root = tempfile::tempdir().unwrap();
    let store = open(root.path());
    let mut projection = projection(&store, 5, None);
    let first = tribute(Address::repeat_byte(0x11), 7);
    let second = tribute(Address::repeat_byte(0x12), 9);
    projection
        .project_block(&block(5, 0x44, vec![stored_log(&first)]))
        .unwrap();
    projection
        .project_block(&block(6, 0x45, vec![stored_log(&second)]))
        .unwrap();
    outbe_nod::nod_writer(store.shared.clone(), store.shared.clone())
        .with_days(store.databases.clone())
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
    assert_projected(&store, &second, 0x45);
    assert!(reader(&store).get(first.tribute_id).unwrap().is_none());
}

#[test]
fn restart_with_drop_pending_finishes_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let store = open(root.path());
    let mut projector = projection(&store, 5, None);
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
    let _projector = projection(&store, 5, None);
    assert!(!store.databases.directory().tribute_day_path(8).exists());
    assert!(reader(&store).get(body.tribute_id).unwrap().is_none());
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
    let mut projection = projection(&store, 5, Some(pin));
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
    assert_projected(&store, &body, 0x48);

    let released = RetainedTributeWriter::with_days(
        store.shared.clone(),
        store.shared.clone(),
        store.databases.clone(),
    )
    .release_input_lease_page(pin.input_lease_id)
    .unwrap();
    assert!(released);
    assert!(!store.databases.directory().tribute_day_path(7).exists());
    assert!(reader(&store).get(body.tribute_id).unwrap().is_none());
}
