use super::super::*;

fn test_oracle_get_params_tx() -> reth_ethereum::TransactionSigned {
    let selector = keccak256("getParams()");
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 100_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(ORACLE_ADDRESS),
        value: U256::ZERO,
        input: Bytes::copy_from_slice(&selector[..4]),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn oracle_tx_keeps_fee_envelope_for_basefee_validation() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let oracle_tx = test_oracle_get_params_tx()
        .try_into_recovered()
        .expect("oracle tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    db.insert_account_info(
        oracle_tx.signer(),
        AccountInfo {
            balance: U256::from(1_000_000u64),
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: MIN_PROTOCOL_BASE_FEE,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    executor
        .execute_transaction(oracle_tx)
        .expect("oracle tx with fee cap at basefee must pass validation");

    assert_eq!(executor.receipts().len(), 1);
}

#[test]
fn gas_10_low_gas_zero_fee_policy_failure_must_not_mint_intrinsic_gas() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let low_gas_zero_fee_tx = test_oracle_submit_vote_tx_with_gas_limit(1)
        .try_into_recovered()
        .expect("oracle tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));

    let err = executor
        .execute_transaction(low_gas_zero_fee_tx)
        .expect_err("gas_limit < intrinsic gas must reject before synthetic receipt creation");
    assert!(
        err.to_string().contains("intrinsic") || err.to_string().contains("gas limit"),
        "GAS-10: low-gas zero-fee rejection must be an admission error, got {err}"
    );
    assert!(
        executor.receipts().is_empty(),
        "GAS-10: invalid low-gas zero-fee tx must not mint a 21k synthetic receipt"
    );
}

/// The per-block zero-fee soft-failure cap admits up to
/// `MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK` soft-failures. Then it rejects further
/// ones with a tx-level `InvalidTx`. The payload builder SKIPS this variant
/// (mark_invalid + continue), and a validator REJECTS the block on it. It is NOT
/// a fatal `Internal` error that would abort the build (the 2026-05-15 halt).
#[test]
fn zero_fee_soft_failure_cap_admits_then_rejects_with_invalid_tx() {
    use alloy_evm::block::{BlockExecutionError, BlockValidationError};
    let signer = test_evm_signer();
    let proposer = signer.address();
    let mut state = state_with_active_proposer(proposer);
    let config = OutbeEvmConfig::new(test_chain_spec()).with_evm_signer(signer.clone());
    let evm = config.evm_with_env(&mut state, test_evm_env(1, REWARDS_ADDRESS));
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));

    let mut admitted = 0u32;
    let rejected_err = loop {
        match executor.record_zero_fee_soft_failure(B256::ZERO) {
            Ok(()) => {
                admitted += 1;
                assert!(admitted <= 4096, "cap never enforced");
            }
            Err(err) => break err,
        }
    };
    assert_eq!(
        admitted, 64,
        "zero-fee soft-failure cap must admit exactly MAX_ZERO_FEE_SOFT_FAILURES_PER_BLOCK (64)"
    );
    assert!(
        matches!(
            rejected_err,
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { .. })
        ),
        "over-cap zero-fee soft-failure must be a tx-level InvalidTx (skip-on-build / \
         reject-on-validate), got: {rejected_err:?}"
    );
}

#[test]
fn zero_fee_oracle_vote_from_delegated_feeder_keeps_zero_balance() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let validator = address!("0x1111111111111111111111111111111111111111");
    let pk = dummy_pubkey(0xA1);
    let zero_fee_tx = test_oracle_submit_vote_tx()
        .try_into_recovered()
        .expect("oracle submitVote tx signer should recover");
    let feeder = Address::from(*zero_fee_tx.signer());

    let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_registered_active_validator(storage.clone(), validator, &pk);

        // Feeder resolution moved to the role-scoped ValidatorSet
        // delegation registry. `resolve_validator_for_feeder` no longer
        // consults the legacy oracle-side mapping.
        let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        validator_set.set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            feeder,
        )?;
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("test genesis state must be seeded");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    for address in seed_storage
        .storage
        .keys()
        .map(|(address, _)| *address)
        .collect::<std::collections::HashSet<_>>()
    {
        db.insert_account_info(address, AccountInfo::default());
    }
    for ((address, slot), value) in seed_storage.storage {
        db.insert_account_storage(address, slot, value)
            .expect("seed storage insert should succeed");
    }
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );

    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    let feeder_balance_before = state
        .basic(feeder)
        .expect("feeder account read should succeed")
        .map(|account| account.balance)
        .unwrap_or_default();
    assert_eq!(feeder_balance_before, U256::ZERO);

    let setup_read_ctx = BlockContext::new(1, 1, CHAIN_ID, OWNER, vec![validator]);
    let mut setup_provider =
        outbe_primitives::storage::direct::DirectStorageProvider::new(&mut state, setup_read_ctx);
    StorageHandle::enter(&mut setup_provider, |storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let record = vs
            .get_validator(validator)?
            .expect("validator should be registered");
        assert_eq!(record.status, outbe_validatorset::logic::status::ACTIVE);
        assert!(record.has_bls_share);

        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        assert_eq!(oracle.resolve_validator_for_feeder(feeder)?, validator);
        Ok::<_, outbe_primitives::error::PrecompileError>(())
    })
    .expect("seeded zero-fee authorization state should be readable");

    {
        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(zero_fee_tx)
            .expect("delegated zero-fee oracle vote should execute");

        let cfg = &executor.inner.evm.ctx_mut().cfg;
        assert_eq!(
            (
                cfg.disable_balance_check,
                cfg.disable_base_fee,
                cfg.disable_fee_charge
            ),
            (false, false, false),
            "oracle waiver must restore fee validation after execution"
        );

        assert_eq!(executor.receipts().len(), 1);
        assert!(executor.receipts()[0].success);
        assert!(executor.receipts()[0].cumulative_gas_used > 0);
        assert!(executor.receipts()[0]
            .logs
            .iter()
            .any(|log| log.address == ORACLE_ADDRESS));
    }
    state.merge_transitions(BundleRetention::Reverts);

    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    let vote_slot = StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .vote_exists
            .get(&validator)
            .slot()
    });
    assert_eq!(
        state
            .bundle_state
            .storage(&ORACLE_ADDRESS, vote_slot)
            .unwrap_or_default(),
        U256::from(1u64)
    );

    let feeder_balance_after = state
        .basic(feeder)
        .expect("feeder account read should succeed")
        .map(|account| account.balance)
        .unwrap_or_default();
    assert_eq!(feeder_balance_after, U256::ZERO);
}

/// / T6.2 parity: two executors with identical state and tx
/// produce byte-equal soft-fail receipts. This is the on-chain parity
/// invariant that keeps `receipts_root` deterministic across proposer
/// and validators when a zero-fee tx is soft-failed.
#[test]
fn parity_soft_failed_zero_fee_receipt_is_byte_equal_across_runs() {
    fn run() -> Vec<reth_ethereum::Receipt> {
        let config = OutbeEvmConfig::new(test_chain_spec());
        // No validator-set seeding, no feeder delegation: the oracle vote will
        // hit `authorize_fee_waiver` -> `UnauthorizedSigner` (code 107).
        let zero_fee_tx = test_oracle_submit_vote_tx()
            .try_into_recovered()
            .expect("oracle submitVote tx signer should recover");

        let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        let marker_code = Bytecode::new_legacy([0xef].into());
        db.insert_account_info(
            ORACLE_ADDRESS,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code),
                ..Default::default()
            },
        );

        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        let mut executor = config.create_executor(evm, ctx);

        executor
            .execute_transaction(zero_fee_tx)
            .expect("soft-fail path must not abort the block build");

        executor.receipts().to_vec()
    }

    let receipts_a = run();
    let receipts_b = run();

    assert_eq!(receipts_a.len(), 1);
    assert_eq!(receipts_b.len(), 1);
    assert!(!receipts_a[0].success);
    assert_eq!(receipts_a[0].logs.len(), 1);
    // Soft-fail log must come from the zero-fee policy address with the
    // OutbeFailure topic0. Anything else is a parity drift.
    assert_eq!(
        receipts_a[0].logs[0].address,
        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS
    );
    assert_eq!(
        receipts_a[0].logs[0].data.topics()[0],
        crate::failure_receipt::OUTBE_FAILURE_TOPIC0
    );
    // Code 107 (UnauthorizedSigner) - padded to 32 bytes BE.
    let mut expected_topic1 = [0u8; 32];
    expected_topic1[30] = 0;
    expected_topic1[31] = 107;
    assert_eq!(
        receipts_a[0].logs[0].data.topics()[1].as_slice(),
        expected_topic1
    );

    // Byte parity: RLP-encode both runs' receipts and compare bytes.
    // EIP-2718 is the canonical encoding used by `receipts_root`, so byte
    // equality here means `receipts_root` will be equal on every node.
    use alloy_consensus::TxReceipt;
    use alloy_eips::eip2718::Encodable2718;
    let buf_a = receipts_a[0].with_bloom_ref().encoded_2718();
    let buf_b = receipts_b[0].with_bloom_ref().encoded_2718();
    assert_eq!(
        buf_a, buf_b,
        "soft-fail receipts must be byte-equal across runs"
    );
}

/// / T6.6 property: `validator_fee_sum` MUST NOT be perturbed
/// by soft-failed zero-fee transactions. Failed zero-fee txs never run
/// the EVM and never contribute miner fees. Only successful user txs in
/// the priority-fee path increment `current_block_validator_fees`.
///
/// This is a focused invariance test (proptest-style over multiple
/// runs without the full `proptest` macro to keep the test fast and
/// dependency-free).
#[test]
fn property_soft_fail_does_not_perturb_validator_fee_sum() {
    let chain_spec = test_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(chain_spec.clone());
    for run in 0..5 {
        let zero_fee_tx = test_oracle_submit_vote_tx()
            .try_into_recovered()
            .expect("oracle submitVote tx signer should recover");

        let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
        let marker_code = Bytecode::new_legacy([0xef].into());
        db.insert_account_info(
            ORACLE_ADDRESS,
            AccountInfo {
                code_hash: marker_code.hash_slow(),
                code: Some(marker_code),
                ..Default::default()
            },
        );
        let mut state = State::builder()
            .with_database(db)
            .with_bundle_update()
            .build();

        let evm_env = EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(CHAIN_ID)
                .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
            block_env: BlockEnv {
                number: U256::from(1u64 + run as u64),
                gas_limit: 30_000_000,
                basefee: 1_000_000_000,
                beneficiary: OWNER,
                timestamp: U256::from(1u64 + run as u64),
                ..Default::default()
            },
        };
        let evm = config.evm_with_env(&mut state, evm_env);
        let ctx = execution_ctx(Some(1), Bytes::new());
        // Construct OutbeBlockExecutor directly (instead of through
        // `config.create_executor`) to keep the concrete type so we can
        // call `current_execution_summary`. The method is private to
        // `OutbeBlockExecutor`. Otherwise the `BlockExecutorFor` opaque
        // return type hides it.
        let mut executor = OutbeBlockExecutor::new(
            EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
            fixtures::executor_inputs_from_ctx(&ctx, None, false),
        );

        // Baseline: no txs.
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "run {run}: baseline fee sum must be zero"
        );

        // Soft-fail one tx.
        executor
            .execute_transaction(zero_fee_tx)
            .expect("soft-fail must succeed");

        // Invariant: failed zero-fee tx contributes 0 to validator fee sum.
        assert_eq!(
            executor.current_execution_summary().validator_fee_sum,
            U256::ZERO,
            "run {run}: soft-failed zero-fee tx must not credit the validator"
        );
    }
}

/// / T6.4 mempool natural-eviction bridge: a soft-failed zero-fee
/// tx returns `Ok(non-zero gas)` from `execute_transaction`, which signals
/// the `BasicBlockBuilder` to append the tx to `block.body`. Reth's pool
/// then evicts the tx hash on canonical commit via the standard
/// `on_new_head_block` -> `pool.remove_transactions(block_hashes)` path.
///
/// This is the contract that lets (T4 Won't Do) skip any custom
/// `mark_invalid` plumbing. It confirms that the executor's `Ok` return
/// is enough for the natural-eviction flow downstream.
#[test]
fn soft_fail_returns_ok_so_tx_lands_in_block_body() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let zero_fee_tx = test_oracle_submit_vote_tx()
        .try_into_recovered()
        .expect("oracle submitVote tx signer should recover");

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ORACLE_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
    let mut state = State::builder()
        .with_database(db)
        .with_bundle_update()
        .build();

    let evm_env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: 1_000_000_000,
            beneficiary: OWNER,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let ctx = execution_ctx(Some(1), Bytes::new());
    let mut executor = config.create_executor(evm, ctx);

    // Soft-fail path returns `Ok`. This is the contract that lets the wrapping
    // `BasicBlockBuilder` append the tx to `block.body.transactions`.
    let gas_output = executor
        .execute_transaction(zero_fee_tx)
        .expect("soft-fail path must not abort the block build");

    // Non-zero gas: signals the tx was "executed and committed" from the
    // BlockBuilder's perspective, even though no EVM code ran.
    assert!(
        gas_output.tx_gas_used() > 0,
        "non-zero gas signals the tx is committed to the block body, \
         which is the prerequisite for Reth's standard pool eviction"
    );
    // Exactly one receipt was pushed.
    assert_eq!(executor.receipts().len(), 1);
    assert!(
        !executor.receipts()[0].success,
        "soft-fail receipt must have status=0"
    );
    // Receipt contains the synthetic failure log. eth_getTransactionReceipt
    // will show this to external observers.
    assert_eq!(executor.receipts()[0].logs.len(), 1);
    assert_eq!(
        executor.receipts()[0].logs[0].address,
        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS
    );
}
