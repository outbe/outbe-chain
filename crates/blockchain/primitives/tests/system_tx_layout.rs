//! V2 begin-zone system-tx layout invariants.
//!
//! Covers AC1/AC3/AC4 + the block-1 mandatory BoundaryOutcome rule + the
//! block-0 prohibition of begin-zone system txs. The tests check codec-level
//! rejection of V1 input bytes at every legacy selector + version combination.

use alloy_consensus::{SignableTransaction as _, TxLegacy};
use alloy_primitives::{address, Bytes, Signature, TxKind, U256};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::system_tx::*;
use reth_ethereum::TransactionSigned;

/// Empty layout - used by the block-1 / block-0 layout tests that only need to
/// drive the membership check, not the structural splitter.
fn empty_layout() -> SystemTxLayout<'static> {
    SystemTxLayout {
        begin: Vec::new(),
        user: Vec::new(),
        end: Vec::new(),
    }
}

const TEST_BLOCK: u64 = 32;
const TEST_CHAIN_ID: u64 = 2026;

fn system_inputs() -> Vec<SystemTx> {
    vec![
        SystemTx::CertifiedParentAccounting {
            metadata: CertifiedParentAccountingMetadata::default(),
        },
        SystemTx::LateFinalizeCredits {
            artifact: Default::default(),
        },
        SystemTx::CycleTick,
        SystemTx::OracleSlashWindow,
        SystemTx::HookEvents,
    ]
}

fn signed_system_tx(input: SystemTx, ordinal: u8) -> TransactionSigned {
    build_unsigned_system_tx(
        ordinal,
        TEST_BLOCK,
        TEST_CHAIN_ID,
        input.encode().expect("system input encodes"),
    )
    .expect("system tx builds")
    .into_signed(Signature::test_signature())
    .into()
}

fn user_tx() -> TransactionSigned {
    TxLegacy {
        chain_id: Some(TEST_CHAIN_ID),
        nonce: 0,
        gas_price: 0,
        gas_limit: 21_000,
        to: TxKind::Call(address!("4444444444444444444444444444444444444444")),
        value: U256::ZERO,
        input: Bytes::new(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

/// AC4: active selectors are present and have their fork-specific byte
/// sequences.
#[test]
fn active_selectors_have_expected_byte_sequence() {
    assert_eq!(
        CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
        [0x4f, 0x53, 0x41, 0x33]
    ); // "OSA3"
    assert_eq!(CYCLE_TICK_SELECTOR, [0x4f, 0x53, 0x43, 0x32]); // "OSC2"
    assert_eq!(BOUNDARY_OUTCOME_SELECTOR, [0x4f, 0x53, 0x42, 0x32]); // "OSB2"
    assert_eq!(ORACLE_SLASH_WINDOW_SELECTOR, [0x4f, 0x53, 0x4f, 0x32]); // "OSO2"
    assert_eq!(SYSTEM_TX_INPUT_VERSION, 2);
}

/// AC4 + INV1: V2 selectors must differ from V1 byte sequences.
#[test]
fn v2_selectors_differ_from_legacy_v1_selectors() {
    let v1_selectors: [[u8; 4]; 4] = [
        *b"OSF1", // legacy FinalizationAndSlashing
        *b"OSC1", // legacy CycleTick
        *b"OSB1", // legacy BoundaryOutcome
        *b"OSO1", // legacy OracleSlashWindow
    ];
    let v2_selectors: [[u8; 4]; 4] = [
        CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
        CYCLE_TICK_SELECTOR,
        BOUNDARY_OUTCOME_SELECTOR,
        ORACLE_SLASH_WINDOW_SELECTOR,
    ];
    for v1 in v1_selectors {
        assert!(
            !v2_selectors.contains(&v1),
            "V2 selector set must not contain any V1 selector bytes: {v1:?}"
        );
    }
}

/// The decoder rejects legacy V1 system-tx input bytes at every height.
///
/// This test proves two failure modes:
/// - (a) The V2 selector parser does not know the V1 selector bytes (`OSF1` /
///   `OSC1` / `OSB1` / `OSO1`).
/// - (b) Even if a caller padded an unknown body with version byte `1`, the
///   decoder rejects it because `SYSTEM_TX_INPUT_VERSION == 2`.
#[test]
fn legacy_v1_system_tx_rejected_at_all_heights() {
    let v1_selectors_and_bodies: [(&[u8; 4], &[u8]); 4] = [
        (b"OSF1", b""), // FinalizationAndSlashing carried metadata; selector alone suffices for rejection.
        (b"OSC1", b""),
        (b"OSB1", b""),
        (b"OSO1", b""),
    ];
    for (selector, body) in v1_selectors_and_bodies {
        let mut bytes = Vec::with_capacity(5 + body.len());
        bytes.extend_from_slice(selector);
        bytes.push(1); // V1 version
        bytes.extend_from_slice(body);
        let err = SystemTx::decode(&bytes).expect_err("V1 selector must be rejected");
        match err {
            SystemTxError::UnknownSelector(actual) => {
                assert_eq!(
                    actual, *selector,
                    "decoder must surface the unknown V1 selector"
                );
            }
            other => panic!("expected UnknownSelector, got {other:?}"),
        }
    }

    // Also assert: a V2 selector with the legacy version byte `1` is rejected.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&CYCLE_TICK_SELECTOR);
    bytes.push(1); // wrong (legacy) version
    let err = SystemTx::decode(&bytes)
        .expect_err("V1 version byte must be rejected even under a V2 selector");
    assert!(
        matches!(err, SystemTxError::UnsupportedVersion(1)),
        "{err:?}"
    );
}

/// Block 1 under V2 must carry a BoundaryOutcome system tx (genesis bootstrap).
#[test]
fn block_1_layout_requires_boundary_outcome_for_v2() {
    let err = validate_active_system_tx_set(&empty_layout(), 1, false, false)
        .expect_err("block 1 without BoundaryOutcome must be rejected under V2");
    assert!(
        matches!(err, SystemTxError::V2Block1MissingBoundaryOutcome),
        "{err:?}",
    );

    // Expected kinds at block 1 with BoundaryOutcome present:
    let expected = expected_begin_block_selectors(1, true, false);
    assert_eq!(
        expected,
        vec![
            CYCLE_TICK_SELECTOR,
            BOUNDARY_OUTCOME_SELECTOR,
            ORACLE_SLASH_WINDOW_SELECTOR,
            HOOK_EVENTS_SELECTOR,
        ],
        "block 1 V2 layout must include CycleTick, BoundaryOutcome, OracleSlashWindow, HookEvents",
    );
}

/// Block 0 must not carry any begin-zone system tx under V2.
#[test]
fn block_0_has_no_begin_zone_system_txs_under_v2() {
    // Membership check accepts an empty layout at block 0 regardless of
    // `has_boundary_outcome` (genesis cannot be a boundary parent of itself).
    validate_active_system_tx_set(&empty_layout(), 0, false, false)
        .expect("empty layout at block 0 must be accepted under V2");
    validate_active_system_tx_set(&empty_layout(), 0, true, false)
        .expect("empty layout at block 0 must be accepted even if has_boundary_outcome is true");

    let expected = expected_begin_block_selectors(0, false, false);
    assert!(
        expected.is_empty(),
        "block 0 must have no expected begin-zone system tx kinds; got {expected:?}",
    );
    let expected_with_bo = expected_begin_block_selectors(0, true, false);
    assert!(
        expected_with_bo.is_empty(),
        "block 0 must have no expected begin-zone system tx kinds even with has_boundary_outcome=true; got {expected_with_bo:?}",
    );
}

fn canonical_transactions() -> Vec<TransactionSigned> {
    system_inputs()
        .into_iter()
        .enumerate()
        .map(|(index, kind)| signed_system_tx(kind, index as u8))
        .chain(std::iter::once(user_tx()))
        .collect()
}

#[test]
fn native_layout_separates_begin_prefix_and_user_transactions() {
    let txs = canonical_transactions();
    let layout = split_system_layout(&txs).unwrap();
    validate_active_system_tx_set(&layout, TEST_BLOCK, false, false).unwrap();
    assert_eq!(
        layout.begin_block_selectors().unwrap(),
        expected_begin_block_selectors(TEST_BLOCK, false, false)
    );
    assert_eq!(layout.user.len(), 1);
    assert!(layout.end.is_empty());
}

#[test]
fn native_layout_accepts_an_empty_user_zone() {
    let mut txs = canonical_transactions();
    txs.pop();
    let layout = split_system_layout(&txs).unwrap();
    validate_active_system_tx_set(&layout, TEST_BLOCK, false, false).unwrap();
    assert!(layout.user.is_empty());
    assert!(layout.end.is_empty());
}

/// V2 begin-zone ordering: CertifiedParentAccounting (>=2), CycleTick (>=1),
/// BoundaryOutcome (when present),
/// OracleSlashWindow (>=1).
#[test]
fn v2_begin_zone_ordering_is_canonical() {
    // Block 2+ canonical layout (no BoundaryOutcome on the parent).
    let expected = expected_begin_block_selectors(2, false, false);
    assert_eq!(
        expected,
        vec![
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            LATE_FINALIZE_CREDITS_SELECTOR,
            CYCLE_TICK_SELECTOR,
            ORACLE_SLASH_WINDOW_SELECTOR,
            HOOK_EVENTS_SELECTOR,
        ],
    );

    // Block 2+ with BoundaryOutcome present (e.g. epoch boundary).
    let expected = expected_begin_block_selectors(42, true, false);
    assert_eq!(
        expected,
        vec![
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            LATE_FINALIZE_CREDITS_SELECTOR,
            CYCLE_TICK_SELECTOR,
            BOUNDARY_OUTCOME_SELECTOR,
            ORACLE_SLASH_WINDOW_SELECTOR,
            HOOK_EVENTS_SELECTOR,
        ],
    );

    // All begin-zone kinds resolve to BeginBlock body zone.
    let mut inputs = system_inputs();
    inputs.push(SystemTx::BoundaryOutcome {
        artifact: outbe_primitives::test_utils::sample_system_tx_boundary(TEST_BLOCK),
    });
    for kind in inputs {
        assert_eq!(kind.body_zone(), BodyZone::BeginBlock);
        assert!(kind.begin_order().is_some());
        assert!(kind.end_order().is_none());
    }
}
