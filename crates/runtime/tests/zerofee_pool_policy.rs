//! Production pool policy. These scenarios run the real `OutbeRuntime`
//! zero-fee policy, not a scripted stand-in.

use std::sync::Arc;

use alloy_consensus::{SignableTransaction as _, TxEip1559, TxEip7702};
use alloy_eips::{
    eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718 as _, eip7702::Authorization,
};
use alloy_primitives::{address, Address, Bytes, Signature, TxKind, B256, U256};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, TRIBUTE_FACTORY_ADDRESS, ZEROFEE_ADDRESS};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::timestamp_to_date_key;
use outbe_runtime::OutbeRuntime;
use outbe_txpool::{
    sponsorship_decision, OutbeTransactionValidator, SponsorshipOutcome, TxPoolZeroFeePolicy,
};
use outbe_zerofee::constants::ELEVATED_SPONSORED_GAS;
use outbe_zerofee::fixtures::sponsored_envelope;
use outbe_zerofee::{
    authorize_sponsorship, pack_counter, ZeroFeeContract, ZeroFeePolicyError,
    FREE_TX_BOOTSTRAP_GAS_LIMIT, FREE_TX_DAILY_LIMIT,
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::SignedTransaction as _;
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
use reth_transaction_pool::{
    blobstore::InMemoryBlobStore, error::InvalidPoolTransactionError,
    validate::EthTransactionValidatorBuilder, EthPooledTransaction, PoolTransaction as _,
    TransactionOrigin, TransactionValidationOutcome, TransactionValidator as _,
};

const CHAIN_ID: u64 = 1;
const NON_VALIDATOR_SIGNER: Address = address!("0x9999999999999999999999999999999999999999");

fn policy() -> Arc<dyn TxPoolZeroFeePolicy> {
    Arc::new(OutbeRuntime::new())
}

fn pooled_tx_with_gas(
    to: Address,
    input: Bytes,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
    gas_limit: u64,
) -> EthPooledTransaction {
    let tx: TransactionSigned = TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        to: TxKind::Call(to),
        value: U256::ZERO,
        input,
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into();
    let encoded_length = tx.encode_2718_len();
    let recovered = tx
        .try_into_recovered()
        .expect("test transaction signer should recover");
    EthPooledTransaction::new(recovered, encoded_length)
}

fn bootstrap_pooled_tx() -> (EthPooledTransaction, Address) {
    let signer: PrivateKeySigner =
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse()
            .unwrap();
    let signer_address = signer.address();
    let authorization = Authorization {
        chain_id: U256::from(CHAIN_ID),
        address: ZEROFEE_ADDRESS,
        nonce: 1,
    };
    let signed_authorization = authorization.clone().into_signed(
        signer
            .sign_hash_sync(&authorization.signature_hash())
            .unwrap(),
    );
    let input: Bytes = outbe_zerofee::precompile::IZeroFee::authorizeSponsorshipCall {
        signer: signer_address,
    }
    .abi_encode()
    .into();
    let tx = TxEip7702 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: FREE_TX_BOOTSTRAP_GAS_LIMIT,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: ZEROFEE_ADDRESS,
        value: U256::ZERO,
        access_list: Default::default(),
        authorization_list: vec![signed_authorization],
        input,
    };
    let signature = signer.sign_hash_sync(&tx.signature_hash()).unwrap();
    let tx: TransactionSigned = tx.into_signed(signature).into();
    let encoded_length = tx.encode_2718_len();
    let recovered = tx.try_into_recovered().unwrap();
    (
        EthPooledTransaction::new(recovered, encoded_length),
        signer_address,
    )
}

fn validator_for(
    provider: MockEthProvider,
) -> OutbeTransactionValidator<
    MockEthProvider,
    EthPooledTransaction,
    reth_ethereum::evm::EthEvmConfig,
> {
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .set_block_gas_limit(30_000_000)
            .set_eip7702(true)
            .disable_balance_check()
            .build(InMemoryBlobStore::default());
    OutbeTransactionValidator::new(inner, policy())
}

fn funded_provider(signer: Address, balance: U256, bytecode: Option<Bytes>) -> MockEthProvider {
    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = MockEthProvider::new()
        .with_chain_spec(chain_spec)
        .with_genesis_block();
    let mut account = ExtendedAccount::new(0, balance);
    if let Some(bytecode) = bytecode {
        account = account.with_bytecode(bytecode);
    }
    provider.add_account(signer, account);
    provider
}

fn zerofee_delegation() -> Bytes {
    let mut designator = vec![0xef, 0x01, 0x00];
    designator.extend_from_slice(ZEROFEE_ADDRESS.as_slice());
    Bytes::from(designator)
}

fn ok_sponsored_envelope() -> outbe_zerofee::ZeroFeeTransaction<'static> {
    sponsored_envelope(NON_VALIDATOR_SIGNER, &[])
}

#[test]
fn pool_admits_true_type4_bootstrap_with_one_atomic_unit() {
    let (transaction, signer) = bootstrap_pooled_tx();
    let validator = validator_for(funded_provider(signer, U256::from(1), None));
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
        panic!("one-unit bootstrap must be admitted, got {outcome:?}");
    };
    assert_eq!(balance, U256::MAX, "bootstrap must receive the pool waiver");
}

#[test]
fn pool_rejects_true_type4_bootstrap_with_zero_balance() {
    let (transaction, signer) = bootstrap_pooled_tx();
    let validator = validator_for(funded_provider(signer, U256::ZERO, None));
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Invalid(_, error) = outcome else {
        panic!("zero-balance bootstrap must be rejected, got {outcome:?}");
    };
    assert!(matches!(
        error,
        InvalidPoolTransactionError::Overdraft { balance, .. } if balance.is_zero()
    ));
}

#[test]
fn decision_tribute_elevated_gas_requires_granted_selector() {
    let (_, selector, elevated_limit) = ELEVATED_SPONSORED_GAS
        .iter()
        .find(|(target, _, _)| *target == TRIBUTE_FACTORY_ADDRESS)
        .expect("TributeFactory grant");
    let unknown = [0xde, 0xad, 0xbe, 0xef];
    assert_ne!(selector, &unknown);
    let runtime = OutbeRuntime::new();
    for input in [
        selector.as_slice(),
        unknown.as_slice(),
        &[][..],
        &[1],
        &[1, 2],
        &[1, 2, 3],
    ] {
        for gas_limit in [500_000, 500_001, *elevated_limit, *elevated_limit + 1] {
            let mut tx = sponsored_envelope(NON_VALIDATOR_SIGNER, input);
            tx.call.to = Some(TRIBUTE_FACTORY_ADDRESS);
            tx.call.gas_limit = gas_limit;
            let expected = if gas_limit <= 500_000
                || (input == selector.as_slice() && gas_limit <= *elevated_limit)
            {
                SponsorshipOutcome::Accepted
            } else {
                SponsorshipOutcome::NotSponsored
            };
            assert_eq!(
                sponsorship_decision(&runtime, NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
                    .expect("envelope mismatch must fall back, not return a policy error"),
                expected,
                "input={input:?}, gas_limit={gas_limit}"
            );
        }
    }
}

#[test]
fn pool_tribute_selector_controls_sponsored_balance_waiver_and_paid_fallback() {
    let (_, selector, _) = ELEVATED_SPONSORED_GAS
        .iter()
        .find(|(target, _, _)| *target == TRIBUTE_FACTORY_ADDRESS)
        .expect("TributeFactory grant");
    let gas_limit = 600_000;
    let funded_balance = U256::from(gas_limit) * U256::from(MIN_PROTOCOL_BASE_FEE);
    for (input, account_balance, sponsored) in [
        (Bytes::copy_from_slice(selector), U256::ZERO, true),
        (
            Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            U256::ZERO,
            false,
        ),
        (
            Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            funded_balance,
            false,
        ),
    ] {
        let transaction = pooled_tx_with_gas(
            TRIBUTE_FACTORY_ADDRESS,
            input,
            MIN_PROTOCOL_BASE_FEE as u128,
            0,
            gas_limit,
        );
        let signer = transaction.sender();
        let validator = validator_for(funded_provider(
            signer,
            account_balance,
            Some(zerofee_delegation()),
        ));
        let outcome = futures::executor::block_on(
            validator.validate_transaction(TransactionOrigin::External, transaction),
        );
        if sponsored || !account_balance.is_zero() {
            let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
                panic!("funded or sponsored transaction must be admitted, got {outcome:?}");
            };
            assert_eq!(
                balance,
                if sponsored {
                    U256::MAX
                } else {
                    account_balance
                }
            );
        } else {
            let TransactionValidationOutcome::Invalid(_, error) = outcome else {
                panic!("unfunded paid fallback must be rejected, got {outcome:?}");
            };
            assert!(matches!(
                error,
                InvalidPoolTransactionError::Overdraft { cost, balance }
                    if cost == funded_balance && balance.is_zero()
            ));
        }
    }
}

#[test]
fn decision_value_bearing_delegated_tx_falls_through_to_normal_path() {
    let mut tx = ok_sponsored_envelope();
    tx.call.value = U256::from(1);
    let runtime = OutbeRuntime::new();
    let out = sponsorship_decision(&runtime, NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("value-bearing delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_paying_delegated_tx_falls_through_to_normal_path() {
    let mut tx = ok_sponsored_envelope();
    tx.call.max_priority_fee_per_gas = Some(1);
    let runtime = OutbeRuntime::new();
    let out = sponsorship_decision(&runtime, NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("paying delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_non_whitelisted_target_delegated_tx_falls_through() {
    let mut tx = ok_sponsored_envelope();
    tx.call.to = Some(ZEROFEE_ADDRESS);
    let runtime = OutbeRuntime::new();
    let out = sponsorship_decision(&runtime, NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("non-whitelisted delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn pool_admits_delegated_sponsored_tx_even_when_daily_quota_is_exhausted() {
    let transaction = pooled_tx_with_gas(
        AGENT_REWARD_ADDRESS,
        Bytes::new(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        100_000,
    );
    let signer = transaction.sender();
    let current_day = timestamp_to_date_key(0);
    let mut storage_provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage_provider, |storage| {
        let slot = ZeroFeeContract::new(storage.clone()).counter_slot(signer);
        storage
            .sstore(
                ZEROFEE_ADDRESS,
                slot,
                U256::from(pack_counter(current_day, FREE_TX_DAILY_LIMIT)),
            )
            .unwrap();
        let error = authorize_sponsorship(storage, signer, 0)
            .expect_err("execution-layer sponsorship must reject exhausted quota");
        assert_eq!(
            error,
            ZeroFeePolicyError::FreeTxDailyExhausted {
                used: FREE_TX_DAILY_LIMIT,
                limit: FREE_TX_DAILY_LIMIT,
            }
        );
    });
    let zerofee_slots = storage_provider
        .storage
        .iter()
        .filter(|((address, _), _)| *address == ZEROFEE_ADDRESS)
        .map(|((_, slot), value)| (B256::from(slot.to_be_bytes::<32>()), *value));

    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = MockEthProvider::new()
        .with_chain_spec(chain_spec)
        .with_genesis_block();
    provider.add_account(
        signer,
        ExtendedAccount::new(0, U256::ZERO).with_bytecode(zerofee_delegation()),
    );
    provider.add_account(
        ZEROFEE_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO).extend_storage(zerofee_slots),
    );
    let validator = validator_for(provider);
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
        panic!(
            "quota-exhausted delegated sponsored tx must still be admitted by pool, got {outcome:?}"
        );
    };
    assert_eq!(balance, U256::MAX);
}
