use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use alloy_consensus::{EthereumTxEnvelope, Transaction as _};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{TxKind, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_consensus::block::ConsensusBlock;
use outbe_metadosis::config::poc_schema_limits;
use outbe_ocomp_protocol::{
    abi::encode_submit_lysis_result_calldata,
    committee::{
        OcompCommitteeSnapshotV1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        OcompMemberV1, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::BoundedBytes,
    hash::hash_framed,
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    local_control::{effective_uid, ClientPolicy, ControlClientSession, EndpointIdentity},
    profile::poc_schema_limits as protocol_limits,
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    vote::ResultVoteV1,
    AttestationResponseV1, ListFinalizedJobsResponseV1, ListFinalizedJobsV1, LocalErrorCode,
    LocalErrorV1, NodeMessageKind, PrepareVoteTransactionV1, PreparedVoteTransactionV1,
    ProtocolError, RequestAttestationV1, SchemaLimits, MAX_FINALIZED_JOBS_PER_RESPONSE,
};
use outbe_primitives::{addresses::METADOSIS_ADDRESS, signer::OutbeEvmSigner};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::SignedTransaction as _;

use crate::ocomp::{
    attestation::{
        AtomicHeightSource, AttestationAuthorityError, AttestationError,
        ExportedAttestationAuthorityV1, FinalizedAttestationAuthority, OcompAttestationConfig,
        OcompAttestationGate,
    },
    control::{NodeControlError, OcompControlServer},
    retention::{
        CandidateFinalityV1, CandidatePinV1, FinalizedInputProofSource, OcompRetentionCoordinator,
        RetentionError,
    },
    sign_once::SignOnceStore,
    signer::OcompSigner,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn signing_key(index: u8) -> SigningKey {
    SigningKey::from_bytes((&[index.saturating_add(1); 32]).into()).expect("test signing key")
}

fn sign(key: &SigningKey, digest: B256) -> [u8; 64] {
    let signature: Signature = key.sign_prehash(digest.as_slice()).expect("test signature");
    signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into()
}

fn registration(index: u8, limits: &SchemaLimits) -> OcompKeyRegistrationV1 {
    let key = signing_key(index);
    let public_key: [u8; 33] = key
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .expect("compressed test key");
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: 42,
            genesis_hash: hash(40),
            fork_id: hash(1),
            protocol_bundle_hash: hash(41),
            validator_index: index,
            validator_identity_hash: hash(70 + index),
            ocomp_public_key_sec1: public_key,
            key_epoch: 1,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
            valid_from_height: 1,
            valid_until_height_exclusive: 1_000,
        },
        proof_of_possession: [0; 64],
    };
    let digest = registration
        .proof_of_possession_digest(limits)
        .expect("PoP digest");
    registration.proof_of_possession = sign(&key, digest);
    registration
}

fn committee(limits: &SchemaLimits) -> OcompCommitteeSnapshotV1 {
    OcompCommitteeSnapshotV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        protocol_bundle_hash: hash(41),
        snapshot_epoch: 1,
        threshold: 3,
        ordered_members: (0..4)
            .map(|index| {
                let registration = registration(index, limits);
                OcompMemberV1 {
                    validator_index: index,
                    validator_identity_hash: registration.core.validator_identity_hash,
                    ocomp_public_key_sec1: registration.core.ocomp_public_key_sec1,
                    key_epoch: registration.core.key_epoch,
                    allowed_purpose_bitmap: registration.core.allowed_purpose_bitmap,
                    valid_from_height: registration.core.valid_from_height,
                    valid_until_height_exclusive: registration.core.valid_until_height_exclusive,
                    proof_of_possession: registration.proof_of_possession,
                }
            })
            .collect(),
    }
}

fn preconditions() -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: TributeInputBindingV1 {
            wwd: 7,
            source_generation: 3,
            collection_key: hash(30),
            sealed_collection_root: hash(31),
            exact_count: 1,
            exact_nominal_total: U256::ZERO,
        },
        nod: NodTargetPreconditionV1 {
            wwd: 7,
            target_generation: 5,
            namespace_root_before: hash(32),
            max_nod_count: 1,
        },
        contributors: ContributorTargetPreconditionV1 {
            series_id: 7,
            expected_series_version: 8,
            max_contributor_count: 1,
            max_eligible_nominal_total: U256::ZERO,
        },
        metadosis: MetadosisAttemptPreconditionV1 {
            wwd: 7,
            pending_nonce: 1,
            expected_status: MetadosisExpectedStatus::OffchainPending,
            state_version: 12,
        },
    }
}

pub(super) fn fixture() -> (
    OcompCommitteeSnapshotV1,
    ExportedAttestationAuthorityV1,
    LysisResultV1,
) {
    let limits = protocol_limits();
    assert_eq!(limits, poc_schema_limits());
    let committee = committee(&limits);
    let intent = JobIntentV1 {
        chain_id: 42,
        genesis_hash: hash(40),
        fork_id: hash(1),
        wwd: 7,
        pending_nonce: 1,
        attempt: 1,
        protocol_bundle_hash: hash(41),
        ce_sealed_root: hash(42),
        sealed_tribute_collection_key: hash(30),
        sealed_tribute_collection_root: hash(31),
        authenticated_day_count: 1,
        authenticated_day_nominal: U256::ZERO,
        pre_admission_envelope_hash: hash(43),
        source_availability_policy_id: hash(44),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: U256::ZERO,
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
            gratis_demand: U256::ZERO,
            gratis_supply: U256::ZERO,
            lysis_budget: U256::ZERO,
            auction_base: U256::ZERO,
            auction_entry_price: U256::ZERO,
            request_budget_split_receipt_hash: hash(113),
        },
        logical_evaluation_height: 100,
        logical_evaluation_time: 1_000,
        activation_preconditions: preconditions(),
        result_committee_snapshot_hash: committee.snapshot_hash(&limits).expect("committee hash"),
        custody_committee_epoch_hash: None,
    };
    intent.validate_semantics().expect("valid test intent");
    let candidate = CandidatePinV1 {
        block_number: 99,
        block_hash: hash(90),
        state_root: hash(91),
        intent_id: intent.intent_id(&limits).expect("intent id"),
        wwd: intent.wwd,
        ce_sealed_root: intent.ce_sealed_root,
        protocol_bundle_hash: intent.protocol_bundle_hash,
        input_lease_id: intent.input_lease_id().expect("input lease id"),
    };
    let job_id = intent
        .job_id(candidate.block_hash, candidate.state_root, &limits)
        .expect("job id");

    let carry_over_credit = CarryOverCreditActionV1 {
        source_wwd: 7,
        reason: CarryOverReason::UnusedLysis,
        amount: U256::ZERO,
    };
    let completion = MetadosisCompletionSummaryV1 {
        wwd: 7,
        pending_nonce: 1,
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
        logical_evaluation_height: 100,
        logical_evaluation_time: 1_000,
    };
    let roots = ResultRootsV1 {
        nod_root: hash(50),
        bucket_root: hash(51),
        contributor_root: hash(52),
        output_manifest_root: hash(53),
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
    let manifest_hash = hash(54);
    let summary = LysisArithmeticSummaryV1 {
        input_manifest_hash: manifest_hash,
        plan_hash: hash(55),
        unit_artifact_root: hash(56),
        fidelity_fraction_root: hash(57),
        gratis_prefix_root: hash(58),
        roots: roots.clone(),
        counts: counts.clone(),
        conservation: conservation.clone(),
        first_error_ordinal: None,
    };
    let arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &summary.encode_canonical(&limits).expect("summary encoding"),
    )
    .expect("arithmetic commitment");
    let result = LysisResultV1 {
        protocol_bundle_hash: intent.protocol_bundle_hash,
        job_id,
        attempt: intent.attempt,
        input_manifest_hash: manifest_hash,
        plan_hash: summary.plan_hash,
        unit_artifact_root: summary.unit_artifact_root,
        fidelity_fraction_root: summary.fidelity_fraction_root,
        gratis_prefix_root: summary.gratis_prefix_root,
        result_chunk_count: 1,
        result_chunk_list_root: hash(61),
        carry_over_credit,
        metadosis_completion_summary: completion,
        tribute_count: 1,
        tribute_nominal_total: U256::ZERO,
        unused_lysis: U256::ZERO,
        roots,
        counts,
        conservation,
        arithmetic_commitment,
        event_summary_hash: lysis_v1_empty_semantic_event_root().expect("empty event root"),
    };
    result.validate_semantics(&limits).expect("valid result");
    (
        committee,
        ExportedAttestationAuthorityV1 {
            candidate,
            job_id,
            manifest_hash,
            finalized_intent: intent,
            finality_recorded_height: 100,
            open_height: 104,
            deadline_height: 110,
        },
        result,
    )
}

struct StaticAuthority {
    value: Mutex<ExportedAttestationAuthorityV1>,
    reloads: AtomicUsize,
}

struct ChangingAuthority {
    first: ExportedAttestationAuthorityV1,
    second: ExportedAttestationAuthorityV1,
    reloads: AtomicUsize,
}

impl FinalizedAttestationAuthority for ChangingAuthority {
    fn reload_exported(
        &self,
        job_id: B256,
        _limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        let reload = self.reloads.fetch_add(1, Ordering::SeqCst);
        let value = if reload == 0 {
            self.first.clone()
        } else {
            self.second.clone()
        };
        if value.job_id != job_id {
            return Err(AttestationAuthorityError::NotExported(job_id));
        }
        Ok(value)
    }
}

impl FinalizedAttestationAuthority for StaticAuthority {
    fn reload_exported(
        &self,
        job_id: B256,
        _limits: &SchemaLimits,
    ) -> Result<ExportedAttestationAuthorityV1, AttestationAuthorityError> {
        self.reloads.fetch_add(1, Ordering::SeqCst);
        let value = self.value.lock().expect("authority mutex").clone();
        if value.job_id != job_id {
            return Err(AttestationAuthorityError::NotExported(job_id));
        }
        Ok(value)
    }
}

struct EmptyProofSource;

impl FinalizedInputProofSource for EmptyProofSource {
    fn candidate_for_block(
        &self,
        _block: &ConsensusBlock,
    ) -> Result<Option<CandidatePinV1>, RetentionError> {
        Ok(None)
    }

    fn resolve_finality(
        &self,
        _candidate: CandidatePinV1,
    ) -> Result<CandidateFinalityV1, RetentionError> {
        Err(RetentionError::Source(
            "unused attestation control fixture".to_owned(),
        ))
    }
}

pub(super) fn attestation_gate(
    directory: &tempfile::TempDir,
    authority: Arc<dyn FinalizedAttestationAuthority>,
    height: Arc<AtomicHeightSource>,
    committee: OcompCommitteeSnapshotV1,
) -> OcompAttestationGate {
    let limits = protocol_limits();
    let signer = OcompSigner::from_secret([1; 32]).expect("local OCOMP signer");
    let root = directory.path().join("sign-once");
    let owner_uid = fs::metadata(directory.path())
        .expect("temporary directory metadata")
        .uid();
    let store = SignOnceStore::open(root, owner_uid, limits).expect("sign-once store");
    OcompAttestationGate::new(
        authority,
        height,
        OcompAttestationConfig {
            identity: EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            validator_index: 0,
        },
        committee,
        signer,
        store,
        limits,
    )
    .expect("attestation gate")
}

fn control_server(
    directory: &tempfile::TempDir,
    gate: OcompAttestationGate,
) -> Arc<OcompControlServer> {
    Arc::new(base_control_server(directory).with_attestation(Arc::new(gate)))
}

fn base_control_server(directory: &tempfile::TempDir) -> OcompControlServer {
    let limits = protocol_limits();
    let retention = Arc::new(OcompRetentionCoordinator::open(
        directory.path().join("retention"),
        Arc::new(EmptyProofSource),
    ));
    OcompControlServer::new(
        retention,
        effective_uid().expect("effective uid"),
        EndpointIdentity {
            chain_id: 42,
            genesis_hash: hash(40),
            boot_nonce: hash(99),
            protocol_bundle_hash: hash(41),
        },
        1,
        limits,
    )
    .expect("node control server")
}

fn recompute_arithmetic(result: &mut LysisResultV1, limits: &SchemaLimits) {
    result.arithmetic_commitment = hash_framed(
        HashDomain::LysisArithmetic,
        &result
            .arithmetic_summary()
            .encode_canonical(limits)
            .expect("arithmetic summary encoding"),
    )
    .expect("arithmetic commitment");
}

fn assert_no_sign_once_record(directory: &tempfile::TempDir) {
    assert_eq!(
        fs::read_dir(directory.path().join("sign-once"))
            .expect("read sign-once directory")
            .count(),
        0,
        "a rejected candidate must not consume the sign-once slot"
    );
}

#[test]
fn ocm_sig_001_node_reconstructs_digest_replays_exactly_and_refuses_second_result() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let height = Arc::new(AtomicHeightSource::new(105));
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority.clone(),
        height.clone(),
        committee.clone(),
    );
    let canonical = result.encode_canonical(&limits).expect("canonical result");

    let first = gate
        .attest_canonical_result(&canonical)
        .expect("first attestation");
    first
        .verify(
            &exported.finalized_intent,
            exported.job_id,
            &committee,
            105,
            exported.open_height,
            exported.deadline_height,
            &limits,
        )
        .expect("candidate verification");
    let replay = gate
        .attest_canonical_result(&canonical)
        .expect("exact replay");
    assert_eq!(replay, first);
    assert_eq!(authority.reloads.load(Ordering::SeqCst), 4);

    let mut conflicting = result.clone();
    conflicting.roots.nod_root = hash(62);
    recompute_arithmetic(&mut conflicting, &limits);
    assert!(matches!(
        gate.attest(conflicting.clone()),
        Err(AttestationError::SignOnce(_))
    ));

    drop(gate);
    let restarted = attestation_gate(&directory, authority, height, committee);
    assert!(matches!(
        restarted.attest(conflicting),
        Err(AttestationError::SignOnce(_))
    ));
}

#[test]
fn ocm_sig_001_real_control_socket_attests_and_returns_typed_conflict_without_dropping_session() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        committee.clone(),
    );
    let server = control_server(&directory, gate);
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));

    let identity = EndpointIdentity {
        chain_id: 42,
        genesis_hash: hash(40),
        boot_nonce: hash(99),
        protocol_bundle_hash: hash(41),
    };
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(effective_uid().expect("effective uid"), identity, limits),
    )
    .expect("connect supervisor control session");
    client.handshake().expect("control handshake");
    let canonical = result.encode_canonical(&limits).expect("canonical result");
    let request = RequestAttestationV1 {
        canonical_result: BoundedBytes(canonical.clone()),
    };
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            request.encode_body(&limits).expect("attestation request"),
        )
        .expect("send first attestation request");
    let first_frame = client.receive_response().expect("first response");
    assert_eq!(first_frame.message_kind, NodeMessageKind::Response as u16);
    let first = AttestationResponseV1::decode_body(&first_frame.body, &limits)
        .expect("attestation response");
    let vote = ResultVoteV1::decode_canonical(&first.canonical_vote.0, &limits)
        .expect("canonical result vote");
    vote.verify(
        &exported.finalized_intent,
        exported.job_id,
        &committee,
        105,
        exported.open_height,
        exported.deadline_height,
        &limits,
    )
    .expect("result vote verification");

    let mut conflicting = result;
    conflicting.roots.nod_root = hash(62);
    recompute_arithmetic(&mut conflicting, &limits);
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            RequestAttestationV1 {
                canonical_result: BoundedBytes(
                    conflicting
                        .encode_canonical(&limits)
                        .expect("conflicting result"),
                ),
            }
            .encode_body(&limits)
            .expect("conflicting request"),
        )
        .expect("send conflicting request");
    let conflict_frame = client.receive_response().expect("conflict response");
    assert_eq!(conflict_frame.message_kind, NodeMessageKind::Error as u16);
    let conflict =
        LocalErrorV1::decode_body(&conflict_frame.body, &limits).expect("typed local error");
    assert_eq!(
        conflict.rejected_kind,
        NodeMessageKind::RequestAttestation as u16
    );
    assert_eq!(conflict.error_code, LocalErrorCode::Conflict as u16);
    assert!(!conflict.retryable);

    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            request.encode_body(&limits).expect("replay request"),
        )
        .expect("send exact replay");
    let replay_frame = client.receive_response().expect("replay response");
    assert_eq!(replay_frame.message_kind, NodeMessageKind::Response as u16);
    let replay = AttestationResponseV1::decode_body(&replay_frame.body, &limits)
        .expect("replayed attestation");
    assert_eq!(replay, first);

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn ocm_vot_001_node_builds_only_the_exact_validator_vote_transaction() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        committee.clone(),
    );
    let evm_signer =
        Arc::new(OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM signer"));
    let expected_signer = evm_signer.address();
    let server = base_control_server(&directory)
        .with_attestation(Arc::new(gate))
        .with_vote_transaction_signer(evm_signer);
    let canonical_result = result.encode_canonical(&limits).expect("canonical result");
    let nonce = 17;
    let max_fee_per_gas = outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS + 7;

    let prepared = server
        .prepare_vote_transaction(&PrepareVoteTransactionV1 {
            canonical_result: BoundedBytes(canonical_result),
            nonce,
            max_fee_per_gas: U256::from(max_fee_per_gas),
            gas_limit: outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
        })
        .expect("restricted result-vote transaction");

    let vote = ResultVoteV1::decode_canonical(&prepared.canonical_vote.0, &limits)
        .expect("canonical result vote");
    vote.verify(
        &exported.finalized_intent,
        exported.job_id,
        &committee,
        105,
        exported.open_height,
        exported.deadline_height,
        &limits,
    )
    .expect("node-attested vote");
    let expected_calldata =
        encode_submit_lysis_result_calldata(&vote, &limits).expect("vote calldata");

    let mut raw = prepared.raw_transaction.0.as_slice();
    let transaction =
        TransactionSigned::decode_2718(&mut raw).expect("signed EIP-2718 transaction");
    assert!(
        raw.is_empty(),
        "transaction must not contain trailing bytes"
    );
    assert!(matches!(&transaction, EthereumTxEnvelope::Eip1559(_)));
    assert_eq!(transaction.chain_id(), Some(42));
    assert_eq!(transaction.nonce(), nonce);
    assert_eq!(
        transaction.gas_limit(),
        outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT
    );
    assert_eq!(transaction.max_fee_per_gas(), max_fee_per_gas);
    assert_eq!(transaction.max_priority_fee_per_gas(), Some(0));
    assert_eq!(transaction.kind(), TxKind::Call(METADOSIS_ADDRESS));
    assert_eq!(transaction.value(), U256::ZERO);
    assert_eq!(transaction.input().as_ref(), expected_calldata.as_slice());
    assert_eq!(*transaction.hash(), prepared.transaction_hash);
    assert_eq!(
        transaction.try_recover().expect("recover validator signer"),
        expected_signer
    );
}

#[test]
fn ocm_vot_001_supervisor_prepares_vote_transaction_over_the_real_control_socket() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let gate = attestation_gate(
        &directory,
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        committee,
    );
    let server = base_control_server(&directory)
        .with_attestation(Arc::new(gate))
        .with_vote_transaction_signer(Arc::new(
            OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM signer"),
        ));
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));

    let identity = EndpointIdentity {
        chain_id: 42,
        genesis_hash: hash(40),
        boot_nonce: hash(99),
        protocol_bundle_hash: hash(41),
    };
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(effective_uid().expect("effective uid"), identity, limits),
    )
    .expect("connect supervisor control session");
    client.handshake().expect("control handshake");

    let request = PrepareVoteTransactionV1 {
        canonical_result: BoundedBytes(result.encode_canonical(&limits).expect("canonical result")),
        nonce: 17,
        max_fee_per_gas: U256::from(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS + 7),
        gas_limit: outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
    };
    client
        .send_request(
            NodeMessageKind::PrepareVoteTransaction as u16,
            request.encode_body(&limits).expect("prepare-vote request"),
        )
        .expect("send prepare-vote request");
    let frame = client.receive_response().expect("prepare-vote response");
    assert_eq!(frame.message_kind, NodeMessageKind::Response as u16);
    let prepared = PreparedVoteTransactionV1::decode_body(&frame.body, &limits)
        .expect("typed prepared vote transaction");
    assert!(!prepared.canonical_vote.0.is_empty());
    assert!(!prepared.raw_transaction.0.is_empty());
    assert_ne!(prepared.transaction_hash, B256::ZERO);

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn ocm_vot_001_unavailable_or_invalid_vote_envelope_never_consumes_the_inner_vote_slot() {
    let limits = protocol_limits();
    let (committee, exported, result) = fixture();
    let canonical_result = result.encode_canonical(&limits).expect("canonical result");

    for request in [
        PrepareVoteTransactionV1 {
            canonical_result: BoundedBytes(canonical_result.clone()),
            nonce: 1,
            max_fee_per_gas: U256::from(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS - 1),
            gas_limit: outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
        },
        PrepareVoteTransactionV1 {
            canonical_result: BoundedBytes(canonical_result.clone()),
            nonce: 1,
            max_fee_per_gas: U256::from(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS),
            gas_limit: outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT - 1,
        },
    ] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let authority = Arc::new(StaticAuthority {
            value: Mutex::new(exported.clone()),
            reloads: AtomicUsize::new(0),
        });
        let gate = attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        );
        let server = base_control_server(&directory)
            .with_attestation(Arc::new(gate))
            .with_vote_transaction_signer(Arc::new(
                OutbeEvmSigner::from_secret_bytes([9; 32]).expect("validator EVM signer"),
            ));
        assert!(matches!(
            server.prepare_vote_transaction(&request),
            Err(NodeControlError::Protocol(_))
        ));
        assert_no_sign_once_record(&directory);
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported),
        reloads: AtomicUsize::new(0),
    });
    let gate = attestation_gate(
        &directory,
        authority,
        Arc::new(AtomicHeightSource::new(105)),
        committee,
    );
    let server = base_control_server(&directory).with_attestation(Arc::new(gate));
    assert!(matches!(
        server.prepare_vote_transaction(&PrepareVoteTransactionV1 {
            canonical_result: BoundedBytes(canonical_result),
            nonce: 1,
            max_fee_per_gas: U256::from(outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS,),
            gas_limit: outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
        }),
        Err(NodeControlError::VoteTransactionSignerUnavailable)
    ));
    assert_no_sign_once_record(&directory);
}

#[test]
fn ocm_sig_001_unavailable_signing_disables_only_attestation_on_the_node_control_session() {
    let limits = protocol_limits();
    let (_, _, result) = fixture();
    let directory = tempfile::tempdir().expect("temporary directory");
    let server = Arc::new(base_control_server(&directory));
    let (node_stream, supervisor_stream) =
        UnixStream::pair().expect("real local control socket pair");
    let server_thread = thread::spawn(move || server.serve_connection(node_stream));
    let mut client = ControlClientSession::connect(
        supervisor_stream,
        ClientPolicy::supervisor_to_node(
            effective_uid().expect("effective uid"),
            EndpointIdentity {
                chain_id: 42,
                genesis_hash: hash(40),
                boot_nonce: hash(99),
                protocol_bundle_hash: hash(41),
            },
            limits,
        ),
    )
    .expect("connect supervisor control session");
    client.handshake().expect("control handshake");
    client
        .send_request(
            NodeMessageKind::RequestAttestation as u16,
            RequestAttestationV1 {
                canonical_result: BoundedBytes(
                    result.encode_canonical(&limits).expect("canonical result"),
                ),
            }
            .encode_body(&limits)
            .expect("attestation request"),
        )
        .expect("send unavailable attestation request");
    let unavailable_frame = client.receive_response().expect("unavailable response");
    assert_eq!(
        unavailable_frame.message_kind,
        NodeMessageKind::Error as u16
    );
    let unavailable = LocalErrorV1::decode_body(&unavailable_frame.body, &limits)
        .expect("typed unavailable error");
    assert_eq!(
        unavailable.error_code,
        LocalErrorCode::InternalOcompUnavailable as u16
    );
    assert!(unavailable.retryable);

    client
        .send_request(
            NodeMessageKind::ListFinalizedJobs as u16,
            ListFinalizedJobsV1 {
                after_cursor: 0,
                limit: MAX_FINALIZED_JOBS_PER_RESPONSE,
            }
            .encode_body(&limits)
            .expect("list request"),
        )
        .expect("send unaffected control request");
    let list_frame = client.receive_response().expect("list response");
    assert_eq!(list_frame.message_kind, NodeMessageKind::Response as u16);
    let listing = ListFinalizedJobsResponseV1::decode_body(&list_frame.body, &limits)
        .expect("list finalized jobs response");
    assert!(listing.jobs.is_empty());

    drop(client);
    server_thread
        .join()
        .expect("control server did not panic")
        .expect("control server stopped cleanly");
}

#[test]
fn ocm_sig_001_node_refuses_wrong_export_deadline_bundle_committee_and_arithmetic() {
    let (committee, exported, result) = fixture();

    let wrong_export = Arc::new(StaticAuthority {
        value: Mutex::new(ExportedAttestationAuthorityV1 {
            manifest_hash: hash(200),
            ..exported.clone()
        }),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            wrong_export,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("exported result authority"))
    ));
    assert_no_sign_once_record(&directory);

    let stale_authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            stale_authority,
            Arc::new(AtomicHeightSource::new(110)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::DeadlineReached { .. })
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_bundle = result.clone();
    wrong_bundle.protocol_bundle_hash = hash(201);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(wrong_bundle),
        Err(AttestationError::Binding("exported result authority"))
    ));
    assert_no_sign_once_record(&directory);

    let mut wrong_committee_export = exported.clone();
    wrong_committee_export
        .finalized_intent
        .result_committee_snapshot_hash = hash(202);
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_committee_export),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent network"))
    ));
    assert_no_sign_once_record(&directory);

    let mut wrong_open_height = exported.clone();
    wrong_open_height.open_height = wrong_open_height
        .finality_recorded_height
        .checked_add(3)
        .expect("fixture height");
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_open_height),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent pin"))
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_arithmetic = result;
    wrong_arithmetic.arithmetic_commitment = hash(203);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee,
        )
        .attest(wrong_arithmetic),
        Err(AttestationError::Protocol(ProtocolError::InvalidInvariant(
            "arithmetic commitment"
        )))
    ));
    assert_no_sign_once_record(&directory);
}

#[test]
fn ocm_sig_001_node_refuses_wrong_intent_result_and_changed_authority_before_signing() {
    let (committee, exported, result) = fixture();

    let mut wrong_pin = exported.clone();
    wrong_pin.candidate.intent_id = hash(210);
    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(wrong_pin),
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(result.clone()),
        Err(AttestationError::Binding("finalized intent pin"))
    ));
    assert_no_sign_once_record(&directory);

    let authority = Arc::new(StaticAuthority {
        value: Mutex::new(exported.clone()),
        reloads: AtomicUsize::new(0),
    });
    let mut wrong_result = result.clone();
    wrong_result.attempt = wrong_result.attempt.saturating_add(1);
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee.clone(),
        )
        .attest(wrong_result),
        Err(AttestationError::Protocol(ProtocolError::InvalidInvariant(
            "result finalized intent binding"
        )))
    ));
    assert_no_sign_once_record(&directory);

    let mut changed = exported.clone();
    changed.manifest_hash = hash(211);
    let authority = Arc::new(ChangingAuthority {
        first: exported,
        second: changed,
        reloads: AtomicUsize::new(0),
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    assert!(matches!(
        attestation_gate(
            &directory,
            authority,
            Arc::new(AtomicHeightSource::new(105)),
            committee,
        )
        .attest(result),
        Err(AttestationError::AuthorityChanged)
    ));
    assert_no_sign_once_record(&directory);
}
