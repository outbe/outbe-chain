use super::*;
use alloy_primitives::{B256, U256};
use std::sync::Arc;

/// The witness entry `consensus_witness_call` records for `value`: its tagged tag-free body
/// beside the inputs hash and the signature.
fn witness_entry_len<T: outbe_core_common::confidential_tee::WitnessSchema>(value: &T) -> usize {
    use outbe_core_common::confidential_tee::{encode_tagged_payload, WitnessSchema};
    let mut body = value.clone();
    body.set_attestation_tag(Vec::new());
    let entry = outbe_tee::witness::EnclaveWitnessEntryV1::Payload {
        payload: encode_tagged_payload(T::TAG, &body).unwrap(),
        inputs_canonical_hash: WitnessSchema::inputs_canonical_hash(value),
        attestation_tag: WitnessSchema::attestation_tag(value).to_vec(),
    };
    postcard::to_allocvec(&entry).unwrap().len()
}

/// The end-block reserve holds the Nod forfeit and the Credis cleanup at their full block budget,
/// and the reserve itself fits the tape.
#[test]
fn the_end_block_reserve_holds_both_private_passes_at_their_full_budget() {
    use outbe_gratis::{GratisOpResult, GratisOpStatus};
    use outbe_system_common::pass_budget::{SWEEP_WRITES_PER_BLOCK, WITNESS_BYTES_PER_WRITE};
    use outbe_tee::witness::{EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1};
    // A `GRA2` balance or event envelope is 92 bytes.
    const GRATIS_BLOB_LEN: usize = 92;

    let nod_read_len = witness_entry_len(&outbe_nod::nod_host::NodGroupLoadReadV1 {
        amount: U256::MAX,
        inputs_canonical_hash: B256::repeat_byte(0x01),
        attestation_tag: vec![0x02; 64],
    });
    let pledge_burn_len = witness_entry_len(&GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: vec![0x03; GRATIS_BLOB_LEN],
        new_pledged: vec![0x04; GRATIS_BLOB_LEN],
        event_amount: U256::MAX,
        next_op_nonce: u64::MAX,
        fidelity: None,
        inputs_canonical_hash: B256::repeat_byte(0x05),
        attestation_tag: vec![0x06; 64],
        encrypted_event_amount: vec![0x07; GRATIS_BLOB_LEN],
    });
    assert!(nod_read_len <= outbe_nod::constants::GROUP_LOAD_READ_WITNESS_BYTES as usize);
    assert!(pledge_burn_len <= outbe_gratis::enclave_client::COLLATERAL_OP_WITNESS_BYTES as usize);

    // A write records at most this one entry's worth of witness.
    let unit = WITNESS_BYTES_PER_WRITE as usize;
    let entry = EnclaveWitnessEntryV1::Payload {
        payload: vec![0x08; unit - 100],
        inputs_canonical_hash: B256::repeat_byte(0x09),
        attestation_tag: vec![0x0A; 64],
    };
    assert_eq!(postcard::to_allocvec(&entry).unwrap().len(), unit);
    let suffix = EnclaveWitnessTapeV1 {
        prefix: Vec::new(),
        txs: (0..2)
            .map(|tx_index| TxWitnessV1 {
                tx_index,
                entries: vec![entry.clone(); SWEEP_WRITES_PER_BLOCK as usize],
            })
            .collect(),
    };
    assert!(
        postcard::to_allocvec(&suffix).unwrap().len()
            <= outbe_evm::executor::witness::END_BLOCK_WITNESS_RESERVE_BYTES
    );
    assert!(
        outbe_tee::witness::compressed_witness_bound(
            outbe_evm::executor::witness::END_BLOCK_WITNESS_RESERVE_BYTES
        ) + 3
            <= outbe_primitives::reshare_artifact::ENCLAVE_WITNESS_RESERVED_BYTES
    );
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
    use alloy_primitives::TxKind;
    use alloy_sol_types::SolCall;
    use outbe_evm::config::OutbeEvmConfig;
    use outbe_nodfactory::precompile::INodFactory;
    use outbe_system_common::addresses::{NOD_FACTORY_ADDRESS, REWARDS_ADDRESS};
    use reth_evm::BlockExecutor as _;
    use reth_primitives_traits::SignedTransaction as _;

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
    let asset_bytecode = Bytecode::new_legacy(alloy_primitives::Bytes::from_static(&[
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
        state.insert_account_info(
            &settlement_asset,
            AccountInfo {
                code_hash: asset_bytecode.hash_slow(),
                code: Some(asset_bytecode.clone()),
                ..Default::default()
            },
        );
        state.insert_account_info(
            &user_sender,
            AccountInfo {
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
            finalize_executor_header_artifacts(&mut executor, 0).unwrap();
        }
        let final_extra_data = executor.final_extra_data.clone();
        let output = executor.finish().unwrap();
        let root = post_state_root(&output.state);
        (
            output.result.receipts,
            output.result.gas_used,
            final_extra_data,
            root,
        )
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

    // 4. Real inspector replay of begin-zone system txs + enclave user tx,
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
        let mut block_state = evm2::evm::BlockStateAccumulator::default();
        {
            let mut trace_evm =
                config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
            let mut inspector = evm2::inspector::NoopInspector::default();
            for tx in begin
                .iter()
                .cloned()
                .chain(std::iter::once(user_tx.clone()))
            {
                let tx_env = tx_env(&config, &tx);
                let trace_out = trace_evm
                    .transact_with_inspector(&tx_env, &mut inspector)
                    .expect("inspector replay must succeed")
                    .commit_to(&mut block_state);
                assert!(
                    trace_out.is_success(),
                    "traced transaction must succeed under detached witness replay"
                );
            }
        }
        trace_state.commit_source(&block_state);
    }
    // Subsequent eth_call on the same thread and same block must NOT match stale detached trace
    // witness (must enter RpcDirect and return PrivateExecutionUnavailable, not TapeInvalid).
    {
        let mut call_evm = config.evm_with_env(&mut trace_state, test_evm_env(2, REWARDS_ADDRESS));
        let call_tx = user_signer
            .sign_eip1559(TxEip1559 {
                chain_id: CHAIN_ID,
                nonce: 1,
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
        let call_tx_env = tx_env(&config, &call_tx);
        let err = call_evm
            .transact(&call_tx_env)
            .expect_err("keyless eth_call to private precompile must fail in RpcDirect");
        assert!(
            err.to_string().contains("PrivateExecutionUnavailable"),
            "expected PrivateExecutionUnavailable, got: {err}"
        );
    }

    outbe_evm::executor::witness::clear_detached_trace_witness();
    outbe_tee::witness::set_witness_policy_for_test(None);
}

pub(super) fn resident_context(seed: u8) -> Arc<outbe_tee::in_process::InProcessTeeContext> {
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
