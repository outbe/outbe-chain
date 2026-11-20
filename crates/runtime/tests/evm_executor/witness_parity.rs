use super::*;
use alloy_primitives::{B256, U256};
use std::sync::Arc;

#[test]
fn executor_three_mode_parity_and_tx_tracer_detached_replay_isolation() {
    use super::fixtures::{
        begin_system_txs_for_test, dummy_pubkey, post_state_root,
        state_with_active_validators_seeded, test_chain_spec, test_evm_env, test_evm_signer,
        BeginBlockFixture, BootstrapFixture, CHAIN_ID,
    };
    use alloy_consensus::TxEip1559;
    use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
    use alloy_evm::{block::BlockExecutor, tracing::TxTracer, Evm as _};
    use alloy_primitives::TxKind;
    use alloy_sol_types::SolCall;
    use outbe_evm::config::OutbeEvmConfig;
    use outbe_nodfactory::precompile::INodFactory;
    use outbe_system_common::addresses::{NOD_FACTORY_ADDRESS, REWARDS_ADDRESS};
    use reth_evm::ConfigureEvm;
    use reth_primitives_traits::SignedTransaction as _;
    use revm::database::states::bundle_state::BundleRetention;

    outbe_chain_constants::initialize(None).unwrap();
    let _ = outbe_consensus::proof::init_consensus_chain_id(CHAIN_ID);

    let leader = resident_context(1);
    let verifier = resident_context(2);
    let leader_pk = leader.attestation_public();
    assert_ne!(leader_pk, verifier.attestation_public());
    let nod_state_key = outbe_core_common::confidential_tee::derive_resident_state_key(
        &leader,
        &outbe_core_common::confidential_tee::NOD,
        0,
    )
    .expect("nod state key");

    let signer = test_evm_signer();
    let proposer = signer.address();
    let super::witness_quote_nod::QuoteNodSeed {
        nod_item,
        nod_id,
        user_signer,
        user_sender,
        settlement_asset,
    } = super::witness_quote_nod::quote_nod_seed(&nod_state_key, CHAIN_ID);

    let user_tx = user_signer
        .sign_eip1559(TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 200_000,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(NOD_FACTORY_ADDRESS),
            value: U256::ZERO,
            input: INodFactory::quoteSettlementCall {
                nodId: nod_id,
                asset: settlement_asset,
            }
            .abi_encode()
            .into(),
            access_list: Default::default(),
        })
        .unwrap()
        .try_into_recovered()
        .unwrap();

    // Minimal EVM contract returning 6 on `decimals()` (0x313ce567) and 840 on `isoCode()`.
    let asset_bytecode = revm::state::Bytecode::new_raw(alloy_primitives::Bytes::from_static(&[
        0x5f, 0x35, 0x60, 0xe0, 0x1c, 0x63, 0x31, 0x3c, 0xe5, 0x67, 0x14, 0x60, 0x17, 0x57, 0x61,
        0x03, 0x48, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3, 0x5b, 0x60, 0x06, 0x5f, 0x52, 0x60, 0x20,
        0x5f, 0xf3,
    ]));

    let build_seeded_state = || {
        let mut state =
            state_with_active_validators_seeded(&[(proposer, dummy_pubkey(0xA2))], |storage| {
                // Seeding decrypts the resident ciphertext. A block policy with no
                // witness scope refuses that read, and the stand-in key cannot open it.
                let policy = outbe_tee::witness::witness_policy();
                outbe_tee::witness::set_witness_policy_for_test(None);
                outbe_nod::test_support::uninstall();
                let _seed_runtime =
                    outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(
                        Arc::clone(&leader),
                    ));
                outbe_nod::test_support::add(&storage, &nod_item).unwrap();
                drop(_seed_runtime);
                outbe_tee::witness::set_witness_policy_for_test(policy);
                outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
                    .asset_vault_set(settlement_asset)
                    .insert(Address::repeat_byte(0x99))
                    .unwrap();
                super::seed_valid_tee_nodes(storage.clone(), &[proposer]);
                let registry = outbe_teeregistry::TeeRegistry::new(storage);
                let node_hash = registry.validator_v1_node_hash.read(&proposer).unwrap();
                registry
                    .v1_node_attestation_ed25519
                    .write(&node_hash, B256::from(leader_pk))
                    .unwrap();
            });
        outbe_nod::test_support::uninstall();
        state.database.insert_account_info(
            settlement_asset,
            revm::state::AccountInfo {
                code_hash: asset_bytecode.hash_slow(),
                code: Some(asset_bytecode.clone()),
                ..Default::default()
            },
        );
        state.database.insert_account_info(
            user_sender,
            revm::state::AccountInfo {
                balance: U256::from(10_000_000u64),
                ..Default::default()
            },
        );
        state
    };

    let chain_spec = test_chain_spec();
    let config = OutbeEvmConfig::new(
        chain_spec,
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    )
    .with_evm_signer(signer);
    let parent_hash = B256::with_last_byte(0x11);
    let (_, metadata) = super::block_two_execution_ctx(None, Bytes::new(), proposer, parent_hash);
    let begin = begin_system_txs_for_test(
        &config,
        BeginBlockFixture {
            block_number: 2,
            parent_hash,
            extra_data: &Bytes::new(),
            parent_consensus_metadata: Some(metadata),
            proposer,
            bootstrap: BootstrapFixture::StandardForBlock,
        },
    );

    let run_block_in_mode = |is_verifier: bool, extra_data: Bytes| {
        let mut state = build_seeded_state();
        let evm = config.evm_with_env(&mut state, test_evm_env(2, REWARDS_ADDRESS));
        let (mut ctx, _) = super::block_two_execution_ctx(
            Some(begin.len() + 1),
            extra_data,
            proposer,
            parent_hash,
        );
        if is_verifier {
            ctx.block_hash = Some(B256::repeat_byte(0x9A));
            ctx.expected_begin_system_txs = begin.clone();
        }
        let mut executor = config.create_executor(evm, ctx);
        outbe_evm::executor::with_phase1_verify_disabled(|| {
            executor.apply_pre_execution_changes().unwrap();
        });
        for tx in begin.clone() {
            executor.execute_transaction(tx).unwrap();
        }
        executor.execute_transaction(user_tx.clone()).unwrap();
        executor.apply_end_block_phase().unwrap();
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
    let _leader_runtime = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(
        Some(Arc::clone(&leader)),
    );
    let proposer_out = run_block_in_mode(false, Bytes::new());
    let decoded_artifacts = decode_outbe_block_artifacts(proposer_out.2.as_ref()).unwrap();
    let witness_bytes = decoded_artifacts
        .enclave_witness
        .expect("proposer block must carry OART 0x0A enclave_witness");
    let decoded_tape = decode_witness_payload_v1(&witness_bytes).unwrap();
    assert_eq!(decoded_tape.txs.len(), 1);
    assert_eq!(decoded_tape.txs[0].tx_index, begin.len() as u32);

    // 2. VerifierWithEnclave (distinct attestation key, same resident state key)
    drop(_leader_runtime);
    let _verifier_runtime =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(verifier));
    let verifier_enclave_out = run_block_in_mode(true, proposer_out.2.clone());
    assert_eq!(proposer_out, verifier_enclave_out);

    // 3. VerifierReplay (keyless fullnode: no local enclave hook installed)
    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));
    drop(_verifier_runtime);
    let _absent = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(None);
    let verifier_replay_out = run_block_in_mode(true, proposer_out.2.clone());
    assert_eq!(proposer_out, verifier_replay_out);

    // 4. Real TxTracer + transact_commit replay of begin-zone system txs + enclave user tx,
    // followed by same-thread eth_call isolation check.
    let mut trace_state = build_seeded_state();
    {
        let preexec_evm = config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
        let (mut ctx, _) = super::block_two_execution_ctx(
            Some(begin.len() + 1),
            proposer_out.2.clone(),
            proposer,
            parent_hash,
        );
        ctx.block_hash = Some(B256::repeat_byte(0x9A));
        ctx.expected_begin_system_txs = begin.clone();
        let mut temp_executor = config.create_executor(preexec_evm, ctx);
        outbe_evm::executor::with_phase1_verify_disabled(|| {
            temp_executor.apply_pre_execution_changes().unwrap();
        });
        // Dropped without finish(), mirroring Reth Trace::apply_pre_execution_changes.
    }
    {
        let trace_evm = config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
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
        let mut call_evm = config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
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

    outbe_evm::executor::witness::clear_detached_trace_witness();
    outbe_tee::witness::set_witness_policy_for_test(None);
}

fn resident_context(seed: u8) -> Arc<outbe_tee::in_process::InProcessTeeContext> {
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::resident::ResidentOfferMaterial;
    let binding = NetworkBindingV1 {
        chain_id: U256::from(super::fixtures::CHAIN_ID).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x22),
        attestation_mode: AttestationMode::NoAttestationDev,
    };
    let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding));
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                b"executor-nod-quote-group-signature-v1!".to_vec(),
                B256::from(binding.chain_id),
                0,
                0,
            )
            .expect("offer material"),
        )
        .expect("activate offer");
    context
}
