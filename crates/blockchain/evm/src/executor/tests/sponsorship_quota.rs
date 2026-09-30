//! Sponsorship quota is part of the block state only when the transaction is.

use alloy_evm::block::CommitChanges;
use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS};
use outbe_zerofee::precompile::IZeroFee::SponsorshipAuthorized;

use super::*;

const TODAY: u32 = 20_260_401;

fn reverting_calldata() -> Vec<u8> {
    vec![0xde, 0xad, 0xbe, 0xef]
}

fn sponsored_tx(nonce: u64, gas_limit: u64, input: Vec<u8>) -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce,
        gas_limit,
        max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(AGENT_REWARD_ADDRESS),
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
    for address in [ZEROFEE_ADDRESS, AGENT_REWARD_ADDRESS] {
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
        outbe_zerofee::ZeroFeeContract::new(storage.clone())
            .counter
            .slot(&signer)
            .slot()
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
        assert_eq!(receipts.len(), 1);
        assert!(!receipts[0].success);
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
        assert!(executor.receipts().is_empty());
    }

    assert_eq!(live_counter(&mut state, signer), (0, 0));
    assert_eq!(signer_balance(&mut state, signer), initial_balance);
    let account = state.basic(signer).expect("account read").expect("account");
    assert_eq!(account.nonce, 0);
}
