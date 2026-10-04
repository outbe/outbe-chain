//! Valid activation conserves capacity; a tampered reservation cannot reach an
//! owner write. The independent result-semantic ceiling is tested by protocol.
use alloy_primitives::U256;
use outbe_ocomp_protocol::receipts::desis_request_brief_hash;
use outbe_primitives::{addresses::NOD_ADDRESS, storage::StorageHandle};

use crate::fixture_kernel::{ActivationFixture, TEST_WWD};

#[test]
fn a_green_activation_preserves_the_sealed_nominal_bound_and_returns_unused_lysis() {
    let mut fixture = ActivationFixture::new(20, 1_010, true);
    fixture.apply().unwrap();
    let snapshot = fixture.semantic_snapshot();
    let reserved = StorageHandle::enter(&mut fixture.provider, |s| {
        outbe_desis::DesisContract::new(s)
            .pending_desis_limit_minor
            .read(&TEST_WWD)
            .unwrap()
    });
    assert_eq!(snapshot.nod.lysis_allocation_minor, U256::from(45));
    assert_eq!(reserved, U256::from(40));
    assert!(snapshot.nod.lysis_allocation_minor + reserved <= U256::from(1_000));
    assert_eq!(snapshot.carry_over, U256::from(15));
    assert_eq!(snapshot.tribute_count, 0);
    assert_eq!(snapshot.tribute_nominal, U256::ZERO);
}

#[test]
fn activation_rejects_a_tampered_over_limit_receipt_before_any_owner_write() {
    let mut fixture = ActivationFixture::new(20, 1_010, true);
    // The result remains valid (45 + 40 <= 1000). Deliberately corrupt the
    // persisted request receipt. Its retained hash must reject the corruption
    // before owner effects, without assuming semantic admission can be bypassed.
    let mut receipt = fixture.request_receipt.clone();
    receipt.desis_limit_minor = U256::from(956);
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
    fixture.replace_request_receipt(&receipt);
    let before = fixture.rollback_snapshot();
    // If even the first Nod owner write is attempted, it fails with a storage
    // error. The retained-receipt mismatch must be returned first.
    fixture.provider.fail_mutation_at_address(NOD_ADDRESS);
    let error = fixture.apply().unwrap_err();
    fixture.provider.clear_mutation_failure();
    assert!(
        error
            .to_string()
            .contains("OCOMP limit receipt/state mismatch"),
        "{error}"
    );
    assert_eq!(fixture.rollback_snapshot(), before);
}
