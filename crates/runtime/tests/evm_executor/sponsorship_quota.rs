//! Sponsorship quota is part of the block state only when the transaction is.

use evm2::{registry::HandlerError, EvmFeatures};
use outbe_evm::executor::OutbeInvalidTxError;
use outbe_system_common::addresses::{
    AGENT_REWARD_ADDRESS, TRIBUTE_FACTORY_ADDRESS, ZEROFEE_ADDRESS,
};
use outbe_zerofee::precompile::IZeroFee::SponsorshipAuthorized;

use super::*;

const TODAY: u32 = 20_260_401;

fn reverting_calldata() -> Vec<u8> {
    vec![0xde, 0xad, 0xbe, 0xef]
}

fn sponsored_tx(nonce: u64, gas_limit: u64, input: Vec<u8>) -> reth_ethereum::TransactionSigned {
    sponsored_tx_to(nonce, gas_limit, input, AGENT_REWARD_ADDRESS)
}

fn sponsored_tx_to(
    nonce: u64,
    gas_limit: u64,
    input: Vec<u8>,
    target: Address,
) -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(target),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

fn pectra_evm_env() -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(SpecId::PRAGUE, pectra_block_env(), CHAIN_ID)
}

fn pectra_block_env() -> BlockEnvExt {
    BlockEnvExt {
        number: U256::from(1),
        gas_limit: U256::from(30_000_000_u64),
        basefee: U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE),
        beneficiary: OWNER,
        timestamp: U256::from(1_775_001_600u64),
        ..Default::default()
    }
}

fn delegated_state(signer: Address, balance: U256, nonce: u64) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    let marker = Bytecode::new_legacy([0xef].into());
    for address in [
        ZEROFEE_ADDRESS,
        AGENT_REWARD_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
    ] {
        db.insert_account_info(
            &address,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker.clone()),
                ..Default::default()
            },
        );
    }
    let delegation = Bytecode::new_eip7702(ZEROFEE_ADDRESS);
    db.insert_account_info(
        &signer,
        AccountInfo {
            balance,
            nonce,
            code_hash: delegation.hash_slow(),
            code: Some(delegation),
            ..Default::default()
        },
    );
    db
}

fn counter_slot(signer: Address) -> U256 {
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone()).counter_slot(signer)
    })
}

fn live_counter<DB: evm2::evm::DynDatabase>(db: &mut CacheDB<DB>, signer: Address) -> (u32, u32) {
    let packed = evm2::evm::Database::get_storage(db, &ZEROFEE_ADDRESS, &counter_slot(signer))
        .expect("counter storage read")
        .saturating_to::<u64>();
    outbe_zerofee::unpack_counter(packed)
}

fn live_balance<DB: evm2::evm::DynDatabase>(db: &mut CacheDB<DB>, signer: Address) -> U256 {
    evm2::evm::Database::get_account(db, &signer)
        .expect("account read")
        .map(|info| info.balance)
        .unwrap_or_default()
}

fn live_nonce<DB: evm2::evm::DynDatabase>(db: &mut CacheDB<DB>, signer: Address) -> u64 {
    evm2::evm::Database::get_account(db, &signer)
        .expect("account read")
        .expect("account")
        .nonce
}

fn assert_invalid_tx(error: BlockExecutionError, nonce_too_low: bool) {
    match error {
        BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
            if nonce_too_low {
                assert!(error.is_nonce_too_low(), "{error}");
            } else {
                assert!(error.is_gas_limit_too_low(), "{error}");
            }
        }
        other => panic!("expected an invalid transaction, got {other}"),
    }
}

#[test]
fn wide_timestamp_rejects_sponsorship_before_quota_or_receipt_changes() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .unwrap();
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1);
    let mut state = delegated_state(signer, initial_balance, 0);
    let mut env = pectra_evm_env();
    env.block.timestamp = U256::MAX;
    let evm = config.evm_with_env(&mut state, env);
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
    let error = executor.execute_transaction(recovered).unwrap_err();
    assert!(matches!(error, BlockExecutionError::Validation(_)));
    assert_eq!(error.to_string(), "timestamp exceeds u64");
    assert!(executor.receipts().is_empty());
    assert_eq!(
        executor.evm().state().loaded_accounts().count(),
        0,
        "no delegation or quota access"
    );
    assert!(
        executor
            .evm()
            .state()
            .overlay_db()
            .cache
            .accounts
            .is_empty(),
        "no committed overlay account writes"
    );
    assert!(
        executor.evm().state().overlay_db().cache.storage.is_empty(),
        "no committed overlay storage writes"
    );
    let bundle = executor.into_state();
    assert!(bundle.is_empty(), "no committed block state changes");
    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

#[test]
fn sponsored_included_revert_keeps_quota() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .expect("test signature recovers");
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1u64);
    let mut state = delegated_state(signer, initial_balance, 0);

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        executor
            .execute_transaction(recovered)
            .expect("an included revert is still a committed transaction");
        let receipts = executor.receipts();
        assert_eq!(executor.sponsored_gas_used, 200_000);
        assert_eq!(receipts.len(), 1);
        assert!(!receipts[0].success);
        assert!(receipts[0].cumulative_gas_used < executor.sponsored_gas_used);
        let sig_hash = SponsorshipAuthorized::SIGNATURE_HASH;
        assert!(
            receipts[0].logs.iter().any(
                |log| log.address == ZEROFEE_ADDRESS && log.topics().first() == Some(&sig_hash)
            ),
            "an included revert still grants the sponsorship"
        );
        let db = executor.evm_mut().state_mut().overlay_db_mut();
        assert_eq!(live_counter(db, signer), (TODAY, 1));
        assert_eq!(live_balance(db, signer), initial_balance);
    }
}

#[test]
fn sponsored_nonce_too_low_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .expect("test signature recovers");
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1u64);
    let mut state = delegated_state(signer, initial_balance, 1);

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        let error = executor
            .execute_transaction(recovered)
            .expect_err("nonce already consumed");
        assert_invalid_tx(error, true);
        assert_eq!(executor.sponsored_gas_used, 0);
        assert!(executor.receipts().is_empty());
        let db = executor.evm_mut().state_mut().overlay_db_mut();
        assert_eq!(live_counter(db, signer), (0, 0));
        assert_eq!(live_balance(db, signer), initial_balance);
        assert_eq!(live_nonce(db, signer), 1);
    }
}

#[test]
fn sponsored_gas_below_intrinsic_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    // 21_000 clears the soft-failure floor and the sponsorship gas cap, and is
    // below the intrinsic cost of this calldata.
    let recovered = sponsored_tx(0, 21_000, reverting_calldata())
        .try_into_recovered()
        .expect("test signature recovers");
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1u64);
    let mut state = delegated_state(signer, initial_balance, 0);

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        let error = executor
            .execute_transaction(recovered)
            .expect_err("gas limit below intrinsic");
        assert_invalid_tx(error, false);
        assert_eq!(executor.sponsored_gas_used, 0);
        assert!(executor.receipts().is_empty());
        let db = executor.evm_mut().state_mut().overlay_db_mut();
        assert_eq!(live_counter(db, signer), (0, 0));
        assert_eq!(live_balance(db, signer), initial_balance);
        assert_eq!(live_nonce(db, signer), 0);
    }
}

#[test]
fn sponsored_declined_commit_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    // Same envelope as the included revert. Declining the commit must leave
    // the quota slot, the balance, and the nonce where they started.
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .expect("test signature recovers");
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1u64);
    let mut state = delegated_state(signer, initial_balance, 0);

    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        let outcome = executor
            .execute_transaction_with_commit_condition(recovered, |_, _| CommitChanges::No)
            .expect("declining the commit is not an execution error");
        assert!(outcome.is_none());
        assert_eq!(executor.sponsored_gas_used, 0);
        assert!(executor.receipts().is_empty());
        let db = executor.evm_mut().state_mut().overlay_db_mut();
        assert_eq!(live_counter(db, signer), (0, 0));
        assert_eq!(live_balance(db, signer), initial_balance);
        assert_eq!(live_nonce(db, signer), 0);
    }
}

#[test]
fn tribute_wrong_selector_above_default_gas_uses_paid_fallback() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let signer = OWNER;
    let gas_limit = 600_000;
    let funded_balance =
        U256::from(gas_limit) * U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE);
    for initial_balance in [U256::ZERO, funded_balance] {
        let recovered = Recovered::new_unchecked(
            sponsored_tx_to(0, gas_limit, reverting_calldata(), TRIBUTE_FACTORY_ADDRESS),
            signer,
        );
        let mut state = delegated_state(signer, initial_balance, 0);
        let paid_gas_used;
        {
            let evm = config.evm_with_env(&mut state, pectra_evm_env());
            let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
            let result = executor.execute_transaction(recovered);
            if initial_balance.is_zero() {
                let error = result.expect_err("unfunded fallback must fail ordinary validation");
                match error {
                    BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                        error,
                        ..
                    }) => {
                        assert!(matches!(
                            error.as_any().downcast_ref::<HandlerError>(),
                            Some(HandlerError::InsufficientFunds { .. })
                        ));
                    }
                    other => panic!("expected insufficient funds, got {other}"),
                }
                assert!(executor.receipts().is_empty());
                paid_gas_used = 0;
            } else {
                result.expect("funded fallback must execute normally");
                let receipts = executor.receipts();
                assert_eq!(receipts.len(), 1);
                assert!(!receipts[0].success, "unknown selector must revert");
                assert!(
                    receipts[0].logs.is_empty(),
                    "no sponsorship or policy failure logs"
                );
                paid_gas_used = receipts[0].cumulative_gas_used;
                assert!(paid_gas_used > 0);
            }
            assert_eq!(executor.sponsored_gas_used, 0);
            assert_eq!(executor.zero_fee_soft_failures, 0);
            let db = executor.evm_mut().state_mut().overlay_db_mut();
            assert_eq!(live_counter(db, signer), (0, 0));
            assert_eq!(
                live_balance(db, signer),
                initial_balance
                    - U256::from(paid_gas_used)
                        * U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE),
                "only the included paid revert debits ordinary gas fees"
            );
            assert_eq!(
                live_nonce(db, signer),
                u64::from(!initial_balance.is_zero())
            );
        }
    }
}

#[test]
fn sponsored_block_budget_defers_large_transaction_but_includes_smaller_and_resets() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let signer = OWNER;
    let mut state = delegated_state(signer, U256::ZERO, 0);
    let large_tx = |nonce, gas_limit| {
        Recovered::new_unchecked(
            sponsored_tx_to(
                nonce,
                gas_limit,
                // Selector-only calldata is sponsored but deliberately fails ABI decoding.
                outbe_tributefactory::precompile::ITributeFactory::offerTributeCall::SELECTOR
                    .to_vec(),
                TRIBUTE_FACTORY_ADDRESS,
            ),
            signer,
        )
    };
    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(4), Bytes::new()));
        for (nonce, gas_limit) in [(0, 8_000_000), (1, 5_000_000)] {
            executor
                .execute_transaction(large_tx(nonce, gas_limit))
                .unwrap();
            assert_eq!(
                executor.sponsored_gas_used,
                if nonce == 0 { 8_000_000 } else { 13_000_000 }
            );
            let receipt = executor.receipts().last().unwrap();
            assert!(
                !receipt.success,
                "selector-only input must revert during ABI decoding"
            );
            assert!(receipt.logs.iter().any(|log| {
                log.address == ZEROFEE_ADDRESS
                    && log.topics().first() == Some(&SponsorshipAuthorized::SIGNATURE_HASH)
            }));
            let db = executor.evm_mut().state_mut().overlay_db_mut();
            assert_eq!(live_counter(db, signer), (TODAY, nonce as u32 + 1));
            assert_eq!(live_balance(db, signer), U256::ZERO);
        }
        assert_eq!(executor.sponsored_gas_used, 13_000_000);
        let error = executor
            .execute_transaction(large_tx(2, 8_000_000))
            .unwrap_err();
        match error {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
                assert!(outbe_evm::is_sponsored_block_budget_deferral(
                    error.as_ref()
                ));
            }
            other => panic!("expected budget deferral, got {other}"),
        }
        assert_eq!(executor.receipts().len(), 2);
        assert_eq!(executor.sponsored_gas_used, 13_000_000);
        assert_eq!(executor.zero_fee_soft_failures, 0);
        {
            let db = executor.evm_mut().state_mut().overlay_db_mut();
            assert_eq!(live_counter(db, signer), (TODAY, 2));
            assert_eq!(live_nonce(db, signer), 2);
        }

        let smaller =
            Recovered::new_unchecked(sponsored_tx(2, 500_000, reverting_calldata()), signer);
        executor.execute_transaction(smaller).unwrap();
        assert_eq!(executor.receipts().len(), 3);
        assert_eq!(executor.sponsored_gas_used, 13_500_000);
        let bundle = executor.into_state();
        state.commit_source(&BundleSource(&bundle));
    }
    assert_eq!(live_counter(&mut state, signer), (TODAY, 3));
    assert_eq!(signer_balance(&mut state, signer), U256::ZERO);
    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        assert_eq!(executor.sponsored_gas_used, 0);
        executor
            .execute_transaction(large_tx(3, 8_000_000))
            .unwrap();
        assert_eq!(executor.sponsored_gas_used, 8_000_000);
        assert!(!executor.receipts()[0].success);
        assert_eq!(executor.zero_fee_soft_failures, 0);
        let bundle = executor.into_state();
        state.commit_source(&BundleSource(&bundle));
    }
    assert_eq!(live_counter(&mut state, signer), (TODAY, 4));
    assert_eq!(signer_balance(&mut state, signer), U256::ZERO);
}

#[test]
fn sponsored_budget_deferral_precedes_quota_exhaustion() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .unwrap();
    let signer = Address::from(*recovered.signer());
    let mut state = delegated_state(signer, U256::ZERO, 0);
    let packed = outbe_zerofee::pack_counter(TODAY, outbe_zerofee::FREE_TX_DAILY_LIMIT);
    state.insert_account_storage(&ZEROFEE_ADDRESS, &counter_slot(signer), &U256::from(packed));
    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        executor.sponsored_gas_used = outbe_zerofee::SPONSORED_GAS_PER_BLOCK;
        let error = executor.execute_transaction(recovered.clone()).unwrap_err();
        match error {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
                assert!(outbe_evm::is_sponsored_block_budget_deferral(
                    error.as_ref()
                ));
            }
            other => panic!("expected budget deferral, got {other}"),
        }
        assert!(executor.receipts().is_empty());
        assert_eq!(executor.zero_fee_soft_failures, 0);
        assert_eq!(
            live_nonce(executor.evm_mut().state_mut().overlay_db_mut(), signer),
            0
        );
        // Once it fits, the same exhausted signer receives the existing soft failure.
        executor.sponsored_gas_used = 0;
        executor.execute_transaction(recovered).unwrap();
        assert_eq!(executor.receipts().len(), 1);
        assert!(!executor.receipts()[0].success);
        assert_eq!(executor.sponsored_gas_used, 0);
        assert_eq!(executor.zero_fee_soft_failures, 1);
        assert_eq!(
            live_counter(executor.evm_mut().state_mut().overlay_db_mut(), signer),
            (TODAY, outbe_zerofee::FREE_TX_DAILY_LIMIT)
        );
    }
}

#[test]
fn sponsored_budget_matcher_requires_exact_reason() {
    #[derive(Debug)]
    struct ForeignNonceTooLow;

    impl std::fmt::Display for ForeignNonceTooLow {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("nonce too low")
        }
    }

    impl std::error::Error for ForeignNonceTooLow {}

    impl reth_evm::InvalidTxError for ForeignNonceTooLow {
        fn is_nonce_too_low(&self) -> bool {
            true
        }

        fn as_any(&self) -> &(dyn std::any::Any + 'static) {
            self
        }
    }

    assert!(outbe_evm::is_sponsored_block_budget_deferral(
        &OutbeInvalidTxError::SponsoredBlockBudgetExceeded
    ));
    for other in [
        OutbeInvalidTxError::EnclaveWitnessBudgetExceeded,
        OutbeInvalidTxError::ZeroFeeSoftFailureCapExceeded { cap: 1 },
        OutbeInvalidTxError::UnsignedEnclaveError,
    ] {
        assert!(!outbe_evm::is_sponsored_block_budget_deferral(&other));
    }
    assert!(!outbe_evm::is_sponsored_block_budget_deferral(
        &ForeignNonceTooLow
    ));
}

#[test]
fn sponsored_execution_restores_existing_fee_flags_on_every_result_exit() {
    let config = OutbeEvmConfig::new(
        test_chain_spec(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    for flags in 0..8u8 {
        for outcome in ["included", "declined", "error"] {
            let recovered = sponsored_tx(0, 200_000, reverting_calldata())
                .try_into_recovered()
                .unwrap();
            let signer = Address::from(*recovered.signer());
            let initial_nonce = u64::from(outcome == "error");
            let mut state = delegated_state(signer, U256::from(1), initial_nonce);
            let expected = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
            let mut env = pectra_evm_env();
            env.version
                .features
                .set(EvmFeatures::BALANCE_CHECK, !expected.0);
            env.version
                .features
                .set(EvmFeatures::BASE_FEE_CHECK, !expected.1);
            env.version
                .features
                .set(EvmFeatures::FEE_CHARGE, !expected.2);
            {
                let evm = config.evm_with_env(&mut state, env);
                let mut executor =
                    config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
                let result =
                    executor.execute_transaction_with_commit_condition(recovered, |_, _| {
                        if outcome == "declined" {
                            CommitChanges::No
                        } else {
                            CommitChanges::Yes
                        }
                    });
                match outcome {
                    "included" => {
                        assert!(result.unwrap().is_some());
                        assert_eq!(executor.receipts().len(), 1);
                        assert!(!executor.receipts()[0].success);
                    }
                    "declined" => {
                        assert!(result.unwrap().is_none());
                        assert!(executor.receipts().is_empty());
                    }
                    "error" => {
                        assert_invalid_tx(result.unwrap_err(), true);
                        assert!(executor.receipts().is_empty());
                    }
                    _ => unreachable!(),
                }
                let features = executor.evm().version().features;
                assert_eq!(
                    (
                        !features.contains(EvmFeatures::BALANCE_CHECK),
                        !features.contains(EvmFeatures::BASE_FEE_CHECK),
                        !features.contains(EvmFeatures::FEE_CHARGE),
                    ),
                    expected,
                    "flags={flags}, outcome={outcome}"
                );
                assert_eq!(
                    executor.current_execution_summary().validator_fee_sum,
                    U256::ZERO
                );
                let db = executor.evm_mut().state_mut().overlay_db_mut();
                assert_eq!(
                    live_counter(db, signer),
                    if outcome == "included" {
                        (TODAY, 1)
                    } else {
                        (0, 0)
                    }
                );
                assert_eq!(live_balance(db, signer), U256::from(1));
            }
        }
    }
}
