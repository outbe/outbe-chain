//! The EIP-1559 base fee and EIP-4844 blob fee are native burns. The sender pays base (+ blob)
//! plus tip. The rewards escrow receives only the tip, and end-of-block settlement routes the
//! burned fee total to `PromisLimitContract` without touching `PROMIS_LIMIT_ADDRESS` native balance.

use super::receipts_and_gas_support::*;
use super::*;
use alloy_consensus::{TxEip4844, TxLegacy};
use alloy_eips::eip4844::{DATA_GAS_PER_BLOB, VERSIONED_HASH_VERSION_KZG};
use alloy_evm::block::{BlockExecutionError, CommitChanges};
use outbe_evm::errors::BurnedFeeSettlementError;
use outbe_primitives::{
    addresses::PROMIS_LIMIT_ADDRESS, error::PrecompileError, storage::direct::DirectStorageProvider,
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;
use revm::context_interface::block::BlobExcessGasAndPrice;
use std::sync::Mutex;

fn cancun_chain_spec() -> Arc<ChainSpec<OutbeHeader>> {
    use outbe_primitives::tee_test_utils::{
        gramine_direct_policy_v1, tee_attestation_v1_extra_field,
    };

    let mut spec = ChainSpecBuilder::from(&*MAINNET).cancun_activated().build();
    spec.chain = CHAIN_ID.into();
    spec.genesis.config.chain_id = CHAIN_ID;
    spec.genesis.timestamp = 1;
    let policy = gramine_direct_policy_v1(spec.chain().id(), spec.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    spec.genesis.config.extra_fields.insert(
        "teeAttestationV1".to_owned(),
        tee_attestation_v1_extra_field(&policy).expect("test TEE activation manifest is canonical"),
    );
    Arc::new(spec.map_header(OutbeHeader::new))
}

fn seed_promis_limit_marker(db: &mut CacheDB<EmptyDBTyped<ProviderError>>) {
    let marker_code = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        PROMIS_LIMIT_ADDRESS,
        AccountInfo {
            code_hash: marker_code.hash_slow(),
            code: Some(marker_code),
            ..Default::default()
        },
    );
}

#[test]
fn base_fee_is_burned_tip_reaches_rewards_and_promis_limit_receives_burned_fee() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let tx = test_priority_fee_tx();
    let recovered = tx
        .clone()
        .try_into_recovered()
        .expect("priority-fee tx signer should recover");
    let sender = recovered.signer();
    let sender_before = U256::from(1_000_000u64);

    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_promis_limit_marker(&mut db);
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: sender_before,
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
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );
    executor
        .execute_transaction(recovered)
        .expect("priority-fee tx should execute");
    let gas_used = U256::from(executor.receipts()[0].cumulative_gas_used);
    let (_evm, _result) = executor.finish().expect("executor finish should succeed");

    let base = U256::from(MIN_PROTOCOL_BASE_FEE);
    // max_fee = 2 x base and tip = base, so the effective price is 2 x base.
    let tip = U256::from(tx.max_priority_fee_per_gas().unwrap_or_default());
    let burned = base * gas_used;
    let balance = |state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>, account: Address| {
        state
            .basic(account)
            .expect("account read")
            .map(|info| info.balance)
            .unwrap_or_default()
    };
    let sender_after = balance(&mut state, sender);
    let rewards_after = balance(&mut state, REWARDS_ADDRESS);
    let recipient_after = balance(&mut state, Address::ZERO);

    assert_eq!(sender_before - sender_after, (base + tip) * gas_used);
    assert_eq!(
        rewards_after,
        tip * gas_used,
        "only the tip reaches the rewards escrow"
    );
    assert_eq!(
        recipient_after,
        U256::ZERO,
        "a zero-value call credits nothing"
    );
    assert_eq!(
        (sender_before - sender_after) - rewards_after,
        burned,
        "the base fee component is credited to no account"
    );

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer: REWARDS_ADDRESS,
        validators: vec![REWARDS_ADDRESS],
    };
    let mut provider = DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        assert_eq!(
            promis.promis_limit().unwrap(),
            burned / UNITS_PER_PROTOCOL_MINOR,
            "burned base fee whole protocol minors must credit Promis Limit"
        );
        assert_eq!(
            promis.rest_minor().unwrap(),
            burned % UNITS_PER_PROTOCOL_MINOR,
            "burned base fee sub-minor remainder must persist in rest_minor"
        );
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}

#[test]
fn base_and_blob_fee_go_to_promis_limit() {
    scenario_a_paid_reverted_and_declined_txs_settle_burned_base_fees();
    scenario_b_eip4844_blob_tx_settles_base_and_blob_fees();
    scenario_c_protocol_fee_waiver_burns_zero_under_nonzero_basefee();
    scenario_d_promis_limit_overflow_fails_the_block_with_typed_error();
    scenario_e_proposer_state_hook_pre_freeze_idempotence_and_validator_parity();
}

fn sign_tx_with_key<T: alloy_consensus::SignableTransaction<Signature>>(
    tx: T,
    key_byte: u8,
) -> (
    Address,
    reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>,
)
where
    reth_ethereum::TransactionSigned: From<alloy_consensus::Signed<T>>,
{
    let key = k256::ecdsa::SigningKey::from_slice(&[key_byte; 32]).unwrap();
    let signer = Address::from_public_key(key.verifying_key());
    let (sig, rec_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        key.sign_prehash(tx.signature_hash().as_slice()).unwrap();
    let signature =
        Signature::from_bytes_and_parity(sig.to_bytes().as_slice(), rec_id.to_byte() != 0)
            .normalized_s();
    let recovered = reth_ethereum::TransactionSigned::from(tx.into_signed(signature))
        .try_into_recovered()
        .expect("signer must recover");
    assert_eq!(Address::from(*recovered.signer()), signer);
    (signer, recovered)
}

fn scenario_a_paid_reverted_and_declined_txs_settle_burned_base_fees() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );

    // base_fee = 100_000_001 minor units; a 21_000 gas tx burns 2_100_000_021_000 minor units
    // (2 whole protocol minors + 100_000_021_000 remainder), crossing >= 1 UNITS_PER_PROTOCOL_MINOR
    // while carrying a non-zero initial old_rest across the block settlement.
    let base_fee: u64 = 100_000_001;
    let old_rest = U256::from(500_000_000_000u64);
    let tip_1: u128 = 10_000_000_000;
    let tip_2: u128 = 20_000_000_000;
    let tip_3: u128 = 5_000_000_000;

    let reverting_target = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");

    // 1. Successful EIP-1559 tx
    let (sender_1, tx_1) = sign_tx_with_key(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: u128::from(base_fee) + tip_1,
            max_priority_fee_per_gas: tip_1,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        },
        0x21,
    );

    // 2. Successful Legacy tx (gas_price = base_fee + tip_2)
    let (sender_2, tx_2) = sign_tx_with_key(
        TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce: 0,
            gas_price: u128::from(base_fee) + tip_2,
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
        },
        0x22,
    );

    // 3. Reverting EIP-1559 tx (calls contract with PUSH1 0x00 PUSH1 0x00 REVERT)
    let (sender_3, tx_3) = sign_tx_with_key(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 50_000,
            max_fee_per_gas: u128::from(base_fee) + tip_3,
            max_priority_fee_per_gas: tip_3,
            to: TxKind::Call(reverting_target),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        },
        0x23,
    );

    // 4. Declined-commit EIP-1559 tx (CommitChanges::No -> result == None)
    let (sender_4, tx_4) = sign_tx_with_key(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: u128::from(base_fee) + tip_1,
            max_priority_fee_per_gas: tip_1,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        },
        0x24,
    );

    let initial_balance = U256::from(100_000_000_000_000_000u64);
    let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut seed_storage, |storage| {
        use outbe_primitives::addresses::SYSTEM_ADDRESS;
        use outbe_promislimit::BurnSource;
        PromisLimitContract::new(storage).burn_to_promis_limit(
            SYSTEM_ADDRESS,
            old_rest,
            BurnSource::BaseFee,
        )
    })
    .unwrap();
    let mut db = cache_db_from_storage(seed_storage);
    seed_promis_limit_marker(&mut db);
    let revert_bytecode = Bytecode::new_legacy([0x60, 0x00, 0x60, 0x00, 0xfd].into());
    db.insert_account_info(
        reverting_target,
        AccountInfo {
            code_hash: revert_bytecode.hash_slow(),
            code: Some(revert_bytecode),
            ..Default::default()
        },
    );
    for sender in [sender_1, sender_2, sender_3, sender_4] {
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: initial_balance,
                ..Default::default()
            },
        );
    }

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
            basefee: base_fee,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(4), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );

    macro_rules! read_bal {
        ($account:expr) => {
            executor
                .evm_mut()
                .db_mut()
                .basic($account)
                .expect("account read")
                .map(|info| info.balance)
                .unwrap_or_default()
        };
    }

    let rewards_0 = read_bal!(REWARDS_ADDRESS);
    let s1_before = read_bal!(sender_1);
    let gas_1 = executor
        .execute_transaction(tx_1)
        .expect("tx 1 should execute")
        .tx_gas_used();
    let s1_after = read_bal!(sender_1);
    let rewards_1 = read_bal!(REWARDS_ADDRESS);
    let rewards_delta_1 = rewards_1 - rewards_0;
    let burned_1 = (s1_before - s1_after) - rewards_delta_1;
    assert_eq!(rewards_delta_1, U256::from(tip_1) * U256::from(gas_1));
    assert_eq!(burned_1, U256::from(base_fee) * U256::from(gas_1));

    let s2_before = read_bal!(sender_2);
    let gas_2 = executor
        .execute_transaction(tx_2)
        .expect("tx 2 should execute")
        .tx_gas_used();
    let s2_after = read_bal!(sender_2);
    let rewards_2 = read_bal!(REWARDS_ADDRESS);
    let rewards_delta_2 = rewards_2 - rewards_1;
    let burned_2 = (s2_before - s2_after) - rewards_delta_2;
    assert_eq!(rewards_delta_2, U256::from(tip_2) * U256::from(gas_2));
    assert_eq!(burned_2, U256::from(base_fee) * U256::from(gas_2));

    let s3_before = read_bal!(sender_3);
    let gas_3 = executor
        .execute_transaction(tx_3)
        .expect("reverting tx 3 should still commit a status=0 receipt")
        .tx_gas_used();
    let s3_after = read_bal!(sender_3);
    let rewards_3 = read_bal!(REWARDS_ADDRESS);
    let rewards_delta_3 = rewards_3 - rewards_2;
    let burned_3 = (s3_before - s3_after) - rewards_delta_3;
    assert_eq!(rewards_delta_3, U256::from(tip_3) * U256::from(gas_3));
    assert_eq!(burned_3, U256::from(base_fee) * U256::from(gas_3));

    let s4_before = read_bal!(sender_4);
    let declined = executor
        .execute_transaction_with_commit_condition(tx_4, |_| CommitChanges::No)
        .expect("declined tx 4 should not error");
    assert!(
        declined.is_none(),
        "declined commit condition must return None"
    );
    let s4_after = read_bal!(sender_4);
    let rewards_4 = read_bal!(REWARDS_ADDRESS);
    let rewards_delta_4 = rewards_4 - rewards_3;
    let burned_4 = (s4_before - s4_after) - rewards_delta_4;
    assert_eq!(
        s4_before - s4_after,
        U256::ZERO,
        "declined tx 4 must not debit sender"
    );
    assert_eq!(rewards_delta_4, U256::ZERO);
    assert_eq!(burned_4, U256::ZERO);

    assert_eq!(executor.receipts().len(), 3);
    assert!(executor.receipts()[0].success);
    assert!(executor.receipts()[1].success);
    assert!(!executor.receipts()[2].success);

    let (_evm, _result) = executor.finish().expect("executor finish should succeed");

    // Independent per-tx oracle sum plus pre-existing old_rest.
    let total_burned = burned_1 + burned_2 + burned_3 + burned_4;
    let formula_burned = U256::from(base_fee) * U256::from(gas_1 + gas_2 + gas_3);
    assert_eq!(total_burned, formula_burned);

    let total_with_old_rest = total_burned + old_rest;
    let expected_limit = total_with_old_rest / UNITS_PER_PROTOCOL_MINOR;
    let expected_rest = total_with_old_rest % UNITS_PER_PROTOCOL_MINOR;
    assert!(expected_limit > U256::ZERO);
    assert!(expected_rest > U256::ZERO);

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer: REWARDS_ADDRESS,
        validators: vec![REWARDS_ADDRESS],
    };
    let mut provider = DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        let actual_limit = promis.promis_limit().unwrap();
        let actual_rest = promis.rest_minor().unwrap();
        assert_eq!(actual_limit, expected_limit);
        assert_eq!(actual_rest, expected_rest);
        assert_eq!(
            actual_limit * UNITS_PER_PROTOCOL_MINOR + actual_rest,
            total_burned + old_rest,
            "§1.8 control identity must hold across cumulative burns + old_rest"
        );
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);

        // §1.8 Control check: Gratis pledged total supply and ValidatorSet active stake are untouched.
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        assert_eq!(vs.get_active_validators().unwrap().len(), 0);
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}

fn scenario_b_eip4844_blob_tx_settles_base_and_blob_fees() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );

    let base_fee: u64 = 100_000_000_000; // 10^11 wei
    let tip: u128 = 7_000_000_000;
    let blob_gasprice: u128 = 50_000_000; // 5 * 10^7 wei per blob gas

    let mut kzg_hash = B256::with_last_byte(0x42);
    kzg_hash[0] = VERSIONED_HASH_VERSION_KZG;
    let blob_versioned_hashes = vec![kzg_hash];

    let (sender, blob_tx) = sign_tx_with_key(
        TxEip4844 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: u128::from(base_fee) + tip,
            max_priority_fee_per_gas: tip,
            to: Address::ZERO,
            value: U256::ZERO,
            access_list: Default::default(),
            blob_versioned_hashes,
            max_fee_per_blob_gas: blob_gasprice * 2,
            input: Bytes::new(),
        },
        0x25,
    );
    assert_eq!(blob_tx.tx().blob_gas_used(), Some(DATA_GAS_PER_BLOB));

    let initial_balance = U256::from(100_000_000_000_000_000u64);
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();
    seed_promis_limit_marker(&mut db);
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: initial_balance,
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
            .with_spec_and_mainnet_gas_params(SpecId::CANCUN),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: base_fee,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: DATA_GAS_PER_BLOB * 3,
                blob_gasprice,
            }),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );

    let gas_used = executor
        .execute_transaction(blob_tx)
        .expect("EIP-4844 blob tx should execute under Cancun")
        .tx_gas_used();
    let (_evm, result) = executor.finish().expect("executor finish should succeed");
    assert_eq!(result.blob_gas_used, DATA_GAS_PER_BLOB);

    let sender_after = state
        .basic(sender)
        .expect("sender read")
        .map(|info| info.balance)
        .unwrap_or_default();
    let rewards_after = state
        .basic(REWARDS_ADDRESS)
        .expect("rewards read")
        .map(|info| info.balance)
        .unwrap_or_default();

    let expected_tip = U256::from(tip) * U256::from(gas_used);
    assert_eq!(rewards_after, expected_tip);

    let expected_base_burn = U256::from(base_fee) * U256::from(gas_used);
    let expected_blob_burn = U256::from(blob_gasprice) * U256::from(DATA_GAS_PER_BLOB);
    let independent_burned_oracle = (initial_balance - sender_after) - rewards_after;
    assert_eq!(
        independent_burned_oracle,
        expected_base_burn + expected_blob_burn,
        "sender debit minus tip must equal burned base fee plus burned blob fee"
    );

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer: REWARDS_ADDRESS,
        validators: vec![REWARDS_ADDRESS],
    };
    let mut provider = DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        assert_eq!(
            promis.promis_limit().unwrap(),
            independent_burned_oracle / UNITS_PER_PROTOCOL_MINOR
        );
        assert_eq!(
            promis.rest_minor().unwrap(),
            independent_burned_oracle % UNITS_PER_PROTOCOL_MINOR
        );
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}

fn scenario_c_protocol_fee_waiver_burns_zero_under_nonzero_basefee() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let validator = address!("0x1111111111111111111111111111111111111111");
    let pk = dummy_pubkey(0xA1);
    let zero_fee_tx = test_oracle_submit_vote_tx()
        .try_into_recovered()
        .expect("oracle submitVote tx signer should recover");
    let feeder = Address::from(*zero_fee_tx.signer());

    let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut seed_storage, |storage| {
        seed_registered_active_validator(storage.clone(), validator, &pk);
        let mut validator_set = outbe_validatorset::contract::ValidatorSet::new(storage);
        validator_set.set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            feeder,
        )?;
        Ok::<_, PrecompileError>(())
    })
    .expect("test genesis state must be seeded");

    let mut db = cache_db_from_storage(seed_storage);
    seed_promis_limit_marker(&mut db);
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
            basefee: 100_000_000_000,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );

    executor
        .execute_transaction(zero_fee_tx)
        .expect("delegated zero-fee oracle vote should execute");
    assert_eq!(executor.receipts().len(), 1);
    assert!(executor.receipts()[0].success);
    assert_eq!(executor.current_block_burned_fees, U256::ZERO);

    let (_evm, _result) = executor.finish().expect("executor finish should succeed");

    let read_ctx = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp: 1,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1,
        proposer: REWARDS_ADDRESS,
        validators: vec![validator],
    };
    let mut provider = DirectStorageProvider::new(&mut state, read_ctx);
    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        assert_eq!(promis.promis_limit().unwrap(), U256::ZERO);
        assert_eq!(promis.rest_minor().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(PROMIS_LIMIT_ADDRESS).unwrap(), U256::ZERO);
        Ok::<_, PrecompileError>(())
    })
    .unwrap();
}

fn scenario_d_promis_limit_overflow_fails_the_block_with_typed_error() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    let base_fee: u64 = 100_000_000_000; // 21_000 * 10^11 = 2.1 * 10^12 >= 1 protocol minor
    let (sender, tx) = sign_tx_with_key(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: u128::from(base_fee),
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        },
        0x26,
    );

    let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut seed_storage, |storage| {
        PromisLimitContract::new(storage).checked_credit(U256::MAX)
    })
    .unwrap();

    let mut db = cache_db_from_storage(seed_storage);
    seed_promis_limit_marker(&mut db);
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: U256::from(100_000_000_000_000_000u64),
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
            basefee: base_fee,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };
    let evm = config.evm_with_env(&mut state, evm_env);
    let mut ctx = execution_ctx(Some(1), Bytes::new());
    ctx.execute_outbe_block_hooks = false;
    let mut executor = OutbeBlockExecutor::new(
        EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
        fixtures::executor_inputs_from_ctx(&ctx, None, false),
    );

    executor
        .execute_transaction(tx)
        .expect("paid tx should execute before end-of-block settlement");

    let assert_overflow_fails_block = |err: &BlockExecutionError| match err {
        BlockExecutionError::Internal(inner) => {
            let settlement_err = inner
                .downcast_other::<BurnedFeeSettlementError>()
                .expect("internal error must downcast to BurnedFeeSettlementError");
            assert!(
                matches!(
                    settlement_err,
                    BurnedFeeSettlementError::Internal(PrecompileError::FatalWithCause(cause))
                        if matches!(
                            cause.downcast_ref::<outbe_promislimit::errors::PromisLimitError>(),
                            Some(outbe_promislimit::errors::PromisLimitError::Overflow)
                        )
                ),
                "expected Internal(FatalWithCause(Overflow)), got {settlement_err:?}"
            );
        }
        other => panic!("expected BlockExecutionError::Internal, got {other:?}"),
    };

    let prep_err = executor
        .prepare_final_header_artifacts(0)
        .expect_err("overflow in prepare_final_header_artifacts must fail the block");
    assert_overflow_fails_block(&prep_err);

    // Settlement failed before setting burned_fees_settled, so finish() fails the same way.
    let Err(finish_err) = executor.finish() else {
        panic!("overflow in finish() must fail the block");
    };
    assert_overflow_fails_block(&finish_err);
}

fn scenario_e_proposer_state_hook_pre_freeze_idempotence_and_validator_parity() {
    let chain_spec = cancun_chain_spec();
    let receipt_builder = reth_ethereum::evm::RethReceiptBuilder::default();
    let config = OutbeEvmConfig::new(
        chain_spec.clone(),
        std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()),
    );
    // 21_000 * 100_000_001 = 2_100_000_021_000 = 2 * 10^12 + 100_000_021_000.
    // With old_rest = 500_000_000_000, total = 2_600_000_021_000 = 2 * 10^12 + 600_000_021_000.
    let base_fee: u64 = 100_000_001;
    let old_rest = U256::from(500_000_000_000u64);
    let expected_limit = U256::from(2u64);
    let expected_rest = U256::from(600_000_021_000u64);
    let tip: u128 = 5_000_000_000;
    let (sender, tx) = sign_tx_with_key(
        TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: 21_000,
            max_fee_per_gas: u128::from(base_fee) + tip,
            max_priority_fee_per_gas: tip,
            to: TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
            access_list: Default::default(),
        },
        0x27,
    );

    let make_state = || {
        let mut seed_storage = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut seed_storage, |storage| {
            use outbe_primitives::addresses::SYSTEM_ADDRESS;
            use outbe_promislimit::BurnSource;
            PromisLimitContract::new(storage).burn_to_promis_limit(
                SYSTEM_ADDRESS,
                old_rest,
                BurnSource::BaseFee,
            )
        })
        .unwrap();
        let mut db = cache_db_from_storage(seed_storage);
        seed_promis_limit_marker(&mut db);
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(100_000_000_000_000_000u64),
                ..Default::default()
            },
        );
        State::builder()
            .with_database(db)
            .with_bundle_update()
            .build()
    };
    let make_env = || EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(1u64),
            gas_limit: 30_000_000,
            basefee: base_fee,
            beneficiary: REWARDS_ADDRESS,
            timestamp: U256::from(1u64),
            ..Default::default()
        },
    };

    // 1. Proposer path: attach OnStateHook, execute tx, call prepare_final_header_artifacts,
    // detach hook (as payload_builder.rs:216-220 does), then call finish().
    let mut proposer_state = make_state();
    let hook_promis_storage_updates: Arc<Mutex<Vec<(U256, U256)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let hook_capture = Arc::clone(&hook_promis_storage_updates);
    proposer_state.set_state_hook(Some(Box::new(move |evm_state: revm::state::EvmState| {
        if let Some(account) = evm_state.get(&PROMIS_LIMIT_ADDRESS) {
            for (slot, val) in &account.storage {
                if val.is_changed() {
                    hook_capture
                        .lock()
                        .unwrap()
                        .push((*slot, val.present_value()));
                }
            }
        }
    })));

    let proposer_result = {
        let evm = config.evm_with_env(&mut proposer_state, make_env());
        let mut ctx = execution_ctx(Some(1), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        let mut proposer_executor = OutbeBlockExecutor::new(
            EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
            fixtures::executor_inputs_from_ctx(&ctx, None, false),
        );
        proposer_executor
            .execute_transaction(tx.clone())
            .expect("proposer tx should execute");
        assert!(
            hook_promis_storage_updates.lock().unwrap().is_empty(),
            "burned fees are settled at end-of-block, not per-tx"
        );

        proposer_executor
            .prepare_final_header_artifacts(0)
            .expect("prepare_final_header_artifacts should settle burned fees");
        // Detach state hook before finish(), mirroring payload_builder.rs:220.
        proposer_executor.evm_mut().db_mut().set_state_hook(None);

        let captured = hook_promis_storage_updates.lock().unwrap().clone();
        assert_eq!(
            captured.len(),
            2,
            "OnStateHook must observe both value and rest_minor slot writes before hook detach"
        );
        assert!(
            captured.contains(&(U256::ZERO, expected_limit))
                && captured.contains(&(U256::from(1u64), expected_rest)),
            "OnStateHook must record exact promis_limit and rest_minor slots: {captured:?}"
        );

        let (_evm, result) = proposer_executor
            .finish()
            .expect("proposer finish should succeed idempotently");
        result
    };
    proposer_state.merge_transitions(BundleRetention::Reverts);

    // 2. Validator path: execute tx and call finish() directly without prepare_final_header_artifacts.
    let mut validator_state = make_state();
    let validator_result = {
        let evm = config.evm_with_env(&mut validator_state, make_env());
        let mut ctx = execution_ctx(Some(1), Bytes::new());
        ctx.execute_outbe_block_hooks = false;
        let mut validator_executor = OutbeBlockExecutor::new(
            EthBlockExecutor::new(evm, ctx.inner.clone(), &chain_spec, &receipt_builder),
            fixtures::executor_inputs_from_ctx(&ctx, None, false),
        );
        validator_executor
            .execute_transaction(tx)
            .expect("validator tx should execute");
        let (_evm, result) = validator_executor
            .finish()
            .expect("validator finish should succeed");
        result
    };
    validator_state.merge_transitions(BundleRetention::Reverts);

    // Verify idempotence (credited exactly once on proposer and identical on validator).
    for state in [&mut proposer_state, &mut validator_state] {
        let read_ctx = BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: 1,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: 1,
            proposer: REWARDS_ADDRESS,
            validators: vec![REWARDS_ADDRESS],
        };
        let mut provider = DirectStorageProvider::new(state, read_ctx);
        StorageHandle::enter(&mut provider, |storage| {
            let promis = PromisLimitContract::new(storage);
            assert_eq!(promis.promis_limit().unwrap(), expected_limit);
            assert_eq!(promis.rest_minor().unwrap(), expected_rest);
            Ok::<_, PrecompileError>(())
        })
        .unwrap();
    }

    // Verify exact receipts_root and state_root parity between proposer and validator paths.
    let proposer_receipt_blooms: Vec<_> = proposer_result
        .receipts
        .iter()
        .map(alloy_consensus::TxReceipt::with_bloom_ref)
        .collect();
    let validator_receipt_blooms: Vec<_> = validator_result
        .receipts
        .iter()
        .map(alloy_consensus::TxReceipt::with_bloom_ref)
        .collect();
    assert_eq!(
        alloy_consensus::proofs::calculate_receipt_root(&proposer_receipt_blooms),
        alloy_consensus::proofs::calculate_receipt_root(&validator_receipt_blooms),
        "proposer and validator must produce identical receipts_root"
    );
    assert_eq!(
        post_state_root(&proposer_state.bundle_state),
        post_state_root(&validator_state.bundle_state),
        "proposer (prepare + finish) and validator (finish only) must produce identical state_root"
    );
}
