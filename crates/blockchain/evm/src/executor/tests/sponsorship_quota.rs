//! Sponsorship quota is part of the block state only when the transaction is.

use alloy_evm::block::CommitChanges;
use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, TRIBUTE_FACTORY_ADDRESS, ZEROFEE_ADDRESS};
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

fn pectra_evm_env() -> EvmEnv {
    EvmEnv {
        cfg_env: revm::context::CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(revm::primitives::hardfork::SpecId::PRAGUE),
        block_env: pectra_block_env(),
    }
}

fn pectra_block_env() -> revm::context::BlockEnv {
    revm::context::BlockEnv {
        number: U256::from(1),
        gas_limit: 30_000_000,
        basefee: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE,
        beneficiary: OWNER,
        timestamp: U256::from(1_775_001_600u64),
        ..Default::default()
    }
}

fn delegated_state(
    signer: Address,
    balance: U256,
    nonce: u64,
) -> State<CacheDB<EmptyDBTyped<ProviderError>>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    let marker = Bytecode::new_legacy([0xef].into());
    for address in [
        ZEROFEE_ADDRESS,
        AGENT_REWARD_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker.clone()),
                ..Default::default()
            },
        );
    }
    let delegation = Bytecode::new_eip7702(ZEROFEE_ADDRESS);
    db.insert_account_info(
        signer,
        AccountInfo {
            balance,
            nonce,
            code_hash: delegation.hash_slow(),
            code: Some(delegation),
            ..Default::default()
        },
    );
    State::builder()
        .with_database(db)
        .with_bundle_update()
        .build()
}

fn counter_slot(signer: Address) -> U256 {
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone()).counter_slot(signer)
    })
}

fn live_counter(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    signer: Address,
) -> (u32, u32) {
    let packed = state
        .storage(ZEROFEE_ADDRESS, counter_slot(signer))
        .expect("counter storage read")
        .saturating_to::<u64>();
    outbe_zerofee::unpack_counter(packed)
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
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .unwrap();
    let signer = Address::from(*recovered.signer());
    let initial_balance = U256::from(1);
    let mut state = delegated_state(signer, initial_balance, 0);
    let mut env = pectra_evm_env();
    env.block_env.timestamp = U256::MAX;
    let evm = config.evm_with_env(&mut state, env);
    let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
    let error = executor.execute_transaction(recovered).unwrap_err();
    assert!(matches!(error, BlockExecutionError::Validation(_)));
    assert_eq!(error.to_string(), "timestamp exceeds u64");
    assert!(executor.receipts().is_empty());
    drop(executor);
    assert!(
        state.cache.accounts.is_empty(),
        "no delegation or quota access"
    );
    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

#[test]
fn sponsored_included_revert_keeps_quota() {
    let config = OutbeEvmConfig::new(test_chain_spec());
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
    }

    assert_eq!(live_counter(&mut state, signer), (TODAY, 1));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
}

#[test]
fn sponsored_nonce_too_low_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(test_chain_spec());
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
    }

    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
    let account = state.basic(signer).expect("account read").expect("account");
    assert_eq!(account.nonce, 1);
}

#[test]
fn sponsored_gas_below_intrinsic_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(test_chain_spec());
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
    }

    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
    let account = state.basic(signer).expect("account read").expect("account");
    assert_eq!(account.nonce, 0);
}

#[test]
fn sponsored_declined_commit_does_not_burn_quota() {
    let config = OutbeEvmConfig::new(test_chain_spec());
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
            .execute_transaction_with_commit_condition(recovered, |_| CommitChanges::No)
            .expect("declining the commit is not an execution error");
        assert!(outcome.is_none());
        assert_eq!(executor.sponsored_gas_used, 0);
        assert!(executor.receipts().is_empty());
    }

    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
    let account = state.basic(signer).expect("account read").expect("account");
    assert_eq!(account.nonce, 0);
}

#[test]
fn sponsored_block_budget_defers_large_transaction_but_includes_smaller_and_resets() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let signer = OWNER;
    let mut state = delegated_state(signer, U256::ZERO, 0);
    let large_tx = |nonce, gas_limit| {
        Recovered::new_unchecked(
            sponsored_tx_to(
                nonce,
                gas_limit,
                reverting_calldata(),
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
        }
        assert_eq!(executor.sponsored_gas_used, 13_000_000);
        let error = executor
            .execute_transaction(large_tx(2, 8_000_000))
            .unwrap_err();
        match error {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
                assert!(crate::is_sponsored_block_budget_deferral(error.as_ref()));
            }
            other => panic!("expected budget deferral, got {other}"),
        }
        assert_eq!(executor.receipts().len(), 2);
        assert_eq!(executor.sponsored_gas_used, 13_000_000);
        assert_eq!(executor.zero_fee_soft_failures, 0);
        let db = executor.inner.evm.db_mut();
        assert_eq!(live_counter(db, signer), (TODAY, 2));
        assert_eq!(db.basic(signer).unwrap().unwrap().nonce, 2);

        let smaller =
            Recovered::new_unchecked(sponsored_tx(2, 500_000, reverting_calldata()), signer);
        executor.execute_transaction(smaller).unwrap();
        assert_eq!(executor.receipts().len(), 3);
        assert_eq!(executor.sponsored_gas_used, 13_500_000);
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
    }
    assert_eq!(live_counter(&mut state, signer), (TODAY, 4));
}

#[test]
fn sponsored_budget_deferral_precedes_quota_exhaustion() {
    let config = OutbeEvmConfig::new(test_chain_spec());
    let recovered = sponsored_tx(0, 200_000, reverting_calldata())
        .try_into_recovered()
        .unwrap();
    let signer = Address::from(*recovered.signer());
    let mut state = delegated_state(signer, U256::ZERO, 0);
    let packed = outbe_zerofee::pack_counter(TODAY, outbe_zerofee::FREE_TX_DAILY_LIMIT);
    state
        .database
        .insert_account_storage(ZEROFEE_ADDRESS, counter_slot(signer), U256::from(packed))
        .unwrap();
    {
        let evm = config.evm_with_env(&mut state, pectra_evm_env());
        let mut executor = config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
        executor.sponsored_gas_used = outbe_zerofee::SPONSORED_GAS_PER_BLOCK;
        let error = executor.execute_transaction(recovered.clone()).unwrap_err();
        match error {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx { error, .. }) => {
                assert!(crate::is_sponsored_block_budget_deferral(error.as_ref()));
            }
            other => panic!("expected budget deferral, got {other}"),
        }
        assert!(executor.receipts().is_empty());
        assert_eq!(executor.zero_fee_soft_failures, 0);
        assert_eq!(
            executor
                .inner
                .evm
                .db_mut()
                .basic(signer)
                .unwrap()
                .unwrap()
                .nonce,
            0
        );
        // Once it fits, the same exhausted signer receives the existing soft failure.
        executor.sponsored_gas_used = 0;
        executor.execute_transaction(recovered).unwrap();
        assert_eq!(executor.receipts().len(), 1);
        assert!(!executor.receipts()[0].success);
        assert_eq!(executor.sponsored_gas_used, 0);
        assert_eq!(executor.zero_fee_soft_failures, 1);
    }
    assert_eq!(
        live_counter(&mut state, signer),
        (TODAY, outbe_zerofee::FREE_TX_DAILY_LIMIT)
    );
}

#[test]
fn sponsored_budget_matcher_requires_exact_reason() {
    assert!(crate::is_sponsored_block_budget_deferral(
        &InvalidTransaction::Str(crate::SPONSORED_BLOCK_BUDGET_EXCEEDED.into(),)
    ));
    for reason in [
        "sponsored block budget exceeded suffix",
        "zero-fee soft-failure cap exceeded",
    ] {
        assert!(!crate::is_sponsored_block_budget_deferral(
            &InvalidTransaction::Str(reason.into())
        ));
    }
    assert!(!crate::is_sponsored_block_budget_deferral(
        &InvalidTransaction::NonceTooLow { tx: 0, state: 1 }
    ));
}

#[test]
fn sponsored_execution_restores_existing_fee_flags_on_every_result_exit() {
    let config = OutbeEvmConfig::new(test_chain_spec());
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
            env.cfg_env.disable_balance_check = expected.0;
            env.cfg_env.disable_base_fee = expected.1;
            env.cfg_env.disable_fee_charge = expected.2;
            {
                let evm = config.evm_with_env(&mut state, env);
                let mut executor =
                    config.create_executor(evm, execution_ctx(Some(1), Bytes::new()));
                let result = executor.execute_transaction_with_commit_condition(recovered, |_| {
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
                let cfg = &executor.inner.evm.ctx_mut().cfg;
                assert_eq!(
                    (
                        cfg.disable_balance_check,
                        cfg.disable_base_fee,
                        cfg.disable_fee_charge
                    ),
                    expected,
                    "flags={flags}, outcome={outcome}"
                );
                assert_eq!(
                    executor.current_execution_summary().validator_fee_sum,
                    U256::ZERO
                );
            }
            assert_eq!(
                live_counter(&mut state, signer),
                if outcome == "included" {
                    (TODAY, 1)
                } else {
                    (0, 0)
                }
            );
            assert_eq!(signer_balance(&mut state, signer), U256::from(1));
        }
    }
}
