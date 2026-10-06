mod result_vote_state;

use super::*;
use alloy_consensus::{SignableTransaction as _, Transaction as _, TxEip1559, TxEip7702};
use alloy_eips::{
    eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718 as _, eip7702::Authorization,
};
use alloy_primitives::{Bytes, Signature, TxKind, B256};
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
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(1));

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
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(1));

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

fn ocomp_submit_result_vote_input() -> Bytes {
    use outbe_ocomp_protocol::{
        abi::SUBMIT_LYSIS_RESULT_SELECTOR, encode_envelope, profile::poc_schema_limits,
        registry::ObjectKind, vote::ResultVotePrefixV1, OCB1_HEADER_LEN,
    };

    let prefix = ResultVotePrefixV1 {
        protocol_bundle_hash: B256::repeat_byte(0x31),
        job_id: B256::repeat_byte(0x32),
        attempt: 3,
        result_validator_set_epoch: 7,
        result_committee_set_hash: B256::repeat_byte(0x33),
        result_ocomp_binding_hash: B256::repeat_byte(0x34),
        ocomp_key_hash: B256::repeat_byte(0x35),
        key_epoch: 1,
    };
    let mut body = Vec::new();
    body.extend_from_slice(prefix.protocol_bundle_hash.as_slice());
    body.extend_from_slice(prefix.job_id.as_slice());
    body.extend_from_slice(&prefix.attempt.to_be_bytes());
    body.extend_from_slice(&prefix.result_validator_set_epoch.to_be_bytes());
    body.extend_from_slice(prefix.result_committee_set_hash.as_slice());
    body.extend_from_slice(prefix.result_ocomp_binding_hash.as_slice());
    body.extend_from_slice(prefix.ocomp_key_hash.as_slice());
    body.extend_from_slice(&prefix.key_epoch.to_be_bytes());
    body.resize(180, 0);
    let payload = encode_envelope(ObjectKind::ResultVoteV1, &body, poc_schema_limits().codec)
        .expect("canonical OCOMP vote prefix must encode");
    assert_eq!(payload.len(), OCB1_HEADER_LEN + body.len());

    let padded_len = (payload.len() + 31) & !31;
    let mut input = vec![0_u8; 68 + padded_len];
    input[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
    input[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
    input[36..68].copy_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
    input[68..68 + payload.len()].copy_from_slice(&payload);
    input.into()
}

#[test]
fn only_submit_vote_has_reserved_zero_fee_priority_class() {
    assert_eq!(
        zero_fee_priority_class(ZeroFeeHookId::OracleSubmitVote),
        Some(1)
    );
}

#[test]
fn zero_fee_submit_vote_orders_above_any_fee_paying_transaction() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let zero_fee_vote = pooled_tx(
        ORACLE_ADDRESS,
        oracle_submit_vote_input(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
    );
    let expensive_normal_tx = pooled_tx(Address::ZERO, Bytes::new(), u128::MAX, u128::MAX);

    assert!(ordering.priority(&zero_fee_vote, 0) > ordering.priority(&expensive_normal_tx, 0));
}

#[test]
fn canonical_ocomp_system_carrier_has_reserved_priority() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let system_carrier = pooled_tx_with_gas(
        outbe_ocomp_protocol::abi::METADOSIS_ADDRESS,
        ocomp_submit_result_vote_input(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    );
    let expensive_normal_tx = pooled_tx(Address::ZERO, Bytes::new(), u128::MAX, u128::MAX);

    assert!(ordering.priority(&system_carrier, 0) > ordering.priority(&expensive_normal_tx, 0));
}

#[test]
fn truncated_ocomp_vote_gets_no_pool_priority() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let malformed_vote = pooled_tx_with_gas(
        outbe_ocomp_protocol::abi::METADOSIS_ADDRESS,
        Bytes::copy_from_slice(&outbe_ocomp_protocol::abi::SUBMIT_LYSIS_RESULT_SELECTOR),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    );

    assert_eq!(ordering.priority(&malformed_vote, 0), Priority::None);
}

#[test]
fn pool_classifies_ocomp_carrier_before_ordinary_intrinsic_gas() {
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };

    let carrier = pooled_tx_with_gas(
        outbe_ocomp_protocol::abi::METADOSIS_ADDRESS,
        ocomp_submit_result_vote_input(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    );
    let provider = MockEthProvider::default().with_genesis_block();
    provider.add_account(
        carrier.sender(),
        ExtendedAccount::new(carrier.nonce(), U256::MAX),
    );
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(1));

    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, carrier),
    );
    let TransactionValidationOutcome::Invalid(_, error) = outcome else {
        panic!("carrier without pinned state must be rejected, got {outcome:?}");
    };
    assert!(
        !error.to_string().contains("intrinsic"),
        "state authorization must run before ordinary intrinsic gas: {error}"
    );
}

#[test]
fn ocomp_carrier_before_lifecycle_activation_stays_invalid() {
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };

    let carrier = pooled_tx_with_gas(
        outbe_ocomp_protocol::abi::METADOSIS_ADDRESS,
        ocomp_submit_result_vote_input(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        outbe_ocomp_protocol::system_carrier::OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    );
    let provider = MockEthProvider::default().with_genesis_block();
    provider.add_account(
        carrier.sender(),
        ExtendedAccount::new(carrier.nonce(), U256::MAX),
    );
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .build(InMemoryBlobStore::default());
    let validator = OutbeTransactionValidator::new(inner, OcompLifecycleActivation::at_block(2));

    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, carrier),
    );
    let TransactionValidationOutcome::Invalid(_, error) = outcome else {
        panic!("inactive lifecycle must stay invalid, got {outcome:?}");
    };
    assert!(error.to_string().contains("not active"));
}

#[test]
fn invalid_result_vote_is_a_bad_pool_transaction() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::InvalidCarrier {
            reason: "invalid signature".to_owned(),
        },
    );
    let ocomp_admission::ResultVotePoolDecision::Reject(reason) = decision else {
        panic!("invalid carrier must be rejected");
    };
    let error = ocomp_admission::OutbeOcompSystemCarrierPoolError(reason);
    assert!(error.is_bad_transaction());
}

#[test]
fn state_unavailable_result_vote_is_not_blamed_on_the_peer() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::StateUnavailable {
            source: outbe_primitives::error::PrecompileError::Storage("disk".to_owned()),
        },
    );
    assert!(matches!(
        decision,
        ocomp_admission::ResultVotePoolDecision::Temporary(_)
    ));
}

#[test]
fn corrupt_result_vote_state_is_not_blamed_on_the_peer() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::CorruptCommittedState {
            source: outbe_primitives::error::PrecompileError::Fatal("torn job".to_owned()),
        },
    );
    assert!(matches!(
        decision,
        ocomp_admission::ResultVotePoolDecision::Temporary(_)
    ));
}

#[test]
fn deadline_passed_result_vote_stays_admissible() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::DeadlinePassed {
            represented_validator: Address::repeat_byte(0x11),
        },
    );
    assert!(matches!(
        decision,
        ocomp_admission::ResultVotePoolDecision::Admit
    ));
}

#[test]
fn deadline_due_unclosed_result_vote_is_temporary() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::DeadlineDueUnclosed {
            deadline_height: 40,
        },
    );
    assert!(matches!(
        decision,
        ocomp_admission::ResultVotePoolDecision::Temporary(
            ocomp_admission::OcompCarrierTemporaryError::DeadlineDueUnclosed {
                deadline_height: 40
            }
        )
    ));
}

#[test]
fn not_yet_open_result_vote_is_temporary() {
    let decision = ocomp_admission::result_vote_pool_decision(
        outbe_metadosis::api::ResultVoteCarrierAdmission::NotYetOpen { open_height: 9 },
    );
    assert!(matches!(
        decision,
        ocomp_admission::ResultVotePoolDecision::Temporary(_)
    ));
}

#[test]
fn malformed_zero_fee_marker_gets_no_pool_priority() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let malformed_vote = pooled_tx(
        ORACLE_ADDRESS,
        Bytes::copy_from_slice(&outbe_oracle::precompile::IOracle::submitVoteCall::SELECTOR),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
    );

    assert_eq!(ordering.priority(&malformed_vote, 0), Priority::None);
}

#[test]
fn reserved_system_address_is_detected_for_pool_rejection() {
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
}

#[test]
fn reserved_system_address_invalidates_valid_pool_outcome() {
    let reserved = pooled_tx(
        OUTBE_SYSTEM_TX_ADDRESS,
        Bytes::from_static(b"not-a-system-prefix"),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
    );
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
}

// -----------------------------------------------------------------
// EIP-7702 sponsorship admission. These tests pin the pool/executor contract:
//   1. classify_sponsorship rejects shape violations (the executor would
//      do the same, so the codes must match).
//   2. precheck_sponsorship rejects self-sponsorship and has no
//      account balance or quota input.
// The pool's `try_eip7702_sponsorship` chains classify + precheck.
// These tests cover both individually and the policy code surface.
// -----------------------------------------------------------------

use alloy_primitives::address;
use outbe_primitives::addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS};
use outbe_zerofee::{classify_sponsorship, precheck_sponsorship, ZeroFeeTransaction};

const NON_VALIDATOR_SIGNER: Address = address!("0x9999999999999999999999999999999999999999");

fn sponsored_envelope<'a>(input: &'a [u8]) -> ZeroFeeTransaction<'a> {
    ZeroFeeTransaction {
        signer: NON_VALIDATOR_SIGNER,
        to: Some(AGENT_REWARD_ADDRESS),
        value: U256::ZERO,
        input,
        gas_limit: 100_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: Some(0),
    }
}

#[test]
fn pool_classify_accepts_well_formed_sponsored_envelope() {
    assert!(classify_sponsorship(&sponsored_envelope(&[])).is_ok());
}

#[test]
fn pool_classify_rejects_non_zero_value_with_plan_code_113() {
    let mut tx = sponsored_envelope(&[]);
    tx.value = U256::from(1);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 113);
}

#[test]
fn pool_classify_rejects_oversized_gas_with_plan_code_114() {
    let mut tx = sponsored_envelope(&[]);
    tx.gas_limit = outbe_zerofee::FREE_TX_DAILY_GAS_LIMIT + 1;
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 114);
}

#[test]
fn pool_classify_rejects_oversized_calldata_with_plan_code_115() {
    let big = vec![0u8; outbe_zerofee::FREE_TX_DAILY_CALLDATA_BYTES + 1];
    let tx = sponsored_envelope(&big);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 115);
}

#[test]
fn pool_classify_rejects_target_outside_whitelist_with_plan_code_116() {
    let mut tx = sponsored_envelope(&[]);
    // ZEROFEE_ADDRESS itself is not in the SPONSORED_TARGET_WHITELIST.
    tx.to = Some(ZEROFEE_ADDRESS);
    let err = classify_sponsorship(&tx).unwrap_err();
    assert_eq!(err.code(), 116);
}

#[test]
fn pool_precheck_rejects_self_sponsorship_with_code_107() {
    let err = precheck_sponsorship(ZEROFEE_ADDRESS).unwrap_err();
    assert_eq!(err.code(), 107);
}

#[test]
fn pool_precheck_accepts_non_paymaster_signer() {
    assert!(precheck_sponsorship(NON_VALIDATOR_SIGNER).is_ok());
}

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
    sponsored_envelope(&[])
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
    tx.value = U256::from(1);
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
    tx.max_priority_fee_per_gas = Some(1);
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
    tx.to = Some(ZEROFEE_ADDRESS); // not in SPONSORED_TARGET_WHITELIST
    let out = sponsorship_decision(NON_VALIDATOR_SIGNER, Some(ZEROFEE_ADDRESS), &tx)
        .expect("non-whitelisted delegated tx must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_does_not_quota_check() {
    // sponsorship_decision has no storage access at all. By
    // construction, it cannot perform a quota check. A delegated,
    // well-formed tx is always Accepted, regardless of how many slots
    // the signer burned. The executor enforces the quota. This test
    // pins the F2 contract at the pool layer.
    for _ in 0..20 {
        let out = sponsorship_decision(
            NON_VALIDATOR_SIGNER,
            Some(ZEROFEE_ADDRESS),
            &ok_sponsored_envelope(),
        )
        .unwrap();
        assert_eq!(out, SponsorshipOutcome::Accepted);
    }
}
