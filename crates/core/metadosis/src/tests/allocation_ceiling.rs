//! Valid activation conserves the day limit. An over-ceiling Desis reservation
//! fails activation before any owner write.
use alloy_primitives::U256;
use outbe_ocomp_protocol::receipts::{desis_request_brief_hash, RequestLimitSplitReceiptV1};
use outbe_primitives::{addresses::NOD_ADDRESS, storage::StorageHandle};

use crate::fixture_kernel::{ActivationFixture, TEST_WWD};
use crate::schema::MetadosisContract;

fn over_ceiling_receipt(fixture: &ActivationFixture) -> RequestLimitSplitReceiptV1 {
    let mut receipt = fixture.request_receipt.clone();
    receipt.desis_limit_minor = fixture.result.tribute_nominal_total
        - fixture.result.conservation.lysis_allocation_minor
        + U256::from(1);
    receipt.day_limit = receipt.lysis_limit_minor + receipt.desis_limit_minor;
    receipt.desis_brief_hash = Some(
        desis_request_brief_hash(
            receipt.protocol_bundle_hash,
            receipt.wwd,
            receipt.desis_limit_minor,
            receipt.logical_anchor,
        )
        .unwrap(),
    );
    receipt
}

fn rebind_retained_receipt_hash(
    fixture: &mut ActivationFixture,
    receipt: &RequestLimitSplitReceiptV1,
) {
    let old = fixture
        .request_receipt
        .receipt_hash(&fixture.limits)
        .unwrap();
    let new = receipt.receipt_hash(&fixture.limits).unwrap();
    StorageHandle::enter(&mut fixture.provider, |s| {
        let state = MetadosisContract::new(s)
            .ocomp_fsm_states
            .get_bytes(&TEST_WWD);
        let mut bytes = state.read().unwrap();
        let hits = bytes
            .windows(32)
            .enumerate()
            .filter_map(|(at, window)| (window == old.as_slice()).then_some(at))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        bytes[hits[0]..hits[0] + 32].copy_from_slice(new.as_slice());
        state.write(&bytes).unwrap();
    });
}

fn apply_without_owner_writes(fixture: &mut ActivationFixture) -> String {
    // A storage error here would mean that activation attempted an owner write first.
    fixture.provider.fail_mutation_at_address(NOD_ADDRESS);
    let error = fixture.apply().unwrap_err();
    fixture.provider.clear_mutation_failure();
    error.to_string()
}

#[test]
fn a_green_activation_preserves_the_day_limit_and_returns_unused_lysis() {
    let mut fixture = crate::fixture_kernel::ActivationScenario::build(20, 1_010, true);
    fixture.apply().unwrap();
    let snapshot = fixture.semantic_snapshot();
    let reserved = StorageHandle::enter(&mut fixture.provider, |s| {
        outbe_desis::DesisContract::new(s)
            .pending_desis_limit_minor
            .read(&TEST_WWD)
            .unwrap()
    });
    assert_eq!(snapshot.nod.lysis_allocation_minor, U256::from(45));
    assert_eq!(snapshot.carry_over, U256::from(15));
    assert_eq!(reserved, U256::from(40));
    assert_eq!(fixture.request_receipt.day_limit, U256::from(100));
    assert_eq!(
        snapshot.nod.lysis_allocation_minor + snapshot.carry_over + reserved,
        fixture.request_receipt.day_limit
    );
    assert_eq!(snapshot.tribute_count, 0);
    assert_eq!(snapshot.tribute_nominal, U256::ZERO);
}

#[test]
fn a_hash_bound_over_ceiling_desis_limit_fails_activation_before_any_owner_write() {
    let mut fixture = crate::fixture_kernel::ActivationScenario::build(20, 1_010, true);
    let receipt = over_ceiling_receipt(&fixture);
    fixture.replace_request_receipt(&receipt);
    rebind_retained_receipt_hash(&mut fixture, &receipt);
    let before = fixture.rollback_snapshot();
    let error = apply_without_owner_writes(&mut fixture);
    assert!(
        error.contains("day allocation exceeds the nominal its tributes retired"),
        "{error}"
    );
    assert_eq!(fixture.rollback_snapshot(), before);
}

#[test]
fn a_tampered_receipt_without_its_retained_hash_is_rejected_before_any_owner_write() {
    let mut fixture = crate::fixture_kernel::ActivationScenario::build(20, 1_010, true);
    let receipt = over_ceiling_receipt(&fixture);
    fixture.replace_request_receipt(&receipt);
    let before = fixture.rollback_snapshot();
    let error = apply_without_owner_writes(&mut fixture);
    assert!(
        error.contains("OCOMP limit receipt/state mismatch"),
        "{error}"
    );
    assert_eq!(fixture.rollback_snapshot(), before);
}
