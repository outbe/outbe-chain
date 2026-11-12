//! Stateless V3 layout / version / fork validator integration tests.
//!
//! Pins the contract that `crates/blockchain/execution/src/consensus.rs::OutbeBeaconConsensus`
//! is a **stateless** V3 validator. It rejects these envelopes **without**
//! running any stateful BLS / VRF / accounting verification:
//! - legacy V1 envelopes, at every height.
//! - malformed V3 envelopes (wrong version byte, unknown selector, missing
//!   body-index 0 for block N >= 2, missing `BoundaryOutcome` for block 1, any
//!   system tx for block 0).
//!
//! Those stateful checks live exclusively in
//! `OutbeBlockExecutor::apply_pre_execution_changes`.
//!
//! Most tests drive the public stateless entry point
//! `outbe_execution::consensus::validate_system_tx_consensus_boundary(body, header, &TEST_BLOCK_ORDER)`
//! directly. Regression tests that depend on upstream Reth validation drive
//! `OutbeBeaconConsensus::validate_block_pre_execution` with a sealed block.

use alloy_consensus::{Block as AlloyBlock, Header, Transaction as _, TxLegacy};
use alloy_primitives::{Address, Bloom, Bytes, TxKind, B256, B64, U256};
use outbe_evm::system_tx::TEST_BLOCK_ORDER;
use outbe_evm::OutbeEvmSigner;
use outbe_execution::consensus::{validate_system_tx_consensus_boundary, OutbeBeaconConsensus};
use outbe_primitives::addresses::{OUTBE_SYSTEM_TX_ADDRESS, REWARDS_ADDRESS};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};
use outbe_primitives::system_tx::SysTxSelector;
use outbe_primitives::system_tx::{
    build_unsigned_system_tx, SystemTx, BEGIN_BLOCK_STAGE_SELECTOR,
    CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
    CONSENSUS_STAGE_SELECTOR, CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX, END_BLOCK_BASE_ORDINAL,
    END_BLOCK_STAGE_SELECTOR, SYSTEM_TX_ARTIFACT_GAS_LIMIT, SYSTEM_TX_INPUT_VERSION,
};
use outbe_primitives::{OutbeBlockBody, OutbeHeader};
use reth_chainspec::ChainSpecBuilder;
use reth_ethereum::{consensus::Consensus, TransactionSigned};
use reth_primitives_traits::{proofs::calculate_transaction_root, Block as _};
use std::sync::Arc;

const TEST_CHAIN_ID: u64 = 1;
const TEST_GAS_LIMIT: u64 = 0;

fn signer() -> OutbeEvmSigner {
    OutbeEvmSigner::from_secret_bytes([7u8; 32]).expect("valid test signer")
}

fn header_for(number: u64, parent_hash: B256) -> OutbeHeader {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default())
        .expect("encode empty artifacts");
    OutbeHeader::new(Header {
        parent_hash,
        beneficiary: if number == 0 {
            Address::ZERO
        } else {
            REWARDS_ADDRESS
        },
        state_root: B256::ZERO,
        transactions_root: B256::ZERO,
        receipts_root: B256::ZERO,
        withdrawals_root: None,
        logs_bloom: Bloom::default(),
        number,
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: 100 + number,
        mix_hash: B256::ZERO,
        base_fee_per_gas: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
        requests_hash: None,
        block_access_list_hash: None,
        slot_number: None,
        extra_data,
        ommers_hash: alloy_consensus::EMPTY_OMMER_ROOT_HASH,
        difficulty: U256::ZERO,
        nonce: B64::ZERO,
    })
}

fn body(transactions: Vec<TransactionSigned>) -> OutbeBlockBody {
    OutbeBlockBody {
        transactions,
        ommers: Vec::new(),
        withdrawals: None,
    }
}

fn header_for_transactions(
    number: u64,
    parent_hash: B256,
    transactions: &[TransactionSigned],
) -> OutbeHeader {
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default())
        .expect("encode empty artifacts");
    OutbeHeader::new(Header {
        parent_hash,
        beneficiary: if number == 0 {
            Address::ZERO
        } else {
            REWARDS_ADDRESS
        },
        state_root: B256::ZERO,
        transactions_root: calculate_transaction_root(transactions),
        receipts_root: B256::ZERO,
        withdrawals_root: None,
        logs_bloom: Bloom::default(),
        number,
        gas_limit: 30_000_000,
        gas_used: 0,
        timestamp: 100 + number,
        mix_hash: B256::ZERO,
        base_fee_per_gas: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
        requests_hash: None,
        block_access_list_hash: None,
        slot_number: None,
        extra_data,
        ommers_hash: alloy_consensus::EMPTY_OMMER_ROOT_HASH,
        difficulty: U256::ZERO,
        nonce: B64::ZERO,
    })
}

/// Build a signed system tx with an arbitrary `input` payload, bypassing the
/// Full calldata validation in `build_unsigned_system_tx`. Used by the
/// legacy V1 / unknown-selector / wrong-version-byte tests.
fn signed_with_raw_input(signer: &OutbeEvmSigner, nonce: u64, input: Vec<u8>) -> TransactionSigned {
    let tx = TxLegacy {
        chain_id: Some(TEST_CHAIN_ID),
        nonce,
        gas_price: 0,
        gas_limit: TEST_GAS_LIMIT,
        to: TxKind::Call(OUTBE_SYSTEM_TX_ADDRESS),
        value: U256::ZERO,
        input: Bytes::from(input),
    };
    signer.sign_unsigned(tx).expect("sign raw system tx")
}

/// Build a properly-formed V3 system tx via the canonical helper. Used by the
/// happy-path baselines and by the unknown-body / wrong-body-index tests where
/// only the layout - not the calldata - is exercised.
fn signed_system_tx(
    signer: &OutbeEvmSigner,
    ordinal: u8,
    block_number: u64,
    input: SystemTx,
) -> TransactionSigned {
    let unsigned = build_unsigned_system_tx(
        ordinal,
        block_number,
        TEST_CHAIN_ID,
        input.encode().expect("encode V3 input"),
    )
    .expect("build V3 unsigned");
    signer.sign_unsigned(unsigned).expect("sign V3 system tx")
}

fn phase1_metadata(block_number: u64, block_hash: B256) -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: block_number,
        finalized_block_hash: block_hash,
        ..Default::default()
    }
}

#[test]
fn osaka_user_tx_cap_accepts_visible_outbe_system_tx_envelopes() {
    const MAX_TX_GAS_LIMIT_OSAKA: u64 = 1 << 24;
    const {
        assert!(
            SYSTEM_TX_ARTIFACT_GAS_LIMIT > MAX_TX_GAS_LIMIT_OSAKA,
            "test must model the Outbe 100M internal system execution budget exceeding Osaka/EIP-7825"
        )
    };

    let signer = signer();
    let parent_hash = B256::with_last_byte(0xA4);
    let transactions = system_txs_block2(&signer, parent_hash, Default::default());
    for tx in &transactions {
        assert_eq!(
            tx.gas_limit(),
            0,
            "reserved Outbe system tx envelopes must carry 0 visible gas under the Tempo model"
        );
        assert!(
            tx.gas_limit() <= MAX_TX_GAS_LIMIT_OSAKA,
            "visible Outbe system tx envelope gas must stay inside Osaka/EIP-7825 user tx cap"
        );
    }

    let header = header_for_transactions(2, parent_hash, &transactions);
    let block = AlloyBlock::new(header, body(transactions)).seal_slow();
    let chain_spec = Arc::new(
        ChainSpecBuilder::mainnet()
            .with_osaka_at(0)
            .build()
            .map_header(OutbeHeader::new),
    );
    let consensus = OutbeBeaconConsensus::new(chain_spec, TEST_BLOCK_ORDER);

    let result = consensus.validate_block_pre_execution(&block);
    assert!(
        result.is_ok(),
        "GAS-04: a valid block with visible Outbe system tx envelopes must pass \
         consensus pre-execution under Osaka; got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Legacy V1 selectors are rejected at every height (no silent drop).
// ---------------------------------------------------------------------------

/// the stateless validator must reject legacy V1 envelopes
/// (`OSF1` / `OSC1` / `OSB1` / `OSO1`) at any block height. The decoder
/// raises `SystemTxError::UnknownSelector`, surfaced by
/// `validate_system_tx_consensus_boundary` as a `ConsensusError::Other`.
#[test]
fn legacy_v1_system_tx_rejected_at_all_heights() {
    let signer = signer();
    let v1_selectors: [&SysTxSelector; 4] = [b"OSF1", b"OSC1", b"OSB1", b"OSO1"];
    let heights: [u64; 5] = [0, 1, 2, 7, 100];

    for height in heights {
        for selector in v1_selectors {
            let mut input = Vec::with_capacity(6);
            input.extend_from_slice(selector);
            input.push(1); // V1 version byte
            input.push(0);
            let tx = signed_with_raw_input(&signer, 0, input);
            let body = body(vec![tx]);
            let header = header_for(height, B256::ZERO);

            let err = validate_system_tx_consensus_boundary(&body, &header, &TEST_BLOCK_ORDER)
                .expect_err("legacy V1 envelope must be rejected at every height (no silent drop)");
            let msg = err.to_string();
            assert!(
                msg.contains("system tx layout") || msg.contains("UnknownSelector"),
                "rejection must reference the system tx layout path; got: {msg}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Block 0 has no begin-zone system txs under V3.
// ---------------------------------------------------------------------------

/// block 0 must contain zero system transactions. The layout validator
/// rejects any begin-zone V3 system tx in block 0.
#[test]
fn block_0_has_no_begin_zone_system_txs_under_v3() {
    let signer = signer();
    let tx = signed_system_tx(&signer, 0, 0, SystemTx::BeginBlock { step_index: 8 });
    let body = body(vec![tx]);
    let header = header_for(0, B256::ZERO);

    let err = validate_system_tx_consensus_boundary(&body, &header, &TEST_BLOCK_ORDER)
        .expect_err("block 0 with a begin-zone system tx must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("system tx set") || msg.contains("ActiveSystemTxSetMismatch"),
        "rejection must reference the active system tx set check; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Block 1 layout requires `BoundaryOutcome` under V3.
// ---------------------------------------------------------------------------

/// a block-1 body that omits `BoundaryOutcome` is rejected
/// (V3 genesis-bootstrap rule). The error path is
/// `SystemTxError::V2Block1MissingBoundaryOutcome`.
#[test]
fn block_1_layout_requires_boundary_outcome_for_v2() {
    let signer = signer();
    let oracle = signed_system_tx(&signer, 0, 1, SystemTx::BeginBlock { step_index: 6 });
    let cycle = signed_system_tx(&signer, 1, 1, SystemTx::BeginBlock { step_index: 8 });
    let body = body(vec![oracle, cycle]);
    let header = header_for(1, B256::ZERO);

    let err = validate_system_tx_consensus_boundary(&body, &header, &TEST_BLOCK_ORDER)
        .expect_err("block 1 without BoundaryOutcome must be rejected under V3");
    let msg = err.to_string();
    assert!(
        msg.contains("V2Block1MissingBoundaryOutcome") || msg.contains("BoundaryOutcome"),
        "rejection must surface the V3 block-1 missing-boundary contract; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Block N >= 2 layout requires `CertifiedParentAccounting` at body[0].
// ---------------------------------------------------------------------------

/// for `block_number >= 2`, the first begin-zone tx must be
/// `CertifiedParentAccounting`. A block 2 body missing it, or placing it
/// elsewhere, is rejected.
#[test]
fn block_b_ge_2_layout_requires_certified_parent_accounting_body0() {
    let signer = signer();

    // (a) block 2 with only OracleSlash + CycleTick - no CertifiedParentAccounting.
    let oracle = signed_system_tx(&signer, 0, 2, SystemTx::BeginBlock { step_index: 6 });
    let cycle = signed_system_tx(&signer, 1, 2, SystemTx::BeginBlock { step_index: 8 });
    let body_missing_phase1 = body(vec![oracle, cycle]);
    let header_missing = header_for(2, B256::ZERO);
    let err_missing = validate_system_tx_consensus_boundary(
        &body_missing_phase1,
        &header_missing,
        &TEST_BLOCK_ORDER,
    )
    .expect_err("block 2 without CertifiedParentAccounting at body[0] must be rejected");
    let msg_missing = err_missing.to_string();
    assert!(
        msg_missing.contains("system tx set")
            || msg_missing.contains("ActiveSystemTxSetMismatch")
            || msg_missing.contains("CertifiedParentAccounting"),
        "rejection must reference the missing CertifiedParentAccounting; got: {msg_missing}"
    );

    // (b) block 2 with CertifiedParentAccounting present but pointing to a
    // hash that does not match the header's parent_hash - also rejected.
    let parent_hash = B256::with_last_byte(0xAA);
    let wrong_parent = B256::with_last_byte(0xBB);
    let transactions = system_txs_block2(&signer, wrong_parent, Default::default());
    let body_bad_hash = body(transactions);
    let header_bad_hash = header_for(2, parent_hash);
    let err_bad_hash = validate_system_tx_consensus_boundary(&body_bad_hash, &header_bad_hash, &TEST_BLOCK_ORDER)
        .expect_err(
            "block 2 with CertifiedParentAccounting metadata pointing at the wrong parent_hash must be rejected",
        );
    let msg_bad_hash = err_bad_hash.to_string();
    assert!(
        msg_bad_hash.contains("CertifiedParentAccounting metadata hash must match block parent"),
        "rejection must call out the parent_hash mismatch; got: {msg_bad_hash}"
    );
}

// ---------------------------------------------------------------------------
// V3 envelope with wrong version byte is rejected.
// ---------------------------------------------------------------------------

/// the decoder rejects an input whose selector is a V3 selector but whose
/// version byte is not `SYSTEM_TX_INPUT_VERSION` (e.g. legacy `0x01`) before
/// any execution.
#[test]
fn v3_envelope_with_wrong_version_byte_rejects() {
    assert_eq!(
        SYSTEM_TX_INPUT_VERSION, 3,
        "this test assumes six-byte framing uses version 3"
    );
    let signer = signer();
    let mut input = Vec::with_capacity(6);
    input.extend_from_slice(&BEGIN_BLOCK_STAGE_SELECTOR);
    input.push(1); // wrong version byte
    input.push(0);
    let tx = signed_with_raw_input(&signer, 0, input);
    let body = body(vec![tx]);
    let header = header_for(1, B256::ZERO);

    let err = validate_system_tx_consensus_boundary(&body, &header, &TEST_BLOCK_ORDER)
        .expect_err("V3 selector with V1 version byte must be rejected by the stateless validator");
    let msg = err.to_string();
    assert!(
        msg.contains("UnsupportedVersion") || msg.contains("system tx layout"),
        "rejection must reference the unsupported-version path; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// reth22-1: the v2.2 `validate_block_pre_execution_with_tx_root` entrypoint
// (the one reth's engine-tree block-insert actually calls) must run the Outbe
// system-tx boundary check, not just the legacy `validate_block_pre_execution`.
// ---------------------------------------------------------------------------

/// reth v2.2 added `Consensus::validate_block_pre_execution_with_tx_root` and
/// the engine-tree now calls it as the PRIMARY pre-execution validator.
/// `OutbeBeaconConsensus` overrides it to run `validate_system_tx_consensus_boundary`
/// before delegating to the inner Eth impl. This pins that contract. A malformed
/// system-tx block must be rejected via BOTH the new `_with_tx_root` entrypoint
/// AND the legacy one. A future bump that drops the override then fails here.
/// The other stateless tests only exercise the legacy entrypoint.
#[test]
fn malformed_block_rejected_via_v2_with_tx_root_entrypoint() {
    let signer = signer();
    // A V3 selector carrying a V1 version byte -> UnsupportedVersion at the
    // boundary check (runs before any tx-root delegation).
    let mut input = Vec::with_capacity(6);
    input.extend_from_slice(&BEGIN_BLOCK_STAGE_SELECTOR);
    input.push(1);
    input.push(0);
    let tx = signed_with_raw_input(&signer, 0, input);
    let transactions = vec![tx];
    let header = header_for_transactions(1, B256::ZERO, &transactions);
    let block = AlloyBlock::new(header, body(transactions)).seal_slow();

    let chain_spec = Arc::new(
        ChainSpecBuilder::mainnet()
            .with_osaka_at(0)
            .build()
            .map_header(OutbeHeader::new),
    );
    let consensus = OutbeBeaconConsensus::new(chain_spec, TEST_BLOCK_ORDER);

    // The v2.2 PRIMARY entrypoint must reject it. The boundary check runs before
    // the tx-root delegation, so `None` is fine - it errors first.
    let with_tx_root = consensus.validate_block_pre_execution_with_tx_root(&block, None);
    assert!(
        with_tx_root.is_err(),
        "v2.2 validate_block_pre_execution_with_tx_root must run the Outbe system-tx \
         boundary check and reject a malformed block; got {with_tx_root:?}"
    );
    // The legacy entrypoint rejects it identically.
    assert!(
        consensus.validate_block_pre_execution(&block).is_err(),
        "legacy validate_block_pre_execution must also reject the malformed block"
    );
}

/// SSA-7: the v2.2 `validate_block_pre_execution_with_tx_root` override must FORWARD
/// the caller-provided transaction_root to the inner Eth impl, not swallow it. On a
/// well-formed block (which passes the Outbe system-tx boundary check), a WRONG
/// Some(tx_root) must be rejected via the inner tx-root mismatch. The CORRECT
/// tx_root must pass. This proves that the override delegates rather than ignores
/// tx_root.
#[test]
fn with_tx_root_forwards_transaction_root_on_wellformed_block() {
    let signer = signer();
    let parent_hash = B256::with_last_byte(0xA4);
    let transactions = system_txs_block2(&signer, parent_hash, Default::default());
    // The block's true transaction root (also written into the header).
    let correct_tx_root = calculate_transaction_root(&transactions);
    let header = header_for_transactions(2, parent_hash, &transactions);
    let block = AlloyBlock::new(header, body(transactions)).seal_slow();
    let chain_spec = Arc::new(
        ChainSpecBuilder::mainnet()
            .with_osaka_at(0)
            .build()
            .map_header(OutbeHeader::new),
    );
    let consensus = OutbeBeaconConsensus::new(chain_spec, TEST_BLOCK_ORDER);

    // Sanity: this block is fully valid for pre-execution (boundary check passes).
    assert!(
        consensus.validate_block_pre_execution(&block).is_ok(),
        "fixture block must be well-formed"
    );

    // Correct tx_root: boundary passes AND the forwarded tx_root matches the header.
    assert!(
        consensus
            .validate_block_pre_execution_with_tx_root(&block, Some(correct_tx_root))
            .is_ok(),
        "well-formed block with the correct tx_root must pass _with_tx_root"
    );

    // Wrong tx_root: boundary passes, but the inner Eth impl must reject on the
    // tx-root mismatch. That is only possible if the override forwarded tx_root.
    let wrong_tx_root = B256::repeat_byte(0xEE);
    assert_ne!(wrong_tx_root, correct_tx_root);
    let err = consensus.validate_block_pre_execution_with_tx_root(&block, Some(wrong_tx_root));
    assert!(
        err.is_err(),
        "a wrong tx_root must be rejected via the inner mismatch - proving _with_tx_root \
         forwards the provided root rather than swallowing it; got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// V3 envelope with unknown selector is rejected.
// ---------------------------------------------------------------------------

/// an input whose selector is not any active selector
/// (`OSCN` / `OSBB` / `OSEB`) is rejected. The selector byte
/// constants live in `outbe_evm::system_tx` so a future selector reshuffle
/// fails here too, not just at the unit-test layer.
#[test]
fn v3_envelope_with_unknown_selector_rejects() {
    assert_eq!(CONSENSUS_STAGE_SELECTOR, *b"OSCN");
    assert_eq!(BEGIN_BLOCK_STAGE_SELECTOR, *b"OSBB");
    assert_eq!(END_BLOCK_STAGE_SELECTOR, *b"OSEB");

    let signer = signer();
    let mut input = Vec::with_capacity(6);
    input.extend_from_slice(b"ZZZZ"); // unknown 4-byte selector
    input.push(SYSTEM_TX_INPUT_VERSION);
    input.push(0);
    let tx = signed_with_raw_input(&signer, 0, input);
    let body = body(vec![tx]);
    let header = header_for(1, B256::ZERO);

    let err = validate_system_tx_consensus_boundary(&body, &header, &TEST_BLOCK_ORDER)
        .expect_err("V3 envelope with an unknown selector must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("UnknownSelector") || msg.contains("system tx layout"),
        "rejection must reference the unknown-selector path; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// LateFinalizeCredits header<->body parity + mandatory phase.
// ---------------------------------------------------------------------------

/// Build the canonical block-2 system set with a chosen LateFinalizeCredits
/// body artifact (ordinals 0..=11): CPA, LateFinalizeCredits, ValidatorLiveness,
/// and the 9 BEGIN_BLOCK_ORDER transactions, followed by the 5 END_BLOCK_ORDER transactions.
fn system_txs_block2(
    signer: &OutbeEvmSigner,
    parent_hash: B256,
    late_artifact: outbe_primitives::reshare_artifact::LateFinalizeCreditsArtifact,
) -> Vec<TransactionSigned> {
    let begin_inputs = [
        SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload: phase1_metadata(1, parent_hash).encode().unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_late_finalize_credits_artifact(
                &(late_artifact),
            )
            .unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
            payload: Default::default(),
        },
        SystemTx::BeginBlock { step_index: 0 },
        SystemTx::BeginBlock { step_index: 1 },
        SystemTx::BeginBlock { step_index: 2 },
        SystemTx::BeginBlock { step_index: 3 },
        SystemTx::BeginBlock { step_index: 4 },
        SystemTx::BeginBlock { step_index: 5 },
        SystemTx::BeginBlock { step_index: 6 },
        SystemTx::BeginBlock { step_index: 7 },
        SystemTx::BeginBlock { step_index: 8 },
    ];
    let mut transactions = begin_inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| signed_system_tx(signer, ordinal as u8, 2, input))
        .collect::<Vec<_>>();
    transactions.extend(end_zone_txs_block2(signer));
    transactions
}

fn end_zone_txs_block2(signer: &OutbeEvmSigner) -> [TransactionSigned; 5] {
    [
        signed_system_tx(
            signer,
            END_BLOCK_BASE_ORDINAL,
            2,
            SystemTx::EndBlock { step_index: 0 },
        ),
        signed_system_tx(
            signer,
            END_BLOCK_BASE_ORDINAL + 1,
            2,
            SystemTx::EndBlock { step_index: 1 },
        ),
        signed_system_tx(
            signer,
            END_BLOCK_BASE_ORDINAL + 2,
            2,
            SystemTx::EndBlock { step_index: 2 },
        ),
        signed_system_tx(
            signer,
            END_BLOCK_BASE_ORDINAL + 3,
            2,
            SystemTx::EndBlock { step_index: 3 },
        ),
        signed_system_tx(
            signer,
            END_BLOCK_BASE_ORDINAL + 4,
            2,
            SystemTx::EndBlock { step_index: 4 },
        ),
    ]
}

/// the header's `late_finalize_credits`
/// (tag 0x06, hash-committed + BLS-verified pre-exec) must equal the body's
/// `LateFinalizeCredits` system-tx calldata. A proposer placing credits in the
/// body that the (default/empty) header does not commit to is rejected.
#[test]
fn late_finalize_header_body_calldata_mismatch_fatal() {
    use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};

    let signer = signer();
    let parent_hash = B256::with_last_byte(0xA4);

    let nonempty = LateFinalizeCreditsArtifact {
        batches: vec![PerBlockCredit {
            fb_number: 1,
            fb_hash: parent_hash,
            epoch: 0,
            view: 2,
            parent_view: 1,
            committee_set_hash: B256::repeat_byte(0xEF),
            signer_bitmap: vec![0x01],
            aggregate_signature: [0u8; 96],
        }],
    };

    // Body carries non-empty credits; the header (default) commits to none.
    let txs = system_txs_block2(&signer, parent_hash, nonempty);
    let header = header_for_transactions(2, parent_hash, &txs);
    let err = validate_system_tx_consensus_boundary(&body(txs), &header, &TEST_BLOCK_ORDER)
        .expect_err("header/body late_finalize_credits mismatch must be rejected");
    assert!(
        err.to_string()
            .contains("does not match header late_finalize_credits"),
        "rejection must reference the parity check; got: {err:?}"
    );

    // Sanity: matching empty header/body passes the parity check (no false positive).
    let txs_ok = system_txs_block2(&signer, parent_hash, LateFinalizeCreditsArtifact::default());
    let header_ok = header_for_transactions(2, parent_hash, &txs_ok);
    validate_system_tx_consensus_boundary(&body(txs_ok), &header_ok, &TEST_BLOCK_ORDER)
        .expect("matching empty header/body must pass");
}

/// the LateFinalizeCredits begin-zone phase is mandatory for every
/// block >= 2. A block-2 body that drops it is rejected by the exact-set
/// validator (it cannot be silently skipped).
#[test]
fn mandatory_late_phase_cannot_be_skipped() {
    use outbe_primitives::reshare_artifact::LateFinalizeCreditsArtifact;

    let signer = signer();
    let parent_hash = B256::with_last_byte(0xA4);
    let mut txs = system_txs_block2(&signer, parent_hash, LateFinalizeCreditsArtifact::default());
    // Drop the LateFinalizeCredits tx (ordinal 1). This leaves CPA, ValidatorLiveness,
    // the 9 BEGIN_BLOCK_ORDER transactions, and the 5 END_BLOCK_ORDER transactions.
    txs.remove(1);
    let header = header_for_transactions(2, parent_hash, &txs);
    let err = validate_system_tx_consensus_boundary(&body(txs), &header, &TEST_BLOCK_ORDER)
        .expect_err("block >= 2 without LateFinalizeCredits must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("system tx set") || msg.to_lowercase().contains("latefinalize"),
        "rejection must reference the missing mandatory phase; got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// a block whose RLP size exceeds the P2P transport cap is rejected at
// verify (deterministically), instead of panicking commonware's bounded sender
// on dissemination.
// ---------------------------------------------------------------------------

#[test]
fn block_exceeding_p2p_transport_cap_is_rejected() {
    use outbe_primitives::consensus::OUTBE_MAX_BLOCK_SIZE;
    let signer = signer();
    // One tx carrying ~1.9 MiB of calldata pushes the block RLP over the cap.
    let transactions = vec![signed_with_raw_input(
        &signer,
        0,
        vec![0u8; OUTBE_MAX_BLOCK_SIZE],
    )];
    let header = header_for_transactions(2, B256::ZERO, &transactions);
    let block = AlloyBlock::new(header, body(transactions)).seal_slow();
    assert!(
        block.rlp_length() > OUTBE_MAX_BLOCK_SIZE,
        "fixture must exceed the cap"
    );
    let chain_spec = Arc::new(
        ChainSpecBuilder::mainnet()
            .with_osaka_at(0)
            .build()
            .map_header(OutbeHeader::new),
    );
    let consensus = OutbeBeaconConsensus::new(chain_spec, TEST_BLOCK_ORDER);
    let err = consensus
        .validate_block_pre_execution(&block)
        .expect_err("a block over the P2P transport cap must be rejected at verify");
    assert!(
        err.to_string().contains("transport cap"),
        "rejection must reference the transport cap; got: {err:?}"
    );
}

#[test]
fn explicit_order_controls_public_boundary_and_consensus_adapter() {
    use outbe_evm::system_tx::BlockOrder;
    use reth_primitives_traits::SealedHeader;

    let signer = signer();
    let parent_hash = B256::repeat_byte(0xA4);
    let mut transactions = system_txs_block2(&signer, parent_hash, Default::default());
    transactions.truncate(transactions.len() - TEST_BLOCK_ORDER.end_block.len());
    let header = header_for_transactions(2, parent_hash, &transactions);
    let block = AlloyBlock::new(header.clone(), body(transactions)).seal_slow();
    let order = BlockOrder {
        end_block: &[],
        ..TEST_BLOCK_ORDER
    };
    validate_system_tx_consensus_boundary(block.body(), &header, &order).unwrap();
    assert!(
        validate_system_tx_consensus_boundary(block.body(), &header, &TEST_BLOCK_ORDER).is_err()
    );
    let chain_spec = Arc::new(
        ChainSpecBuilder::mainnet()
            .with_osaka_at(0)
            .build()
            .map_header(OutbeHeader::new),
    );
    for injected in [order, BlockOrder::default()] {
        let consensus = OutbeBeaconConsensus::new(chain_spec.clone(), injected);
        let sealed_header = SealedHeader::seal_slow(header.clone());
        let accepted = !injected.consensus.is_empty();
        assert_eq!(
            consensus
                .validate_body_against_header(block.body(), &sealed_header)
                .is_ok(),
            accepted
        );
        assert_eq!(
            consensus.validate_block_pre_execution(&block).is_ok(),
            accepted
        );
        assert_eq!(
            consensus
                .validate_block_pre_execution_with_tx_root(
                    &block,
                    Some(header.inner.transactions_root)
                )
                .is_ok(),
            accepted
        );
    }
}

#[test]
fn header_committed_steps_cannot_be_omitted_from_injected_order() {
    use reth_ethereum::consensus::ConsensusError;
    use {
        outbe_evm::system_tx::BlockOrder,
        outbe_primitives::{
            reshare_artifact::ConsensusHeaderArtifact,
            system_tx::binding::{BoundaryBindingError, PublicationBindingError},
            test_utils::{sample_radicle_publication_batch, sample_system_tx_boundary},
        },
    };

    let parent_hash = B256::repeat_byte(0xA4);
    let mut transactions = system_txs_block2(&signer(), parent_hash, Default::default());
    transactions.remove(2); // The custom plan contains only parent accounting and late credits.
    let order = BlockOrder {
        consensus: &TEST_BLOCK_ORDER.consensus[..2],
        ..TEST_BLOCK_ORDER
    };
    let mut header = header_for_transactions(2, parent_hash, &transactions);
    let body = body(transactions);
    validate_system_tx_consensus_boundary(&body, &header, &order).unwrap();
    for artifacts in [
        OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(
                sample_system_tx_boundary(2),
            )),
            ..Default::default()
        },
        OutbeBlockArtifacts {
            radicle_publication_enabled: true,
            radicle_publication: Some(sample_radicle_publication_batch(1)),
            ..Default::default()
        },
    ] {
        header.inner.extra_data = encode_outbe_block_artifacts(&artifacts).unwrap();
        let ConsensusError::Other(error) =
            validate_system_tx_consensus_boundary(&body, &header, &order).unwrap_err()
        else {
            panic!("expected missing consensus step")
        };
        if artifacts.radicle_publication.is_some() {
            assert!(matches!(
                error.downcast_ref::<PublicationBindingError>(),
                Some(PublicationBindingError::Missing)
            ));
        } else {
            assert!(matches!(
                error.downcast_ref::<BoundaryBindingError>(),
                Some(BoundaryBindingError::Missing)
            ));
        }
    }
}
