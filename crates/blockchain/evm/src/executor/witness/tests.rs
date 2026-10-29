use super::*;
use ed25519_dalek::{Signer, SigningKey};
use outbe_primitives::reshare_artifact::OutbeBlockArtifacts;
use outbe_tee::{
    protocol::EnclaveResponse,
    witness::{
        consensus_call, decode_witness_payload_v1, encode_witness_payload_v1, EnclaveWitnessEntryV1,
    },
    TransportError,
};
use reth_ethereum::node::api::ConfigureEvm;

fn sign_nod_read(key: &SigningKey, inputs_hash: B256, amount: U256) -> EnclaveResponse {
    let preimage = outbe_tee::nod_mine::nod_read_preimage(inputs_hash, amount);
    let sig = key.sign(&preimage);
    EnclaveResponse::NodAmountReadV1 {
        amount,
        inputs_canonical_hash: inputs_hash,
        attestation_tag: sig.to_bytes().to_vec(),
    }
}

fn verify_nod_read(
    pub_key: &[u8; 32],
    expected_hash: B256,
    response: &EnclaveResponse,
) -> Result<(), TransportError> {
    let EnclaveResponse::NodAmountReadV1 {
        amount,
        inputs_canonical_hash,
        attestation_tag,
    } = response
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    if *inputs_canonical_hash != expected_hash {
        return Err(TransportError::Attestation("hash mismatch".into()));
    }
    let preimage = outbe_tee::nod_mine::nod_read_preimage(expected_hash, *amount);
    outbe_tee::tribute_v2::verify_attestation(pub_key, &preimage, attestation_tag)
}

#[test]
fn reuses_prewarm_results_is_pinned_false() {
    let _ = outbe_consensus::proof::init_consensus_chain_id(424242);
    let chain_id = outbe_consensus::proof::consensus_chain_id();
    let spec = ChainSpec {
        chain: reth_ethereum::chainspec::Chain::from_id(chain_id),
        ..Default::default()
    };
    let config = crate::config::OutbeEvmConfig::new(Arc::new(spec));
    assert!(
        !config.reuses_prewarm_results(),
        "OutbeEvmConfig must never reuse speculative prewarm results without witness commit conditions"
    );
}

#[test]
fn block_one_and_mandatory_prefix_worst_case_fixture_fits_budget() {
    let leader_sk = SigningKey::from_bytes(&[0x11; 32]);
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        timestamp_millis_part: 500,
        ..Default::default()
    })
    .unwrap();

    let mut tape = EnclaveWitnessTapeV1::default();
    for idx in 0..8u8 {
        let resp = sign_nod_read(
            &leader_sk,
            B256::repeat_byte(idx.wrapping_add(1)),
            U256::from(1000u64 + u64::from(idx)),
        );
        tape.prefix
            .push(EnclaveWitnessEntryV1::try_from(resp).unwrap());
    }
    let mut bootstrap_entries = Vec::with_capacity(140);
    for idx in 0..140u16 {
        let request_hash = keccak256(idx.to_be_bytes());
        let mut outcome = Vec::with_capacity(280);
        outcome.extend_from_slice(&[0xAA; 96]);
        outcome.extend_from_slice(
            b"INTEL-SA-00334,INTEL-SA-00615,INTEL-SA-00657,INTEL-SA-00828,INTEL-SA-01045",
        );
        outcome.extend_from_slice(&idx.to_le_bytes());
        outcome.resize(280, 0x5A);
        let preimage = outbe_tee::dcap_protocol::dcap_verification_attestation_preimage(
            request_hash,
            &outcome,
        )
        .unwrap();
        let sig = leader_sk.sign(&preimage);
        bootstrap_entries.push(EnclaveWitnessEntryV1::DcapVerificationFinishedV1 {
            request_hash,
            outcome,
            attestation_tag: sig.to_bytes().to_vec(),
        });
    }
    tape.txs.push(TxWitnessV1 {
        tx_index: 0,
        entries: bootstrap_entries,
    });

    let raw = postcard::to_allocvec(&tape).unwrap();
    assert!(raw.len() <= MAX_DECOMPRESSED_WITNESS_BYTES);
    assert!(tape_fits_budget(&tape, &base_extra));
    let encoded = encode_witness_payload_v1(&tape).unwrap();
    assert!(encoded.len().saturating_add(3) <= ENCLAVE_WITNESS_RESERVED_BYTES);
    let decoded = decode_witness_payload_v1(&encoded).unwrap();
    assert_eq!(decoded, tape);
}

#[test]
fn proposer_record_verifier_with_enclave_and_verifier_replay_roundtrip() {
    let leader_sk = SigningKey::from_bytes(&[0x21; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let validator_sk = SigningKey::from_bytes(&[0x22; 32]);
    let validator_pk = validator_sk.verifying_key().to_bytes();
    assert_ne!(leader_pk, validator_pk);

    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
    let prefix_hash = B256::repeat_byte(0x31);
    let tx_hash = B256::repeat_byte(0x32);

    // 1. ProposerRecord
    let proposer_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader_pk),
        EnclaveWitnessTapeV1::default(),
    )));
    {
        let _scope = WitnessExecutionScope::enter(proposer_state.clone());
        let (_, resp) = consensus_call(
            "prefix_read",
            || {
                Ok((
                    leader_pk,
                    sign_nod_read(&leader_sk, prefix_hash, U256::from(42)),
                ))
            },
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
        assert!(
            matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(42))
        );
    }
    {
        let mut guard = proposer_state.lock().unwrap();
        let staged = std::mem::take(&mut guard.staged);
        guard.tape.prefix.extend(staged);
        guard.tx_index = Some(2);
        guard.is_user_candidate = true;
    }
    {
        let _scope = WitnessExecutionScope::enter(proposer_state.clone());
        consensus_call(
            "tx_read",
            || {
                Ok((
                    leader_pk,
                    sign_nod_read(&leader_sk, tx_hash, U256::from(99)),
                ))
            },
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&proposer_state),
        2,
        &base_extra,
        true,
    ));
    let encoded_payload = {
        let mut guard = proposer_state.lock().unwrap();
        let entries = std::mem::take(&mut guard.staged);
        guard.tape.txs.push(TxWitnessV1 {
            tx_index: 2,
            entries,
        });
        encode_witness_payload_v1(&guard.tape).unwrap()
    };

    // 2. VerifierWithEnclave (validator with distinct enclave key)
    let decoded_tape = decode_witness_payload_v1(&encoded_payload).unwrap();
    let verifier_enclave_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierWithEnclave,
        Some(leader_pk),
        decoded_tape.clone(),
    )));
    {
        let _scope = WitnessExecutionScope::enter(verifier_enclave_state.clone());
        consensus_call(
            "prefix_read",
            || {
                Ok((
                    validator_pk,
                    sign_nod_read(&validator_sk, prefix_hash, U256::from(42)),
                ))
            },
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
    }
    {
        let mut guard = verifier_enclave_state.lock().unwrap();
        assert_eq!(guard.prefix_cursor, guard.tape.prefix.len());
        guard.tx_index = Some(2);
        guard.replay_cursor = 0;
    }
    {
        let _scope = WitnessExecutionScope::enter(verifier_enclave_state.clone());
        consensus_call(
            "tx_read",
            || {
                Ok((
                    validator_pk,
                    sign_nod_read(&validator_sk, tx_hash, U256::from(99)),
                ))
            },
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&verifier_enclave_state),
        2,
        &base_extra,
        false,
    ));

    // 3. VerifierReplay (keyless fullnode: transport closure must never execute)
    let replay_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader_pk),
        decoded_tape,
    )));
    {
        let _scope = WitnessExecutionScope::enter(replay_state.clone());
        consensus_call(
            "prefix_read",
            || panic!("VerifierReplay must never call local transport"),
            |pk, r| verify_nod_read(pk, prefix_hash, r),
        )
        .unwrap();
    }
    {
        let mut guard = replay_state.lock().unwrap();
        guard.tx_index = Some(2);
        guard.replay_cursor = 0;
    }
    {
        let _scope = WitnessExecutionScope::enter(replay_state.clone());
        let (_, resp) = consensus_call(
            "tx_read",
            || panic!("VerifierReplay must never call local transport"),
            |pk, r| verify_nod_read(pk, tx_hash, r),
        )
        .unwrap();
        assert!(
            matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(99))
        );
    }
    assert!(OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
        Some(&replay_state),
        2,
        &base_extra,
        false,
    ));
}

#[test]
fn user_candidate_exceeding_witness_budget_sets_budget_deferred_and_rolls_back() {
    let leader_sk = SigningKey::from_bytes(&[0x41; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();

    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader_pk),
        EnclaveWitnessTapeV1::default(),
    )));
    {
        let mut guard = state.lock().unwrap();
        guard.tx_index = Some(0);
        guard.is_user_candidate = true;
        // Stage an entry whose uncompressed size exceeds MAX_DECOMPRESSED_WITNESS_BYTES (262_144)
        guard.staged.push(
            EnclaveWitnessEntryV1::DcapOnboardingVerificationFinishedV1 {
                request_hash: B256::repeat_byte(0x55),
                outcome: vec![0xAA; 150_000],
                onboarding_artifact: vec![0xBB; 150_000],
                attestation_tag: vec![0xCC; 64],
            },
        );
    }
    assert!(
        !OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
            Some(&state),
            0,
            &base_extra,
            true,
        ),
        "oversized user candidate must be denied commit"
    );
    let guard = state.lock().unwrap();
    assert_eq!(
        guard.candidate_rejection,
        Some(CandidateRejection::BudgetDeferred)
    );
    assert_eq!(guard.first_witness_fault, None);
    drop(guard);

    let deferral_err = crate::enclave_witness_budget_deferral(B256::repeat_byte(0x99));
    let BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) =
        deferral_err
    else {
        panic!("expected InvalidTx validation error");
    };
    assert!(crate::is_enclave_witness_budget_deferral(error.as_ref()));
}

#[test]
fn verifier_unconsumed_tx_witness_entries_latch_tape_invalid_on_commit_check() {
    let leader_sk = SigningKey::from_bytes(&[0x61; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let base_extra = encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
    let resp = sign_nod_read(&leader_sk, B256::repeat_byte(0x62), U256::from(1));
    let tape = EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: vec![TxWitnessV1 {
            tx_index: 0,
            entries: vec![EnclaveWitnessEntryV1::try_from(resp).unwrap()],
        }],
    };
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader_pk),
        tape,
    )));
    assert!(
        !OutbeBlockExecutor::<()>::check_witness_commit_allowed_with(
            Some(&state),
            0,
            &base_extra,
            false,
        ),
        "unconsumed witness entry at tx_index=0 must reject commit"
    );
    assert_eq!(
        state.lock().unwrap().first_witness_fault,
        Some(BlockWitnessFault::TapeInvalid)
    );
}

#[test]
fn witness_fault_to_block_error_preserves_typed_witness_abort_reason() {
    for (fault, expected) in [
        (
            BlockWitnessFault::TapeInvalid,
            WitnessAbortReason::TapeInvalid,
        ),
        (
            BlockWitnessFault::SemanticMismatch,
            WitnessAbortReason::SemanticMismatch,
        ),
        (
            BlockWitnessFault::LocalUnavailable,
            WitnessAbortReason::LocalUnavailable,
        ),
    ] {
        let err = witness_fault_to_block_error(fault, "test detail");
        let found = NodeLocalExecutionAbort::find(&err)
            .expect("NodeLocalExecutionAbort must be discoverable in error chain");
        assert_eq!(found.witness_reason, Some(expected));
        assert_eq!(found.reason, "test detail");
    }
}

#[test]
fn evm_transact_witness_scope_covers_rpc_direct_prewarm_and_detached_trace() {
    let leader_sk = SigningKey::from_bytes(&[0x71; 32]);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let hash0 = B256::repeat_byte(0x72);
    let hash2 = B256::repeat_byte(0x73);
    let entry0 =
        EnclaveWitnessEntryV1::try_from(sign_nod_read(&leader_sk, hash0, U256::from(11))).unwrap();
    let entry2 =
        EnclaveWitnessEntryV1::try_from(sign_nod_read(&leader_sk, hash2, U256::from(22))).unwrap();

    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));

    // 1. Prewarm (precompiles_mut_called = true) leaves current_witness_state() as None and fails closed.
    super::clear_detached_trace_witness();
    super::with_evm_transact_witness_scope(
        super::EvmTransactWitnessInput {
            attached_state: None,
            owned_by_executor: false,
            precompiles_mut_called: true,
            chain_id: 424242,
            block_number: 5,
            block_timestamp: 1000,
        },
        || {
            assert!(outbe_tee::call_context::current_witness_state().is_none());
            let err = consensus_call(
                "prewarm_call",
                || panic!("prewarm must not reach transport"),
                |pk, r| verify_nod_read(pk, hash0, r),
            )
            .unwrap_err();
            assert!(err.is_node_local());
        },
    );

    // 2. Direct RPC (owned_by_executor = false, precompiles_mut_called = false) enters RpcDirect.
    super::with_evm_transact_witness_scope(
        super::EvmTransactWitnessInput {
            attached_state: None,
            owned_by_executor: false,
            precompiles_mut_called: false,
            chain_id: 424242,
            block_number: 5,
            block_timestamp: 1000,
        },
        || {
            let state = outbe_tee::call_context::current_witness_state()
                .expect("direct RPC must enter RpcDirect scope");
            assert_eq!(state.lock().unwrap().mode, WitnessExecutionMode::RpcDirect);
            let err = consensus_call(
                "rpc_direct_keyless",
                || panic!("keyless RpcDirect must not reach transport"),
                |pk, r| verify_nod_read(pk, hash0, r),
            )
            .unwrap_err();
            assert!(err.to_string().contains("PrivateExecutionUnavailable"));
        },
    );

    // 3. Detached historical trace witness replays tx_index=0, skips non-enclave tx_index=1, and replays tx_index=2.
    let trace_state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader_pk),
        EnclaveWitnessTapeV1 {
            prefix: Vec::new(),
            txs: vec![
                TxWitnessV1 {
                    tx_index: 0,
                    entries: vec![entry0],
                },
                TxWitnessV1 {
                    tx_index: 2,
                    entries: vec![entry2],
                },
            ],
        },
    )));
    super::install_detached_trace_witness(424242, 5, 1000, Some(trace_state.clone()));

    let rpc_input = super::EvmTransactWitnessInput {
        attached_state: None,
        owned_by_executor: false,
        precompiles_mut_called: false,
        chain_id: 424242,
        block_number: 5,
        block_timestamp: 1000,
    };
    // tx_index = 0
    super::with_evm_transact_witness_scope(rpc_input, || {
        let (_, resp) = consensus_call(
            "trace_tx0",
            || panic!("VerifierReplay must not reach transport"),
            |pk, r| verify_nod_read(pk, hash0, r),
        )
        .unwrap();
        assert!(
            matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(11))
        );
    });
    // tx_index = 1 (no enclave calls)
    super::with_evm_transact_witness_scope(
        super::EvmTransactWitnessInput {
            attached_state: None,
            owned_by_executor: false,
            precompiles_mut_called: false,
            chain_id: 424242,
            block_number: 5,
            block_timestamp: 1000,
        },
        || {},
    );
    // tx_index = 2
    super::with_evm_transact_witness_scope(
        super::EvmTransactWitnessInput {
            attached_state: None,
            owned_by_executor: false,
            precompiles_mut_called: false,
            chain_id: 424242,
            block_number: 5,
            block_timestamp: 1000,
        },
        || {
            let (_, resp) = consensus_call(
                "trace_tx2",
                || panic!("VerifierReplay must not reach transport"),
                |pk, r| verify_nod_read(pk, hash2, r),
            )
            .unwrap();
            assert!(
                matches!(resp, EnclaveResponse::NodAmountReadV1 { amount, .. } if amount == U256::from(22))
            );
        },
    );
    assert_eq!(trace_state.lock().unwrap().tx_cursor, 2);

    super::clear_detached_trace_witness();
    outbe_tee::witness::set_witness_policy_for_test(None);
}

#[test]
fn executor_three_mode_parity_and_tx_tracer_detached_replay_isolation() {
    use crate::config::OutbeEvmConfig;
    use crate::executor::tests::fixtures::{
        begin_system_txs_for_test, block_one_execution_ctx, dummy_pubkey, post_state_root,
        state_with_active_validators_seeded, test_chain_spec, test_evm_env, test_evm_signer,
        BeginBlockFixture, BootstrapFixture, CHAIN_ID,
    };
    use alloy_consensus::TxEip1559;
    use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
    use alloy_evm::{block::BlockExecutor, tracing::TxTracer, Evm as _};
    use alloy_primitives::{Signature, TxKind};
    use alloy_sol_types::SolCall;
    use outbe_nod::precompile::INod;
    use outbe_primitives::{
        addresses::{NOD_ADDRESS, REWARDS_ADDRESS},
        time::WorldwideDay,
    };
    use reth_evm::ConfigureEvm;
    use reth_primitives_traits::SignedTransaction as _;
    use revm::database::states::bundle_state::BundleRetention;

    outbe_chain_constants::initialize(None).unwrap();
    let _ = outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID);

    let mut preimage = Vec::new();
    preimage.extend_from_slice(b"outbe/test/gramine-direct/attestation");
    preimage.extend_from_slice(&[1u8; 32]);
    preimage.extend_from_slice(&0u64.to_be_bytes());
    let leader_seed: [u8; 32] = alloy_primitives::keccak256(preimage).into();
    let leader_sk = SigningKey::from_bytes(&leader_seed);
    let leader_pk = leader_sk.verifying_key().to_bytes();
    let validator_sk = SigningKey::from_bytes(&[0x82; 32]);
    let validator_pk = validator_sk.verifying_key().to_bytes();
    assert_ne!(leader_pk, validator_pk);

    let signer = test_evm_signer();
    let proposer = signer.address();
    let worldwide_day = WorldwideDay::new(20_241_220);
    let entry_price_minor = U256::from(450_000_000u64);
    let issued_at = outbe_primitives::time::date_key_to_utc_timestamp(20_200_101) + 1;
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::DEV,
        issued_at,
        entry_price_minor,
        840,
    );
    let nod_id = outbe_nod::identity::generate_nod_id(proposer, worldwide_day);
    let nod_item = {
        let _enclave = outbe_nod::test_support::scope();
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id,
                owner: proposer,
                gratis_load_minor: U256::from(1_000_000u64),
                worldwide_day,
                league_id: 1,
                group_key,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at,
            },
            entry_price_minor,
        )
    };
    outbe_nod::test_support::uninstall();

    let user_tx: reth_ethereum::TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(NOD_ADDRESS),
        value: U256::ZERO,
        input: INod::getNodCall { nodId: nod_id }.abi_encode().into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let user_tx = user_tx.try_into_recovered().unwrap();
    let user_sender = Address(*user_tx.signer());

    let build_seeded_state = || {
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
                let _enclave = outbe_nod::test_support::scope();
                outbe_nod::test_support::add(&storage, &nod_item).unwrap();
            });
        outbe_nod::test_support::uninstall();
        state.database.insert_account_info(
            user_sender,
            revm::state::AccountInfo {
                balance: U256::from(10_000_000u64),
                ..Default::default()
            },
        );
        state
    };

    let make_enclave_hook = |sk: SigningKey| -> outbe_tee::tribute_client::TestLocalEnclaveHook {
        let pk = sk.verifying_key().to_bytes();
        Arc::new(move |req| match req {
            outbe_tee::protocol::EnclaveRequest::ReadNodAmountV1 { nod } => {
                let hash = outbe_tee::nod_mine::nod_read_hash(nod)
                    .map_err(outbe_tee::TransportError::codec)?;
                let resp = sign_nod_read(&sk, hash, U256::from(1_000_000u64));
                Ok((pk, resp))
            }
            _ => Err(outbe_tee::TransportError::UnexpectedResponse),
        })
    };

    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(chain_spec).with_evm_signer(signer);
    let begin = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 1,
            parent_hash: B256::ZERO,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: None,
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );

    let run_block_in_mode = |is_verifier: bool, extra_data: Bytes| {
        let mut state = build_seeded_state();
        let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
        let mut ctx = block_one_execution_ctx(Some(begin.len() + 1), extra_data);
        ctx.proposer_evm_address = Some(proposer);
        if is_verifier {
            ctx.block_hash = Some(B256::repeat_byte(0x9A));
            ctx.expected_begin_system_txs = begin.clone();
        }
        let mut executor = config.create_executor(evm, ctx);
        executor.apply_pre_execution_changes().unwrap();
        for tx in begin.clone() {
            executor.execute_transaction(tx).unwrap();
        }
        executor.execute_transaction(user_tx.clone()).unwrap();
        if !is_verifier {
            executor.prepare_final_header_artifacts(0).unwrap();
        }
        let final_extra_data = executor.final_extra_data.clone();
        let (evm, result) = executor.finish().unwrap();
        drop(evm);
        state.merge_transitions(BundleRetention::Reverts);
        let root = post_state_root(&state.bundle_state);
        (result.receipts, result.gas_used, final_extra_data, root)
    };

    // 1. ProposerRecord
    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::EnclaveBacked,
    ));
    outbe_tee::tribute_client::set_test_local_enclave_hook(Some(make_enclave_hook(
        leader_sk.clone(),
    )));
    let proposer_out = run_block_in_mode(false, Bytes::new());
    let decoded_artifacts = decode_outbe_block_artifacts(proposer_out.2.as_ref()).unwrap();
    let witness_bytes = decoded_artifacts
        .enclave_witness
        .expect("proposer block must carry OART 0x0A enclave_witness");
    let decoded_tape = decode_witness_payload_v1(&witness_bytes).unwrap();
    assert_eq!(decoded_tape.txs.len(), 1);
    assert_eq!(decoded_tape.txs[0].tx_index, begin.len() as u32);

    // 2. VerifierWithEnclave (validator with distinct enclave key validator_sk)
    outbe_tee::tribute_client::set_test_local_enclave_hook(Some(make_enclave_hook(validator_sk)));
    let verifier_enclave_out = run_block_in_mode(true, proposer_out.2.clone());
    assert_eq!(proposer_out, verifier_enclave_out);

    // 3. VerifierReplay (keyless fullnode: no local enclave hook installed)
    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));
    outbe_tee::tribute_client::set_test_local_enclave_hook(None);
    let verifier_replay_out = run_block_in_mode(true, proposer_out.2.clone());
    assert_eq!(proposer_out, verifier_replay_out);

    // 4. Real TxTracer + transact_commit replay of begin-zone system txs + enclave user tx,
    // followed by same-thread eth_call isolation check.
    let mut trace_state = build_seeded_state();
    {
        let preexec_evm = config.evm_with_env(&mut trace_state, test_evm_env(1, REWARDS_ADDRESS));
        let mut ctx = block_one_execution_ctx(Some(begin.len() + 1), proposer_out.2.clone());
        ctx.proposer_evm_address = Some(proposer);
        ctx.block_hash = Some(B256::repeat_byte(0x9A));
        ctx.expected_begin_system_txs = begin.clone();
        let mut temp_executor = config.create_executor(preexec_evm, ctx);
        temp_executor.apply_pre_execution_changes().unwrap();
        // Dropped without finish(), mirroring Reth Trace::apply_pre_execution_changes.
    }
    {
        let trace_evm = config.evm_with_env(&mut trace_state, test_evm_env(1, REWARDS_ADDRESS));
        let mut tracer = TxTracer::new(trace_evm);
        for tx in begin
            .iter()
            .cloned()
            .chain(std::iter::once(user_tx.clone()))
        {
            let tx_env = config.tx_env(&tx);
            let trace_out = tracer.trace(tx_env).expect("TxTracer replay must succeed");
            assert!(
                trace_out.result.is_success(),
                "traced transaction must succeed under detached witness replay"
            );
        }
    }
    // Subsequent eth_call on the same thread and same block must NOT match stale detached trace
    // witness (must enter RpcDirect and return PrivateExecutionUnavailable, not TapeInvalid).
    {
        let mut call_evm = config.evm_with_env(&mut trace_state, test_evm_env(1, REWARDS_ADDRESS));
        let mut call_tx_env = config.tx_env(&user_tx);
        call_tx_env.nonce = 1;
        let err = call_evm
            .transact_raw(call_tx_env)
            .expect_err("keyless eth_call to private precompile must fail in RpcDirect");
        assert!(
            err.to_string().contains("PrivateExecutionUnavailable"),
            "expected PrivateExecutionUnavailable, got: {err}"
        );
    }

    super::clear_detached_trace_witness();
    outbe_tee::tribute_client::set_test_local_enclave_hook(None);
    outbe_tee::witness::set_witness_policy_for_test(None);
}
