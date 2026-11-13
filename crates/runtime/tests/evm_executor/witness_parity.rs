use super::*;
use alloy_primitives::{B256, U256};
use ed25519_dalek::{Signer, SigningKey};
use outbe_enclave::protocol::EnclaveResponse;
use std::sync::Arc;

fn sign_nod_read(key: &SigningKey, inputs_hash: B256, amount: U256) -> EnclaveResponse {
    let preimage = outbe_enclave::nod_mine::nod_read_preimage(inputs_hash, amount);
    let sig = key.sign(&preimage);
    EnclaveResponse::NodAmountReadV1 {
        amount,
        inputs_canonical_hash: inputs_hash,
        attestation_tag: sig.to_bytes().to_vec(),
    }
}

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
    use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, REWARDS_ADDRESS};
    use outbe_system_common::time::WorldwideDay;
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
    let issued_at = outbe_system_common::time::date_key_to_utc_timestamp(20_200_101).unwrap() + 1;
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::DEV,
        issued_at,
        entry_price_minor,
        840,
    );
    let settlement_asset = Address::repeat_byte(0x71);
    let user_signer =
        outbe_primitives::signer::OutbeEvmSigner::from_secret_bytes([2u8; 32]).unwrap();
    let user_sender = user_signer.address();
    let nod_id = outbe_nod::test_support::nod_id_of(user_sender, worldwide_day.value(), 840, 840);
    let nod_item = {
        let _enclave = outbe_nod::test_support::scope();
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id,
                owner: user_sender,
                gratis_load_minor: U256::from(1_000_000u64),
                emission_day: worldwide_day.value(),
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
                let _enclave = outbe_nod::test_support::scope();
                outbe_nod::test_support::add(&storage, &nod_item).unwrap();
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

    let make_enclave_hook =
        |sk: SigningKey| -> outbe_enclave::tribute_client::TestLocalEnclaveHook {
            let pk = sk.verifying_key().to_bytes();
            Arc::new(move |req| match req {
                outbe_enclave::protocol::EnclaveRequest::ReadNodAmountV1 { nod } => {
                    let hash = outbe_enclave::nod_mine::nod_read_hash(nod)
                        .map_err(outbe_tee::TransportError::codec)?;
                    let resp = sign_nod_read(&sk, hash, U256::from(1_000_000u64));
                    Ok((pk, resp))
                }
                outbe_enclave::protocol::EnclaveRequest::EncryptNodQuoteV1 {
                    nod,
                    context,
                    payment_minor,
                } => {
                    let inputs_canonical_hash = outbe_enclave::response_auth::hash(
                        b"outbe/nod/quote-encrypt-inputs/v1",
                        &(nod.as_ref(), context, *payment_minor),
                    )
                    .map_err(outbe_tee::TransportError::codec)?;
                    let encrypted_payment =
                        vec![0x42u8; outbe_core_common::nod_encryption::NOD_BLOB_LEN];
                    let preimage = outbe_enclave::response_auth::attestation(
                        b"outbe/nod/quote-encrypt-result/v1",
                        inputs_canonical_hash,
                        &encrypted_payment,
                    )
                    .map_err(outbe_tee::TransportError::codec)?;
                    let sig = sk.sign(&preimage);
                    Ok((
                        pk,
                        EnclaveResponse::NodQuoteEncryptedV1 {
                            encrypted_payment,
                            inputs_canonical_hash,
                            attestation_tag: sig.to_bytes().to_vec(),
                        },
                    ))
                }
                _ => Err(outbe_tee::TransportError::UnexpectedResponse),
            })
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
    outbe_enclave::tribute_client::set_test_local_enclave_hook(Some(make_enclave_hook(
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
    outbe_enclave::tribute_client::set_test_local_enclave_hook(Some(make_enclave_hook(
        validator_sk,
    )));
    let verifier_enclave_out = run_block_in_mode(true, proposer_out.2.clone());
    assert_eq!(proposer_out, verifier_enclave_out);

    // 3. VerifierReplay (keyless fullnode: no local enclave hook installed)
    outbe_tee::witness::set_witness_policy_for_test(Some(
        outbe_tee::witness::WitnessPolicy::ReplayOnly,
    ));
    outbe_enclave::tribute_client::set_test_local_enclave_hook(None);
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
    outbe_enclave::tribute_client::set_test_local_enclave_hook(None);
    outbe_tee::witness::set_witness_policy_for_test(None);
}
