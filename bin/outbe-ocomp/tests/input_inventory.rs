#[allow(dead_code)]
mod support;

use std::fs;

use alloy_primitives::Address;
use outbe_ocomp::input_inventory::{
    open_sealed_inventory, TributeInventoryBuilder, TributeInventoryWorkConfig,
};
use support::state_tributes::state_tributes;

fn tiny_work() -> TributeInventoryWorkConfig {
    TributeInventoryWorkConfig {
        owners_per_run: 17,
        merge_fan_in: 3,
    }
}

#[test]
fn encrypted_inventory_authenticates_ciphertext_and_preserves_bytes_without_decryption() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 1, None);
    outbe_tribute::enclave_client::test_enclave::uninstall();
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    let mut invalid = fixture.records[0].clone();
    let mut body =
        outbe_primitives::entity_codec::decode_tribute_v2(&invalid.canonical_body).unwrap();
    body.encrypted_amounts[10] ^= 1;
    invalid.canonical_body = outbe_primitives::entity_codec::encode_tribute_v2(&body).unwrap();
    assert!(builder.push(invalid).is_err());
    builder.push(fixture.records[0].clone()).unwrap();
    let inventory = builder.finish().unwrap();
    let mut spool = inventory.tribute_bodies().unwrap();
    assert_eq!(
        spool.next_body(1_048_576).unwrap().unwrap(),
        fixture.records[0].canonical_body
    );
    assert!(spool.next_body(1_048_576).unwrap().is_none());
    drop(inventory);
    drop(fixture.sources);
    let inventory = open_sealed_inventory(directory.path(), fixture.subject).unwrap();
    let archive = inventory.source_proofs().unwrap();
    assert_eq!(
        outbe_primitives::entity_codec::encode_tribute_v2(
            &archive
                .record(fixture.records[0].tribute_id)
                .unwrap()
                .tribute
        )
        .unwrap(),
        fixture.records[0].canonical_body,
    );
}

#[test]
fn frozen_inventory_retains_mpt_proofs_across_external_sort_and_restart() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 33, None);
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in &fixture.records {
        builder.push(record.clone()).unwrap();
    }
    drop(builder.finish().unwrap());
    drop(fixture.sources);
    let inventory = open_sealed_inventory(directory.path(), fixture.subject).unwrap();
    let archive = inventory.source_proofs().unwrap();
    for record in fixture.records {
        let source = archive.record(record.tribute_id).unwrap();
        assert_eq!(source.tribute.context.owner, record.owner);
        assert_eq!(
            outbe_primitives::entity_codec::encode_tribute_v2(&source.tribute).unwrap(),
            record.canonical_body
        );
    }
}

#[test]
#[ignore = "capacity coverage: run with mise run e2e-capacity"]
fn inventory_streams_4097_bodies_and_disk_sorts_unique_owners() {
    const COUNT: u32 = 4097;
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), COUNT, None);
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in &fixture.records {
        builder.push(record.clone()).unwrap();
    }
    assert_eq!(builder.retention_stats().peak_owner_records, 17);
    assert_eq!(builder.retention_stats().configured_owner_record_bound, 17);
    let inventory = builder.finish().unwrap();
    assert_eq!(inventory.unique_owner_count(), u64::from(COUNT));
    assert_eq!(inventory.reference_isos(), vec![840, 978]);
    let mut cursor = inventory.owner_batches().unwrap();
    let mut owners = Vec::new();
    while let Some(batch) = cursor.next_batch(256).unwrap() {
        assert!(batch.len() <= 256);
        owners.extend(batch);
    }
    assert_eq!(owners.len(), COUNT as usize);
    assert!(owners.windows(2).all(|pair| pair[0] < pair[1]));
    let mut bodies = inventory.tribute_bodies().unwrap();
    for record in &fixture.records {
        assert_eq!(
            bodies.next_body(1_048_576).unwrap().unwrap(),
            record.canonical_body
        );
    }
    assert!(bodies.next_body(1_048_576).unwrap().is_none());
    drop(inventory);
    drop(fixture.sources);
    assert_eq!(
        open_sealed_inventory(directory.path(), fixture.subject)
            .unwrap()
            .unique_owner_count(),
        u64::from(COUNT)
    );
}

#[test]
fn incomplete_inventory_is_rebuilt_without_accepting_partial_authority() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 257, None);
    let mut interrupted = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in fixture.records.iter().take(100) {
        interrupted.push(record.clone()).unwrap();
    }
    assert!(interrupted.finish().is_err());
    let mut rebuilt = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject,
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in fixture.records {
        rebuilt.push(record).unwrap();
    }
    assert_eq!(rebuilt.finish().unwrap().unique_owner_count(), 257);
}

#[test]
fn inventory_rejects_body_field_substitution_and_source_reordering() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 2, None);
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject,
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    let mut wrong_owner = fixture.records[0].clone();
    wrong_owner.owner = Address::repeat_byte(9);
    assert!(builder.push(wrong_owner).is_err());
    let mut wrong_body = fixture.records[0].clone();
    let mut decoded =
        outbe_primitives::entity_codec::decode_tribute_v2(&wrong_body.canonical_body).unwrap();
    decoded.context.tribute_price_minor += alloy_primitives::U256::ONE;
    wrong_body.canonical_body =
        outbe_primitives::entity_codec::encode_tribute_v2(&decoded).unwrap();
    assert!(builder.push(wrong_body).is_err());
    builder.push(fixture.records[1].clone()).unwrap();
    assert!(builder.push(fixture.records[0].clone()).is_err());
}

#[test]
fn sealed_inventory_rejects_a_substituted_finalized_checkpoint() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 17, None);
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in fixture.records {
        builder.push(record).unwrap();
    }
    drop(builder.finish().unwrap());
    drop(fixture.sources);
    let mut subject = fixture.subject;
    subject.checkpoint.finalized_block_number += 1;
    assert!(open_sealed_inventory(directory.path(), subject).is_err());
}

#[test]
fn sealed_inventory_detects_body_spool_corruption() {
    let directory = support::tempdir().unwrap();
    let fixture = state_tributes(directory.path(), 17, None);
    let mut builder = TributeInventoryBuilder::create(
        directory.path(),
        fixture.subject.clone(),
        tiny_work(),
        &fixture.sources,
    )
    .unwrap();
    for record in fixture.records {
        builder.push(record).unwrap();
    }
    drop(builder.finish().unwrap());
    drop(fixture.sources);
    let spool = directory.path().join("tributes.spool");
    let mut bytes = fs::read(&spool).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(spool, bytes).unwrap();
    assert!(open_sealed_inventory(directory.path(), fixture.subject).is_err());
}
