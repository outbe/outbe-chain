#[test]
fn a_suggestion_above_the_ceiling_is_signed_at_the_ceiling() {
    let suggested = MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS + 2;
    let fee = fee_within_envelope(
        suggested,
        MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
    );
    assert_eq!(fee, MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS);
}

#[test]
fn a_suggestion_below_the_floor_is_signed_at_the_floor() {
    let fee = fee_within_envelope(
        0,
        MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
    );
    assert_eq!(fee, MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS);
}

#[test]
fn every_suggestion_lands_inside_the_envelope_the_submitter_validates() {
    for suggested in [0, 1, 7, 13, 14, 15, 1_000_000, u128::MAX] {
        let fee = fee_within_envelope(
            suggested,
            MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
            MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
        );
        assert!(
            (MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS..=MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS)
                .contains(&fee),
            "suggestion {suggested} left the envelope as {fee}"
        );
    }
}

use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use alloy_consensus::TxEip1559;
use alloy_primitives::Bytes;
use outbe_ocomp_protocol::{
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
};
use outbe_primitives::signer::OutbeEvmSigner;

use super::*;

const JOB_ID: B256 = B256::repeat_byte(0x21);
const BLOCK_A: B256 = B256::repeat_byte(0x31);
const BLOCK_B: B256 = B256::repeat_byte(0x32);

#[test]
fn signer_fee_cap_is_twice_the_native_protocol_floor() {
    assert_eq!(
        MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
        MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS * 2
    );
}

#[test]
fn public_rpc_preserves_null_result_for_pending_receipt() {
    let response = br#"{"jsonrpc":"2.0","id":7,"result":null}"#;

    assert_eq!(
        parse_rpc_response(response, 7).expect("valid pending receipt response"),
        serde_json::Value::Null
    );
}

#[test]
fn public_rpc_rejects_success_response_without_result_field() {
    let response = br#"{"jsonrpc":"2.0","id":7}"#;

    assert!(matches!(
        parse_rpc_response(response, 7),
        Err(PublicVoteRpcErrorV1::MissingResult)
    ));
}

fn test_result(limits: &SchemaLimits) -> LysisResultV1 {
    let roots = ResultRootsV1 {
        nod_root: B256::repeat_byte(0x31),
        bucket_root: B256::repeat_byte(0x32),
        output_manifest_root: B256::repeat_byte(0x34),
    };
    let counts = ExactCountsV1 {
        tribute_count: 1,
        nod_count: 1,
        bucket_count: 0,
        semantic_event_count: 0,
    };
    let conservation = ConservationTotalsV1 {
        tribute_nominal_total: U256::ZERO,
        day_limit: U256::ZERO,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: U256::ZERO,
        desis_limit_minor: U256::ZERO,
        lysis_allocation_minor: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
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
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            status: CompletionStatus::Completed,
            logical_evaluation_height: 1,
            logical_evaluation_time: 1,
        },
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis_limit_minor: U256::ZERO,
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

fn finalized_job_spec() -> FinalizedJobSpecV1 {
    use outbe_ocomp_protocol::control::FinalizedJobSummaryV1;

    FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: 1,
            job_id: JOB_ID,
            intent_id: B256::repeat_byte(0x40),
            finalized_block_hash: B256::repeat_byte(0x41),
            finalized_state_root: B256::repeat_byte(0x42),
            protocol_bundle_hash: B256::repeat_byte(0x20),
            open_height: 1,
            deadline_height: 100,
        },
        canonical_job_intent: BoundedBytes(Vec::new()),
    }
}

struct FakePreparer {
    signer: OutbeEvmSigner,
    target: Address,
    calls: Arc<AtomicU64>,
    limits: SchemaLimits,
}

impl VoteTransactionPreparerV1 for FakePreparer {
    type Error = std::io::Error;

    fn prepare_vote_transaction(
        &self,
        _canonical_result: &[u8],
        _finalized: &FinalizedJobSpecV1,
        _canonical_height: u64,
        (nonce, max_fee_per_gas, gas_limit): (u64, u128, u64),
    ) -> Result<PreparedVoteTransactionV1, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = test_result(&self.limits);
        let vote = ResultVoteV1 {
            protocol_bundle_hash: result.protocol_bundle_hash,
            job_id: result.job_id,
            attempt: result.attempt,
            result_validator_set_epoch: 7,
            result_committee_set_hash: B256::repeat_byte(0x41),
            result_ocomp_binding_hash: B256::repeat_byte(0x42),
            ocomp_key_hash: B256::repeat_byte(0x43),
            key_epoch: 1,
            result,
            signature_rs: [0; 64],
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
                to: TxKind::Call(self.target),
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
struct FakeRpc {
    state: Arc<Mutex<FakeRpcState>>,
}

struct FakeRpcState {
    receipt: Option<VoteReceiptV1>,
    canonical: Option<VoteBlockV1>,
    finalized: VoteBlockV1,
    broadcasts: usize,
}

impl FakeRpc {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeRpcState {
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

    fn orphan(&self) {
        let mut state = self.state.lock().expect("fake RPC state");
        state.receipt = None;
        state.canonical = None;
    }

    fn finalize(&self, number: u64, hash: B256) {
        self.state.lock().expect("fake RPC state").finalized = VoteBlockV1 { number, hash };
    }

    fn broadcasts(&self) -> usize {
        self.state.lock().expect("fake RPC state").broadcasts
    }
}

impl VoteSubmissionRpcV1 for FakeRpc {
    type Error = std::io::Error;

    fn chain_id(&self) -> Result<u64, Self::Error> {
        Ok(42)
    }

    fn canonical_nonce(&self, _address: Address) -> Result<u64, Self::Error> {
        Ok(7)
    }

    fn gas_price(&self) -> Result<u128, Self::Error> {
        Ok(MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS)
    }

    fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
        expected_hash: B256,
    ) -> Result<B256, Self::Error> {
        if raw_transaction.is_empty() {
            return Err(std::io::Error::other("empty transaction"));
        }
        let mut state = self.state.lock().expect("fake RPC state");
        state.broadcasts += 1;
        if let Some(receipt) = state.receipt.as_mut() {
            receipt.transaction_hash = expected_hash;
        }
        Ok(expected_hash)
    }

    fn transaction_receipt(
        &self,
        _transaction_hash: B256,
    ) -> Result<Option<VoteReceiptV1>, Self::Error> {
        Ok(self.state.lock().expect("fake RPC state").receipt)
    }

    fn canonical_block(&self, _number: u64) -> Result<Option<VoteBlockV1>, Self::Error> {
        Ok(self.state.lock().expect("fake RPC state").canonical)
    }

    fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error> {
        Ok(self.state.lock().expect("fake RPC state").finalized)
    }
}

fn fixture(
    root: &Path,
    target: Address,
) -> (
    SupervisorVoteSubmitterV1<FakeRpc>,
    FakeRpc,
    FakePreparer,
    Arc<AtomicU64>,
) {
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM test signer");
    let sender_address = signer.address();
    let rpc = FakeRpc::new();
    let calls = Arc::new(AtomicU64::new(0));
    let preparer = FakePreparer {
        signer,
        target,
        calls: calls.clone(),
        limits: poc_schema_limits(),
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
    (submitter, rpc, preparer, calls)
}

#[test]
fn ocm_pub_001_journals_prepare_submit_include_and_finalize_across_restart() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (mut submitter, rpc, preparer, calls) = fixture(directory.path(), METADOSIS_ADDRESS);

    assert_eq!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(rpc.broadcasts(), 0);
    assert_eq!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 1);

    let transaction_hash = submitter
        .journal
        .load(JOB_ID)
        .unwrap()
        .unwrap()
        .transaction_hash;
    {
        let mut state = rpc.state.lock().expect("fake RPC state");
        state.receipt = Some(VoteReceiptV1 {
            transaction_hash,
            block_number: 10,
            block_hash: BLOCK_A,
            success: true,
        });
        state.canonical = Some(VoteBlockV1 {
            number: 10,
            hash: BLOCK_A,
        });
    }
    assert_eq!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Included(VoteInclusionV1 {
            block_number: 10,
            block_hash: BLOCK_A,
            success: true,
        })
    );
    rpc.finalize(9, B256::repeat_byte(9));
    assert!(matches!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Included(_)
    ));
    rpc.finalize(10, BLOCK_A);
    assert!(matches!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Finalized(VoteInclusionV1 { success: true, .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    drop(submitter);
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM test signer");
    let mut restarted = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: directory.path().to_path_buf(),
            expected_chain_id: 42,
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        rpc,
    )
    .expect("restarted vote submitter");
    assert!(matches!(
        restarted
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Finalized(_)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn ocm_pub_001_orphaned_inclusion_demotes_and_rebroadcasts_identical_transaction() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (mut submitter, rpc, preparer, _) = fixture(directory.path(), METADOSIS_ADDRESS);
    submitter
        .reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .unwrap();
    submitter
        .reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .unwrap();
    let transaction_hash = submitter
        .journal
        .load(JOB_ID)
        .unwrap()
        .unwrap()
        .transaction_hash;
    {
        let mut state = rpc.state.lock().expect("fake RPC state");
        state.receipt = Some(VoteReceiptV1 {
            transaction_hash,
            block_number: 10,
            block_hash: BLOCK_A,
            success: true,
        });
        state.canonical = Some(VoteBlockV1 {
            number: 10,
            hash: BLOCK_A,
        });
    }
    submitter
        .reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .unwrap();
    rpc.orphan();
    assert_eq!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 2);

    {
        let mut state = rpc.state.lock().expect("fake RPC state");
        state.receipt = Some(VoteReceiptV1 {
            transaction_hash,
            block_number: 12,
            block_hash: BLOCK_B,
            success: true,
        });
        state.canonical = Some(VoteBlockV1 {
            number: 12,
            hash: BLOCK_B,
        });
    }
    assert!(matches!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Included(VoteInclusionV1 {
            block_number: 12,
            block_hash: BLOCK_B,
            ..
        })
    ));
    rpc.finalize(12, BLOCK_B);
    assert!(matches!(
        submitter
            .reconcile(
                &preparer,
                JOB_ID,
                result_digest(),
                &canonical_result(),
                &finalized_job_spec(),
            )
            .unwrap(),
        VoteSubmissionOutcomeV1::Finalized(_)
    ));
}

#[test]
fn ocm_pub_001_rejects_a_locally_signed_transaction_with_any_other_target() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let wrong_target = Address::repeat_byte(0x44);
    let (mut submitter, rpc, preparer, _) = fixture(directory.path(), wrong_target);
    assert!(matches!(
        submitter.reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        ),
        Err(VoteSubmissionErrorV1::InvalidPreparedTransaction(_))
    ));
    assert_eq!(rpc.broadcasts(), 0);
    assert!(submitter.journal.load(JOB_ID).unwrap().is_none());
}

#[test]
fn vote_submission_journal_rejects_one_byte_tampering() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let (mut submitter, _, preparer, _) = fixture(directory.path(), METADOSIS_ADDRESS);
    submitter
        .reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .unwrap();
    let path = submitter.journal.record_path(JOB_ID);
    drop(submitter);
    let mut bytes = fs::read(&path).expect("journal bytes");
    bytes[40] ^= 1;
    fs::write(&path, bytes).expect("tamper journal");
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM test signer");
    let rpc = FakeRpc::new();
    let submitter = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: directory.path().to_path_buf(),
            expected_chain_id: 42,
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        rpc,
    )
    .expect("reopen journal root");
    assert!(matches!(
        submitter.journal.load(JOB_ID),
        Err(VoteSubmissionErrorV1::JournalChecksumMismatch)
    ));
}

#[test]
fn public_vote_nonce_targets_canonical_state_instead_of_pending_state() {
    assert_eq!(
        canonical_nonce_params(Address::repeat_byte(0x44)),
        serde_json::json!([format!("{:#x}", Address::repeat_byte(0x44)), "latest"])
    );
}

struct NonceRecordingPreparer {
    inner: FakePreparer,
    last_nonce: Arc<AtomicU64>,
}

impl VoteTransactionPreparerV1 for NonceRecordingPreparer {
    type Error = std::io::Error;

    fn prepare_vote_transaction(
        &self,
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
        canonical_height: u64,
        (nonce, max_fee_per_gas, gas_limit): (u64, u128, u64),
    ) -> Result<PreparedVoteTransactionV1, Self::Error> {
        self.last_nonce.store(nonce, Ordering::SeqCst);
        self.inner.prepare_vote_transaction(
            canonical_result,
            finalized,
            canonical_height,
            (nonce, max_fee_per_gas, gas_limit),
        )
    }
}

/// Unlike [`FakeRpc`], the account nonce can move under a journaled envelope.
#[derive(Clone)]
struct NonceRpc {
    state: Arc<Mutex<NonceRpcState>>,
}

struct NonceRpcState {
    nonce: u64,
    fail_chain_id_once: bool,
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
                fail_chain_id_once: false,
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

    fn fail_next_chain_id(&self) {
        self.state.lock().unwrap().fail_chain_id_once = true;
    }

    fn include(&self, transaction_hash: B256) {
        self.include_with_status(transaction_hash, true);
    }

    fn include_with_status(&self, transaction_hash: B256, success: bool) {
        let mut state = self.state.lock().unwrap();
        state.receipt = Some(VoteReceiptV1 {
            transaction_hash,
            block_number: 1,
            block_hash: BLOCK_A,
            success,
        });
        state.canonical = Some(VoteBlockV1 {
            number: 1,
            hash: BLOCK_A,
        });
    }

    fn finalize(&self, number: u64, hash: B256) {
        self.state.lock().unwrap().finalized = VoteBlockV1 { number, hash };
    }

    fn broadcasts(&self) -> usize {
        self.state.lock().unwrap().broadcasts
    }
}

impl VoteSubmissionRpcV1 for NonceRpc {
    type Error = std::io::Error;

    fn chain_id(&self) -> Result<u64, Self::Error> {
        let mut state = self.state.lock().unwrap();
        if state.fail_chain_id_once {
            state.fail_chain_id_once = false;
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "transient RPC outage",
            ));
        }
        Ok(42)
    }

    fn canonical_nonce(&self, _address: Address) -> Result<u64, Self::Error> {
        Ok(self.state.lock().unwrap().nonce)
    }

    fn gas_price(&self) -> Result<u128, Self::Error> {
        Ok(outbe_ocomp_protocol::system_carrier::MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS)
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

fn nonce_fixture(
    root: &Path,
) -> (
    SupervisorVoteSubmitterV1<NonceRpc>,
    NonceRpc,
    NonceRecordingPreparer,
    Arc<AtomicU64>,
    Arc<AtomicU64>,
) {
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("test signer");
    let sender_address = signer.address();
    let rpc = NonceRpc::new();
    let calls = Arc::new(AtomicU64::new(0));
    let last_nonce = Arc::new(AtomicU64::new(u64::MAX));
    let preparer = NonceRecordingPreparer {
        inner: FakePreparer {
            signer,
            target: METADOSIS_ADDRESS,
            calls: calls.clone(),
            limits: poc_schema_limits(),
        },
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

fn heal_reconcile(
    submitter: &mut SupervisorVoteSubmitterV1<NonceRpc>,
    preparer: &NonceRecordingPreparer,
) -> VoteSubmissionOutcomeV1 {
    submitter
        .reconcile(
            preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .expect("reconcile")
}

fn journaled_calldata(record: &VoteSubmissionRecordV1) -> Bytes {
    let mut encoded = record.raw_transaction.as_slice();
    let transaction = EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut encoded)
        .expect("journaled transaction decodes");
    assert!(encoded.is_empty());
    transaction.input().clone()
}

#[test]
fn a_bypassed_nonce_rebuilds_the_vote_from_the_prepared_stage() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, last_nonce) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(last_nonce.load(Ordering::SeqCst), 7);
    let first = submitter.journal.load(JOB_ID).unwrap().unwrap();

    rpc.bypass_nonce(8);
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(last_nonce.load(Ordering::SeqCst), 8);
    let replacement = submitter.journal.load(JOB_ID).unwrap().unwrap();
    assert_eq!(journaled_calldata(&replacement), journaled_calldata(&first));
    assert_ne!(replacement.transaction_hash, first.transaction_hash);
    assert!(replacement.generation > first.generation);

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 1);
}

#[test]
fn durable_journal_exposes_stage_age_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, _, _) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert!(submitter.journal.stage_age(JOB_ID).is_some());

    drop(submitter);
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("test signer");
    let restarted = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: directory.path().to_path_buf(),
            expected_chain_id: 42,
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        rpc,
    )
    .expect("restarted vote submitter");
    assert!(restarted.journal.stage_age(JOB_ID).is_some());
}

#[test]
fn a_bypassed_nonce_rebuilds_the_vote_from_the_submitted_stage() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, last_nonce) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    let first = submitter.journal.load(JOB_ID).unwrap().unwrap();

    rpc.bypass_nonce(9);
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(last_nonce.load(Ordering::SeqCst), 9);
    let replacement = submitter.journal.load(JOB_ID).unwrap().unwrap();
    assert_eq!(journaled_calldata(&replacement), journaled_calldata(&first));
    assert_ne!(replacement.transaction_hash, first.transaction_hash);
    assert!(replacement.generation > first.generation);
}

#[test]
fn an_included_receipt_wins_over_the_advanced_nonce() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, _) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );

    // Our own envelope landed: the receipt must keep the record.
    let transaction_hash = preparer
        .prepare_vote_transaction(
            &canonical_result(),
            &finalized_job_spec(),
            1,
            (
                7,
                outbe_ocomp_protocol::system_carrier::MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
                outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
            ),
        )
        .unwrap()
        .transaction_hash;
    rpc.bypass_nonce(8);
    rpc.include(transaction_hash);

    assert!(matches!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Included(_)
    ));
    // Two prepare calls: the fixture one plus the hash probe above.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_healthy_nonce_keeps_rebroadcasting_the_same_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, _) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    assert_eq!(rpc.broadcasts(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_finalized_revert_prepares_a_fresh_vote_envelope() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, last_nonce) = nonce_fixture(directory.path());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Submitted
    );
    let first = submitter.journal.load(JOB_ID).unwrap().unwrap();
    rpc.include_with_status(first.transaction_hash, false);
    assert!(matches!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Included(VoteInclusionV1 { success: false, .. })
    ));

    rpc.bypass_nonce(8);
    rpc.finalize(1, BLOCK_A);
    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    let replacement = submitter.journal.load(JOB_ID).unwrap().unwrap();
    assert_eq!(replacement.stage, VoteSubmissionStageV1::Prepared);
    assert!(replacement.generation > first.generation);
    assert_eq!(replacement.nonce, 8);
    assert_ne!(replacement.transaction_hash, first.transaction_hash);
    assert_eq!(last_nonce.load(Ordering::SeqCst), 8);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    drop(submitter);
    let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).expect("test signer");
    let restarted = SupervisorVoteSubmitterV1::open(
        VoteSubmissionConfigV1 {
            journal_root: directory.path().to_path_buf(),
            expected_chain_id: 42,
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        rpc,
    )
    .expect("restarted vote submitter");
    assert_eq!(
        restarted.journal.load(JOB_ID).unwrap().unwrap(),
        replacement
    );
}

#[test]
fn submission_error_classification_is_fail_closed() {
    assert_eq!(
        rpc_error(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "temporarily unavailable",
        ))
        .class(),
        VoteSubmissionFailureClassV1::Retryable
    );
    assert_eq!(
        rpc_error(PublicVoteRpcErrorV1::Malformed(
            "invalid receipt".to_owned()
        ))
        .class(),
        VoteSubmissionFailureClassV1::Unrecoverable
    );
    for error in [
        VoteSubmissionErrorV1::Preparer("invalid attestation".to_owned()),
        VoteSubmissionErrorV1::InvalidJournal,
        VoteSubmissionErrorV1::JournalChecksumMismatch,
        VoteSubmissionErrorV1::DifferentResultForJournaledJob,
    ] {
        assert_eq!(error.class(), VoteSubmissionFailureClassV1::Unrecoverable);
    }
}

#[test]
fn a_retryable_rpc_failure_recovers_without_journaling_partial_state() {
    let directory = tempfile::tempdir().unwrap();
    let (mut submitter, rpc, preparer, calls, _) = nonce_fixture(directory.path());
    rpc.fail_next_chain_id();

    let error = submitter
        .reconcile(
            &preparer,
            JOB_ID,
            result_digest(),
            &canonical_result(),
            &finalized_job_spec(),
        )
        .expect_err("first RPC probe fails");
    assert_eq!(error.class(), VoteSubmissionFailureClassV1::Retryable);
    assert!(submitter.journal.load(JOB_ID).unwrap().is_none());

    assert_eq!(
        heal_reconcile(&mut submitter, &preparer),
        VoteSubmissionOutcomeV1::Prepared
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
