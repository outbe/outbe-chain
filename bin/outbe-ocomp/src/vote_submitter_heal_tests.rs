//! Self-heal of a vote whose pinned nonce was consumed by another transaction
//! from the shared role-delegated sender (payout batches share that key).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alloy_consensus::TxEip1559;
use alloy_eips::eip2718::Encodable2718 as _;
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use outbe_ocomp_protocol::{
    abi::encode_submit_lysis_result_calldata,
    common::BoundedBytes,
    hash::hash_framed,
    intent::DayType,
    profile::poc_schema_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    vote::ResultVoteV1,
    PreparedVoteTransactionV1, SchemaLimits,
};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::signer::OutbeEvmSigner;

use crate::vote_submitter::{
    SupervisorVoteSubmitterV1, VoteBlockV1, VoteReceiptV1, VoteSubmissionConfigV1,
    VoteSubmissionOutcomeV1, VoteSubmissionRpcV1, VoteTransactionPreparerV1,
};

const JOB_ID: B256 = B256::repeat_byte(0x51);
const BLOCK: B256 = B256::repeat_byte(0x52);

fn test_result(limits: &SchemaLimits) -> LysisResultV1 {
    let roots = ResultRootsV1 {
        nod_root: B256::repeat_byte(0x31),
        bucket_root: B256::repeat_byte(0x32),
        contributor_root: B256::repeat_byte(0x33),
        output_manifest_root: B256::repeat_byte(0x34),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        contributor_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        eligible_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        gratis_supply: U256::ZERO,
        lysis_budget: U256::ZERO,
        auction_base: U256::ZERO,
        nod_gratis_consumed: U256::ZERO,
        unused_lysis: U256::ZERO,
        carry_over_credit: U256::ZERO,
        nod_cost_total: U256::ZERO,
    };
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: B256::repeat_byte(0x35),
        plan_hash: B256::repeat_byte(0x36),
        unit_artifact_root: B256::repeat_byte(0x37),
        fidelity_fraction_root: B256::repeat_byte(0x38),
        gratis_prefix_root: B256::repeat_byte(0x39),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    LysisResultV1 {
        protocol_bundle_hash: B256::repeat_byte(0x20),
        job_id: JOB_ID,
        attempt: 0,
        input_manifest_hash: summary.input_manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: B256::repeat_byte(0x3a),
        carry_over_credit: CarryOverCreditActionV1 {
            source_wwd: 1,
            reason: CarryOverReason::UnusedLysis,
            amount: U256::ZERO,
        },
        metadosis_completion_summary: MetadosisCompletionSummaryV1 {
            wwd: 1,
            pending_nonce: 0,
            day_type: DayType::Green,
            tribute_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
            nod_gratis_consumed: U256::ZERO,
            unused_lysis: U256::ZERO,
            carry_over_credit: U256::ZERO,
            status: CompletionStatus::Completed,
            logical_evaluation_height: 1,
            logical_evaluation_time: 1,
        },
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment: hash_framed(
            HashDomain::LysisArithmetic,
            &summary.encode_canonical(limits).unwrap(),
        )
        .unwrap(),
        event_summary_hash: lysis_v1_empty_semantic_event_root().unwrap(),
    }
}

fn canonical_result() -> Vec<u8> {
    let limits = poc_schema_limits();
    test_result(&limits).encode_canonical(&limits).unwrap()
}

fn result_digest() -> B256 {
    let limits = poc_schema_limits();
    test_result(&limits).result_digest(&limits).unwrap()
}

struct RecordingPreparer {
    signer: OutbeEvmSigner,
    limits: SchemaLimits,
    calls: Arc<AtomicU64>,
    last_nonce: Arc<AtomicU64>,
}

impl VoteTransactionPreparerV1 for RecordingPreparer {
    type Error = std::io::Error;

    fn prepare_vote_transaction(
        &self,
        _canonical_result: &[u8],
        nonce: u64,
        max_fee_per_gas: u128,
        gas_limit: u64,
    ) -> Result<PreparedVoteTransactionV1, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.last_nonce.store(nonce, Ordering::SeqCst);
        let vote = ResultVoteV1 {
            protocol_bundle_hash: B256::repeat_byte(0x20),
            job_id: JOB_ID,
            attempt: 0,
            result_committee_snapshot_hash: B256::repeat_byte(0x23),
            validator_index: 0,
            key_epoch: 1,
            result: test_result(&self.limits),
            signature_rs: [0x24; 64],
        };
        let canonical_vote = vote
            .encode_canonical(&self.limits)
            .map_err(std::io::Error::other)?;
        let calldata = encode_submit_lysis_result_calldata(&vote, &self.limits)
            .map_err(std::io::Error::other)?;
        let signed = self
            .signer
            .sign_eip1559(TxEip1559 {
                chain_id: 42,
                nonce,
                gas_limit,
                max_fee_per_gas,
                max_priority_fee_per_gas: 0,
                to: TxKind::Call(METADOSIS_ADDRESS),
                value: U256::ZERO,
                input: Bytes::from(calldata),
                access_list: Default::default(),
            })
            .map_err(std::io::Error::other)?;
        let transaction_hash = *signed.hash();
        let mut raw_transaction = Vec::with_capacity(signed.encode_2718_len());
        signed.encode_2718(&mut raw_transaction);
        Ok(PreparedVoteTransactionV1 {
            canonical_vote: BoundedBytes(canonical_vote),
            raw_transaction: BoundedBytes(raw_transaction),
            transaction_hash,
        })
    }
}

#[derive(Clone)]
struct NonceRpc {
    state: Arc<Mutex<NonceRpcState>>,
}

struct NonceRpcState {
    nonce: u64,
    receipt: Option<VoteReceiptV1>,
    canonical: Option<VoteBlockV1>,
    finalized: VoteBlockV1,
    broadcasts: usize,
}

impl NonceRpc {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(NonceRpcState {
                nonce: 7,
                receipt: None,
                canonical: None,
                finalized: VoteBlockV1 {
                    number: 0,
                    hash: B256::ZERO,
                },
                broadcasts: 0,
            })),
        }
    }

    fn bypass_nonce(&self, nonce: u64) {
        self.state.lock().unwrap().nonce = nonce;
    }

    fn include(&self, transaction_hash: B256) {
        let mut state = self.state.lock().unwrap();
        state.receipt = Some(VoteReceiptV1 {
            transaction_hash,
            block_number: 1,
            block_hash: BLOCK,
            success: true,
        });
        state.canonical = Some(VoteBlockV1 {
            number: 1,
            hash: BLOCK,
        });
    }

    fn broadcasts(&self) -> usize {
        self.state.lock().unwrap().broadcasts
    }
}

impl VoteSubmissionRpcV1 for NonceRpc {
    type Error = std::io::Error;

    fn chain_id(&self) -> Result<u64, Self::Error> {
        Ok(42)
    }

    fn canonical_nonce(&self, _address: Address) -> Result<u64, Self::Error> {
        Ok(self.state.lock().unwrap().nonce)
    }

    fn gas_price(&self) -> Result<u128, Self::Error> {
        Ok(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS)
    }

    fn send_raw_transaction(
        &self,
        _raw_transaction: &[u8],
        expected_hash: B256,
    ) -> Result<B256, Self::Error> {
        self.state.lock().unwrap().broadcasts += 1;
        Ok(expected_hash)
    }

    fn transaction_receipt(
        &self,
        _transaction_hash: B256,
    ) -> Result<Option<VoteReceiptV1>, Self::Error> {
        Ok(self.state.lock().unwrap().receipt)
    }

    fn canonical_block(&self, _number: u64) -> Result<Option<VoteBlockV1>, Self::Error> {
        Ok(self.state.lock().unwrap().canonical)
    }

    fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error> {
        Ok(self.state.lock().unwrap().finalized)
    }
}

fn fixture(
    root: &Path,
) -> (
    SupervisorVoteSubmitterV1<NonceRpc>,
    NonceRpc,
    RecordingPreparer,
    Arc<AtomicU64>,
    Arc<AtomicU64>,
) {
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("test signer");
    let sender_address = signer.address();
    let rpc = NonceRpc::new();
    let calls = Arc::new(AtomicU64::new(0));
    let last_nonce = Arc::new(AtomicU64::new(u64::MAX));
    let preparer = RecordingPreparer {
        signer,
        limits: poc_schema_limits(),
        calls: calls.clone(),
        last_nonce: last_nonce.clone(),
    };
    let submitter = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: root.to_path_buf(),
            expected_chain_id: 42,
            sender_address,
            limits: poc_schema_limits(),
        },
        rpc.clone(),
    )
    .expect("vote submitter");
    (submitter, rpc, preparer, calls, last_nonce)
}

fn reconcile(
    submitter: &mut SupervisorVoteSubmitterV1<NonceRpc>,
    preparer: &RecordingPreparer,
) -> VoteSubmissionOutcomeV1 {
    submitter
        .reconcile(preparer, JOB_ID, result_digest(), &canonical_result())
        .expect("reconcile")
}

#[test]
fn a_bypassed_nonce_rebuilds_the_vote_from_the_prepared_stage() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, last_nonce) = fixture(directory.path());

    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(last_nonce.load(Ordering::SeqCst), 7);

    rpc.bypass_nonce(8);
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(last_nonce.load(Ordering::SeqCst), 8);

    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 1);
}

#[test]
fn a_bypassed_nonce_rebuilds_the_vote_from_the_submitted_stage() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, last_nonce) = fixture(directory.path());

    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );

    rpc.bypass_nonce(9);
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(last_nonce.load(Ordering::SeqCst), 9);
}

#[test]
fn an_included_receipt_wins_over_the_advanced_nonce() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, _) = fixture(directory.path());

    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );

    // Our own envelope landed: the receipt must keep the record.
    let transaction_hash = {
        let limits = poc_schema_limits();
        let prepared = preparer
            .prepare_vote_transaction(
                &canonical_result(),
                7,
                outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS,
                outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
            )
            .unwrap();
        let _ = limits;
        prepared.transaction_hash
    };
    rpc.bypass_nonce(8);
    rpc.include(transaction_hash);

    assert!(matches!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Included(_)
    ));
    // Two prepare calls: the fixture one plus the hash probe above.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_healthy_nonce_keeps_rebroadcasting_the_same_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, _) = fixture(directory.path());

    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(
        reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
