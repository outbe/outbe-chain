use super::*;
use alloy_consensus::{SignableTransaction as _, TxEip1559, TxEip7702};
use alloy_eips::{
    eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718 as _, eip7702::Authorization,
};
use alloy_primitives::{Bytes, Signature, TxKind};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::{ORACLE_ADDRESS, OUTBE_SYSTEM_TX_ADDRESS};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::SignedTransaction as _;
use reth_transaction_pool::EthPooledTransaction;

const CHAIN_ID: u64 = 1;
fn pooled_tx(
    to: Address,
    input: Bytes,
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
) -> EthPooledTransaction {
    pooled_tx_with_gas(
        to,
        input,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        1_000_000,
    )
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
        address: outbe_zerofee::ZEROFEE_ADDRESS,
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
        gas_limit: outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: outbe_zerofee::ZEROFEE_ADDRESS,
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

#[test]
fn pool_admits_true_type4_bootstrap_with_one_atomic_unit() {
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };

    let (transaction, signer) = bootstrap_pooled_tx();
    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new()
        .with_chain_spec(chain_spec)
        .with_genesis_block();
    provider.add_account(signer, ExtendedAccount::new(0, U256::from(1)));
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .set_eip7702(true)
            .disable_balance_check()
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner);

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
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };

    let (transaction, signer) = bootstrap_pooled_tx();
    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new()
        .with_chain_spec(chain_spec)
        .with_genesis_block();
    provider.add_account(signer, ExtendedAccount::new(0, U256::ZERO));
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .set_eip7702(true)
            .disable_balance_check()
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner);

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

fn oracle_submit_vote_input() -> Bytes {
    outbe_oracle::precompile::IOracle::submitVoteCall {
        tuples: vec![outbe_oracle::precompile::IOracle::ExchangeRateTuple {
            base: outbe_oracle::api::COEN_ASSET,
            quote: outbe_oracle::api::currency_address(840),
            exchangeRate: U256::from(1_000_000u64),
            volume: U256::from(10_000_000_000u64),
        }],
    }
    .abi_encode()
    .into()
}

alloy_sol_types::sol! {
    function submitCheckpoint(
        uint32 domain,
        bytes32 root,
        uint32 index,
        bytes32 messageId,
        bytes signature
    );
}

fn renew_enclave_input() -> Bytes {
    use outbe_primitives::{
        tee_attestation_v1::{
            AttestationEvidenceV1, AttestationMode, AttestationOperationV1,
            GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1,
        },
        tee_registry_abi_v1::ITeeRegistryV1,
    };
    let intent = RegistrationIntentV1 {
        chain_id: [1; 32],
        genesis_hash: B256::repeat_byte(1),
        operation: AttestationOperationV1::RenewEnclave,
        attestation_mode: AttestationMode::GramineDirectDev,
        policy_hash: B256::repeat_byte(2),
        node_id: NodeIdV1 {
            reth_p2p_public: [
                0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce,
                0x87, 0x0b, 0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81,
                0x5b, 0x16, 0xf8, 0x17, 0x98,
            ],
        },
        enclave_id: B256::repeat_byte(3),
        binding_id: B256::repeat_byte(4),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 1,
        transition_nonce: 0,
        requested_valid_until: 1,
        recipient_x25519: [5; 32],
        attestation_ed25519: [6; 32],
        noise_responder_x25519: [7; 32],
        node_host_authorization_hash: B256::repeat_byte(8),
    };
    let evidence = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
        intent,
        dev_attestation_public: [6; 32],
        dev_signature: [9; 64],
        transition_key_ready_proof: None,
    })
    .encode_canonical()
    .unwrap();
    ITeeRegistryV1::renewEnclaveCall {
        evidence: evidence.into(),
        nodeSignature: vec![1; 65].into(),
        enclaveSignature: vec![2; 64].into(),
    }
    .abi_encode()
    .into()
}

fn hyperlane_submit_checkpoint_input() -> Bytes {
    submitCheckpointCall {
        domain: 54_322_345,
        root: B256::repeat_byte(1),
        index: 7,
        messageId: B256::repeat_byte(2),
        signature: Bytes::from(vec![3u8; 65]),
    }
    .abi_encode()
    .into()
}

#[test]
fn zero_fee_hooks_order_above_any_fee_paying_transaction() {
    use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let expensive_normal_tx = pooled_tx(Address::ZERO, Bytes::new(), u128::MAX, u128::MAX);

    for (hook, target, input) in [
        (
            ZeroFeeHookId::OracleSubmitVote,
            ORACLE_ADDRESS,
            oracle_submit_vote_input(),
        ),
        (
            ZeroFeeHookId::HyperlaneSubmitCheckpoint,
            HYPERLANE_CONTROLLER_ADDRESS,
            hyperlane_submit_checkpoint_input(),
        ),
        (
            ZeroFeeHookId::RenewEnclave,
            outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
            renew_enclave_input(),
        ),
    ] {
        assert_eq!(zero_fee_priority_class(hook), Some(1));
        let zero_fee_tx = pooled_tx(target, input, MIN_PROTOCOL_BASE_FEE as u128, 0);
        assert_eq!(ordering.priority(&zero_fee_tx, 0), Priority::Value((1, 0)));
        assert!(ordering.priority(&zero_fee_tx, 0) > ordering.priority(&expensive_normal_tx, 0));
    }
}

#[test]
fn malformed_zero_fee_marker_gets_no_pool_priority() {
    use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    for (target, selector) in [
        (
            ORACLE_ADDRESS,
            outbe_oracle::precompile::IOracle::submitVoteCall::SELECTOR,
        ),
        (HYPERLANE_CONTROLLER_ADDRESS, submitCheckpointCall::SELECTOR),
    ] {
        let malformed = pooled_tx(
            target,
            Bytes::copy_from_slice(&selector),
            MIN_PROTOCOL_BASE_FEE as u128,
            0,
        );
        assert_eq!(ordering.priority(&malformed, 0), Priority::None);
    }
}

#[test]
fn reserved_system_address_invalidates_valid_pool_outcome() {
    let reserved = pooled_tx(
        OUTBE_SYSTEM_TX_ADDRESS,
        Bytes::from_static(b"not-a-system-prefix"),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
    );
    let normal = pooled_tx(
        Address::ZERO,
        Bytes::new(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
    );
    assert!(is_reserved_system_tx(&reserved));
    assert!(!is_reserved_system_tx(&normal));

    let valid = TransactionValidationOutcome::Valid {
        balance: U256::MAX,
        state_nonce: 0,
        bytecode_hash: None,
        transaction: ValidTransaction::Valid(reserved),
        propagate: true,
        authorities: None,
    };
    let ValidOutcomeSplit::Valid(parts) = take_valid_outcome(valid) else {
        panic!("valid outcome should split");
    };

    match reject_reserved_system_tx_outcome(parts) {
        ReservedSystemTxPolicy::Reject(TransactionValidationOutcome::Invalid(tx, err)) => {
            assert!(is_reserved_system_tx(&tx));
            assert!(err
                .to_string()
                .contains("reserved system transaction address"));
        }
        other => panic!("expected reserved-address invalid outcome, got {other:?}"),
    }

    let normal_valid = TransactionValidationOutcome::Valid {
        balance: U256::from(123),
        state_nonce: 0,
        bytecode_hash: None,
        transaction: ValidTransaction::Valid(normal),
        propagate: true,
        authorities: None,
    };
    let ValidOutcomeSplit::Valid(normal_parts) = take_valid_outcome(normal_valid) else {
        panic!("normal valid outcome should split");
    };
    assert!(matches!(
        reject_reserved_system_tx_outcome(normal_parts),
        ReservedSystemTxPolicy::Continue(_)
    ));
}

use alloy_primitives::address;
use outbe_primitives::addresses::ZEROFEE_ADDRESS;
use outbe_zerofee::{fixtures::sponsored_envelope, ZeroFeeTransaction};

const NON_VALIDATOR_SIGNER: Address = address!("0x9999999999999999999999999999999999999999");

// -----------------------------------------------------------------
// sponsorship_decision - the pure pool-admission decision core.
// These tests pin the EXACT composition that try_eip7702_sponsorship
// performs (delegation match -> classify -> precheck, no quota) without
// a provider mock. Thus `cargo test` catches a regression in the wiring
// here. The gated live e2e script is not the only check. Examples of such
// a regression:
// reordered checks, a dropped classify, an accidental quota gate, or a
// wrong delegation target match.
// -----------------------------------------------------------------

fn ok_sponsored_envelope<'a>() -> ZeroFeeTransaction<'a> {
    sponsored_envelope(NON_VALIDATOR_SIGNER, &[])
}

#[test]
fn decision_not_sponsored_when_no_delegation() {
    let out = sponsorship_decision(NON_VALIDATOR_SIGNER, None, &ok_sponsored_envelope())
        .expect("no delegation must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_not_sponsored_when_delegated_elsewhere() {
    // Delegated to a non-paymaster address -> normal fee path.
    let out = sponsorship_decision(
        NON_VALIDATOR_SIGNER,
        Some(ORACLE_ADDRESS),
        &ok_sponsored_envelope(),
    )
    .expect("foreign delegation must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_accepts_well_formed_delegated_envelope() {
    let out = sponsorship_decision(
        NON_VALIDATOR_SIGNER,
        Some(ZEROFEE_ADDRESS),
        &ok_sponsored_envelope(),
    )
    .expect("valid sponsored tx must be accepted");
    assert_eq!(out, SponsorshipOutcome::Accepted);
}

#[test]
fn decision_value_bearing_delegated_tx_falls_through_to_normal_path() {
    // Delegated, but the envelope carries native value, so
    // it is NOT a sponsorship request. It must fall through to the
    // normal fee path (NotSponsored). The pool must NOT reject it.
    // EIP-7702 delegation is additive and must never block a normal tx.
    let mut tx = ok_sponsored_envelope();
    tx.call.value = U256::from(1);
    let out = sponsorship_decision(NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("value-bearing delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_paying_delegated_tx_falls_through_to_normal_path() {
    // The core fix: a delegated account that sets a tip
    // (priority_fee > 0) is paying, not requesting sponsorship.
    // It must reach the normal cost-vs-balance gate, so a signer
    // can keep transacting (and paying) after the daily free quota
    // is exhausted.
    let mut tx = ok_sponsored_envelope();
    tx.call.max_priority_fee_per_gas = Some(1);
    let out = sponsorship_decision(NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("paying delegated tx must not error");
    assert_eq!(
        out,
        SponsorshipOutcome::NotSponsored,
        "priority_fee>0 from a delegated account must be a normal paid tx"
    );
}

#[test]
fn decision_non_whitelisted_target_delegated_tx_falls_through() {
    // Delegated, zero-tip, but target not in the sponsored whitelist
    // -> not a sponsorship request -> normal path. The signer pays to
    // call whatever contract they like. Delegation does not gate it.
    let mut tx = ok_sponsored_envelope();
    tx.call.to = Some(ZEROFEE_ADDRESS); // not in SPONSORED_TARGET_WHITELIST
    let out = sponsorship_decision(NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("non-whitelisted delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_rejects_self_sponsorship() {
    let err = sponsorship_decision(
        ZEROFEE_ADDRESS,
        Some(ZEROFEE_ADDRESS),
        &ok_sponsored_envelope(),
    )
    .expect_err("self-sponsorship must be rejected in pool precheck");
    assert!(
        err.to_string()
            .contains("zero-fee signer is not authorized"),
        "{err}"
    );
}

#[test]
fn pool_admits_delegated_sponsored_tx_even_when_daily_quota_is_exhausted() {
    use outbe_primitives::{
        addresses::AGENT_REWARD_ADDRESS,
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
        time::timestamp_to_date_key,
    };
    use outbe_zerofee::{
        authorize_sponsorship, pack_counter, ZeroFeeContract, ZeroFeePolicyError,
        FREE_TX_DAILY_LIMIT,
    };
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };

    let transaction = pooled_tx_with_gas(
        AGENT_REWARD_ADDRESS,
        Bytes::new(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        100_000,
    );
    let signer = transaction.sender();

    // Seed exhausted daily quota for `signer` in ZeroFee storage and confirm execution rejects it.
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
        // Successful authorization is opaque; compare only the rejection.
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

    let mut delegation_designator = Vec::with_capacity(23);
    delegation_designator.extend_from_slice(&[0xef, 0x01, 0x00]);
    delegation_designator.extend_from_slice(ZEROFEE_ADDRESS.as_slice());

    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = MockEthProvider::<reth_ethereum::EthPrimitives>::new()
        .with_chain_spec(chain_spec)
        .with_genesis_block();
    provider.add_account(
        signer,
        ExtendedAccount::new(0, U256::ZERO).with_bytecode(Bytes::from(delegation_designator)),
    );
    provider.add_account(
        ZEROFEE_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO).extend_storage(zerofee_slots),
    );

    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .set_eip7702(true)
            .disable_balance_check()
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner);

    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
        panic!("quota-exhausted delegated sponsored tx must still be admitted by pool, got {outcome:?}");
    };
    assert_eq!(balance, U256::MAX);
}
