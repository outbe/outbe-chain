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
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_runtime::OutbeRuntime;
use outbe_system_common::addresses::{
    AGENT_REWARD_ADDRESS, TRIBUTE_FACTORY_ADDRESS, ZEROFEE_ADDRESS,
};
use outbe_system_common::time::timestamp_to_date_key;
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
        let expected_balance = if sponsored {
            U256::MAX
        } else {
            account_balance
        };
        if sponsored || !account_balance.is_zero() {
            let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
                panic!("funded or sponsored transaction must be admitted, got {outcome:?}");
            };
            assert_eq!(balance, expected_balance);
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

mod equivocation {
    use super::*;
    use alloy_evm::{block::BlockExecutor as _, eth::EthBlockExecutionCtx};
    use commonware_codec::{DecodeExt as _, Encode as _};
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_utils::ordered::Set;
    use outbe_evm::config::{OutbeBlockExecutionCtx, OutbeEvmConfig};
    use outbe_primitives::block::BlockContext;
    use outbe_primitives::storage::direct::DirectStorageProvider;
    use outbe_slashindicator::{precompile::ISlashIndicator, schema::SlashIndicator, test_signing};
    use outbe_staking::contract::Staking;
    use outbe_system_common::addresses::{SLASH_INDICATOR_ADDRESS, STAKING_ADDRESS};
    use outbe_validatorset::{
        contract::ValidatorSet,
        state::{CommitteeEntry, CommitteeSnapshot},
        ValidatorLifecycle,
    };
    use outbe_zerofee::MAX_ZERO_FEE_SLASH_EQUIVOCATION_GAS_LIMIT as GAS_LIMIT;
    use reth_evm::{ConfigureEvm as _, EvmEnv};
    use revm::{
        context::{BlockEnv, CfgEnv},
        database::{CacheDB, Database as _, State},
        database_interface::EmptyDBTyped,
        primitives::hardfork::SpecId,
        state::{AccountInfo, Bytecode},
    };

    const ACCUSED: Address = Address::repeat_byte(0xaa);
    const OWNER: Address = Address::repeat_byte(0xbb);
    const AMOUNT: u64 = 1_000_000;

    fn reporter(index: u16) -> PrivateKeySigner {
        let mut bytes = [0u8; 32];
        bytes[30..].copy_from_slice(&(index + 1).to_be_bytes());
        PrivateKeySigner::from_slice(&bytes).unwrap()
    }

    fn members() -> Vec<CommitteeEntry> {
        let mut entries: Vec<_> = (1..=255)
            .map(|seed| CommitteeEntry {
                address: reporter(seed - 1).address(),
                consensus_pubkey: bls12381::PrivateKey::from_seed(u64::from(seed))
                    .public_key()
                    .encode()
                    .as_ref()
                    .try_into()
                    .unwrap(),
            })
            .chain([CommitteeEntry {
                address: ACCUSED,
                consensus_pubkey: test_signing::keypair(77).unwrap().1.to_bytes(),
            }])
            .collect();
        entries.sort_by_key(|entry| entry.consensus_pubkey);
        entries
    }

    fn committee() -> Set<bls12381::PublicKey> {
        Set::from_iter_dedup(
            members().iter().map(|entry| {
                bls12381::PublicKey::decode(entry.consensus_pubkey.as_slice()).unwrap()
            }),
        )
    }

    fn seed(exiting: bool) -> HashMapStorageProvider {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        provider.enter(|storage| {
            let mut vs = ValidatorSet::new(storage.clone());
            vs.test_configure_registry(OWNER).unwrap();
            vs.set_config_max_validators(256).unwrap();
            vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
            let snapshot = CommitteeSnapshot {
                committee: members(),
                vrf_material_version: 1,
                vrf_group_public_key_bytes: vec![0x11; 96],
                vrf_public_polynomial_hash: B256::ZERO,
            };
            for member in &snapshot.committee {
                vs.test_register_active_validator(
                    member.address,
                    &member.consensus_pubkey,
                    U256::from(64 * AMOUNT),
                )
                .unwrap();
            }
            vs.test_seed_committee_ring(OWNER, &snapshot).unwrap();
            assert_eq!(vs.validator_count().unwrap(), 256);
            let mut staking = Staking::new(storage.clone());
            staking.config_min_stake.write(U256::from(1)).unwrap();
            staking.config_unbonding_period.write(100_000).unwrap();
            for member in &snapshot.committee {
                staking
                    .stake_amount
                    .write(&member.address, U256::from(64 * AMOUNT))
                    .unwrap();
            }
            staking
                .total_staked
                .write(U256::from(256 * 64 * AMOUNT))
                .unwrap();
            storage
                .increase_balance(STAKING_ADDRESS, U256::from(256 * 64 * AMOUNT))
                .unwrap();
            // Reserve the final queue entry for the actual Active -> Exiting transition.
            let entries = 63 + u32::from(exiting);
            for _ in 0..entries {
                staking.unstake(ACCUSED, U256::from(AMOUNT)).unwrap();
            }
            assert_eq!(
                vs.validator_lifecycle(ACCUSED).unwrap().is_active_status(),
                !exiting
            );
            assert_eq!(snapshot.committee.len(), 256);
            assert_eq!(
                vs.get_active_validators().unwrap().len(),
                256 - usize::from(exiting)
            );
            assert_eq!(
                staking.per_val_unbonding_len.read(&ACCUSED).unwrap(),
                entries
            );
            for index in 0..entries {
                assert_eq!(
                    staking.unbonding_amount.read(&index).unwrap(),
                    U256::from(AMOUNT)
                );
                assert!(staking.unbonding_complete_time.read(&index).unwrap() > 1);
            }
        });
        provider
    }

    fn calldata(kind: usize, view: u64) -> Bytes {
        let (sk, pk) = test_signing::keypair(77).unwrap();
        let committee = committee();
        let notarize = outbe_primitives::proof::notarize_namespace(&committee);
        let finalize = outbe_primitives::proof::finalize_namespace(&committee);
        let nullify = outbe_primitives::proof::nullify_namespace(&committee);
        let namespaces = [
            (&notarize, &notarize),
            (&finalize, &finalize),
            (&nullify, &finalize),
        ];
        let (first_namespace, namespace) = namespaces[kind];
        let proposals = [
            test_signing::proposal(1, view, view - 1, [1; 32]),
            test_signing::proposal(1, view, view - 1, [1; 32]),
            test_signing::nullify_payload(1, view),
        ];
        let first_payload = &proposals[kind];
        let block1 = test_signing::signed_evidence(
            &sk,
            &pk,
            first_namespace,
            first_payload,
            test_signing::POP_DST,
        )
        .into();
        let block2 = test_signing::signed_evidence(
            &sk,
            &pk,
            namespace,
            &test_signing::proposal(1, view, view - 1, [2; 32]),
            test_signing::POP_DST,
        )
        .into();
        match kind {
            0 => ISlashIndicator::submitConflictingNotarizeEvidenceCall { block1, block2 }
                .abi_encode()
                .into(),
            1 => ISlashIndicator::submitConflictingFinalizeEvidenceCall { block1, block2 }
                .abi_encode()
                .into(),
            _ => ISlashIndicator::submitNullifyFinalizeEvidenceCall {
                nullifyBlock: block1,
                finalizeBlock: block2,
            }
            .abi_encode()
            .into(),
        }
    }

    fn transaction(index: u16, input: Bytes) -> TransactionSigned {
        let tx = TxEip1559 {
            chain_id: CHAIN_ID,
            nonce: 0,
            gas_limit: GAS_LIMIT,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE.into(),
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(SLASH_INDICATOR_ADDRESS),
            value: U256::ZERO,
            input,
            access_list: Default::default(),
        };
        let signature = reporter(index)
            .sign_hash_sync(&tx.signature_hash())
            .unwrap();
        tx.into_signed(signature).into()
    }

    fn assert_pool_admission(seeded: &HashMapStorageProvider, kind: usize) {
        let provider = funded_provider(reporter(0).address(), U256::ZERO, None);
        for address in seeded
            .storage
            .keys()
            .map(|(address, _)| *address)
            .collect::<std::collections::HashSet<_>>()
        {
            let slots = seeded
                .storage
                .iter()
                .filter(|((a, _), _)| *a == address)
                .map(|((_, slot), value)| (B256::from(slot.to_be_bytes::<32>()), *value));
            provider.add_account(
                address,
                ExtendedAccount::new(0, U256::ZERO).extend_storage(slots),
            );
        }
        let validator = validator_for(provider);
        // All reporters pass admission against the same pre-block state.
        for index in 0..6 {
            let tx = transaction(index, calldata(kind, 5 + u64::from(index % 2)));
            let length = tx.encode_2718_len();
            let pooled = EthPooledTransaction::new(tx.try_into_recovered().unwrap(), length);
            let outcome = futures::executor::block_on(
                validator.validate_transaction(TransactionOrigin::External, pooled),
            );
            assert!(
                matches!(outcome, TransactionValidationOutcome::Valid { balance, .. }
                    if balance == U256::MAX),
                "zero-balance evidence admission: {outcome:?}"
            );
        }
    }

    type TestState = State<CacheDB<EmptyDBTyped<reth_evm::execute::ProviderError>>>;

    fn assert_post_state(state: &mut TestState, exiting: bool) {
        for index in 0..6 {
            let account = state
                .basic(reporter(index).address())
                .unwrap()
                .unwrap_or_default();
            assert_eq!(account.balance, U256::ZERO);
            assert_eq!(account.nonce, u64::from(index == 0));
        }
        let context = BlockContext {
            block_number: 1,
            timestamp: 1,
            chain_id: CHAIN_ID,
            proposer: OWNER,
            block_gas_limit: 30_000_000,
            user_gas_used: 0,
            genesis_hash: B256::ZERO,
            genesis_timestamp: 0,
            validators: vec![],
        };
        let mut direct = DirectStorageProvider::new(state, context);
        StorageHandle::enter(&mut direct, |storage| {
            let si = SlashIndicator::new(storage.clone());
            assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 1);
            let lifecycle = ValidatorSet::new(storage.clone())
                .validator_lifecycle(ACCUSED)
                .unwrap();
            assert_eq!(matches!(lifecycle, ValidatorLifecycle::Exiting(_)), exiting);
            assert_eq!(
                matches!(lifecycle, ValidatorLifecycle::JailRetained(_)),
                !exiting
            );
            let staking = Staking::new(storage);
            assert_eq!(
                staking.stake_amount.read(&ACCUSED).unwrap(),
                U256::from(u64::from(!exiting) * AMOUNT * 92 / 100)
            );
            let entries = 63 + u32::from(exiting);
            for index in 0..entries {
                assert_eq!(
                    staking.unbonding_amount.read(&index).unwrap(),
                    U256::from(AMOUNT * 92 / 100)
                );
            }
        });
    }

    #[test]
    fn metered_slash_budget_pool_admission_and_reporter_conflict_cap() {
        outbe_primitives::proof::init_consensus_chain_id(CHAIN_ID).unwrap();
        for (kind, exiting) in (0..3).flat_map(|kind| [false, true].map(|exiting| (kind, exiting)))
        {
            let seeded = seed(exiting);
            assert_pool_admission(&seeded, kind);
            let mut db = CacheDB::<EmptyDBTyped<reth_evm::execute::ProviderError>>::default();
            // Seeded precompiles must survive EIP-161 touched-account cleanup.
            let marker = Bytecode::new_legacy([0xef].into());
            let system_account = AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker),
                ..Default::default()
            };
            for address in seeded.storage.keys().map(|(address, _)| *address) {
                db.insert_account_info(address, system_account.clone());
            }
            for ((address, slot), value) in seeded.storage {
                db.insert_account_storage(address, slot, value).unwrap();
            }
            db.insert_account_info(
                STAKING_ADDRESS,
                AccountInfo {
                    balance: U256::from(256 * 64 * AMOUNT),
                    ..system_account.clone()
                },
            );
            db.insert_account_info(SLASH_INDICATOR_ADDRESS, system_account);
            let mut state = State::builder()
                .with_database(db)
                .with_bundle_update()
                .build();
            let chain = Arc::new(
                reth_chainspec::ChainSpecBuilder::mainnet()
                    .build()
                    .map_header(outbe_primitives::OutbeHeader::new),
            );
            let config = OutbeEvmConfig::new(chain, Arc::new(OutbeRuntime::new()));
            let env = EvmEnv {
                cfg_env: CfgEnv::new()
                    .with_chain_id(CHAIN_ID)
                    .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
                block_env: BlockEnv {
                    number: U256::from(1),
                    timestamp: U256::from(1),
                    gas_limit: 30_000_000,
                    basefee: MIN_PROTOCOL_BASE_FEE,
                    beneficiary: OWNER,
                    ..Default::default()
                },
            };
            let evm = config.evm_with_env(&mut state, env);
            let ctx = OutbeBlockExecutionCtx {
                inner: EthBlockExecutionCtx {
                    parent_hash: B256::ZERO,
                    parent_beacon_block_root: None,
                    ommers: &[],
                    withdrawals: None,
                    extra_data: Bytes::new(),
                    tx_count_hint: Some(6),
                    slot_number: None,
                },
                timestamp_millis_part: 0,
                block_hash: None,
                block_state_root: None,
                expected_begin_system_txs: vec![],
                system_layout_error: None,
                parent_consensus_metadata: None,
                proposer_evm_address: None,
                execute_outbe_block_hooks: false,
                prebuilt_phase1_tx: None,
                parent_artifact_hint: None,
                pending_tee_bootstrap: None,
                execution_read_budget: None,
            };
            {
                let mut executor = config.create_executor(evm, ctx);
                executor
                    .execute_transaction(
                        transaction(0, calldata(kind, 5))
                            .try_into_recovered()
                            .unwrap(),
                    )
                    .unwrap();
                assert!(
                    executor.receipts()[0].success,
                    "{:?}",
                    executor.receipts()[0]
                );
                let gas = executor.receipts()[0].cumulative_gas_used;
                println!("equivocation kind={kind} exiting={exiting} committee=256 gas_used={gas}");
                assert!(gas > 0);
                assert!(gas < GAS_LIMIT);
                for index in 1..=4 {
                    let output = executor
                        .execute_transaction(
                            transaction(index, calldata(kind, 5 + u64::from(index % 2)))
                                .try_into_recovered()
                                .unwrap(),
                        )
                        .unwrap();
                    assert_eq!(output.tx_gas_used(), 21_000);
                    let receipt = &executor.receipts()[index as usize];
                    assert!(!receipt.success);
                    assert_eq!(receipt.logs.len(), 1);
                    assert_eq!(
                        receipt.logs[0].address,
                        outbe_primitives::addresses::ZERO_FEE_POLICY_LOG_ADDRESS
                    );
                    assert_eq!(
                        receipt.logs[0].data.topics()[0],
                        outbe_evm::failure_receipt::OUTBE_FAILURE_TOPIC0
                    );
                    assert_eq!(
                        receipt.logs[0].data.topics()[1],
                        B256::from(U256::from(108))
                    );
                    assert_eq!(receipt.cumulative_gas_used, gas + u64::from(index) * 21_000);
                }
                let before = executor.receipts().to_vec();
                let error = executor
                    .execute_transaction(
                        transaction(5, calldata(kind, 6))
                            .try_into_recovered()
                            .unwrap(),
                    )
                    .unwrap_err();
                assert!(matches!(
                    error,
                    alloy_evm::block::BlockExecutionError::Validation(
                        alloy_evm::block::BlockValidationError::InvalidTx { .. }
                    )
                ));
                assert!(error
                    .to_string()
                    .contains("zero-fee soft-failure cap (4) exceeded"));
                assert_eq!(executor.receipts(), before);
            }
            assert_post_state(&mut state, exiting);
        }
    }
}
