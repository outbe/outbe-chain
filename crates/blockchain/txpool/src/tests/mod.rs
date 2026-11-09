use super::*;
use alloy_consensus::{SignableTransaction as _, TxEip1559, TxEip7702};
use alloy_eips::{
    eip1559::MIN_PROTOCOL_BASE_FEE, eip2718::Encodable2718 as _, eip7702::Authorization,
};
use alloy_primitives::{address, Bytes, Signature};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use outbe_primitives::{
    addresses::{
        HYPERLANE_CONTROLLER_ADDRESS, ORACLE_ADDRESS, TEE_REGISTRY_ADDRESS, ZEROFEE_ADDRESS,
    },
    transaction_call::TransactionCallFields,
    zero_fee_types::{
        BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, ZeroFeeAuthorization,
        ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction,
    },
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::SignedTransaction as _;
use reth_transaction_pool::{validate::ValidTransaction, EthPooledTransaction};
use std::sync::Arc;

const CHAIN_ID: u64 = 1;
const NON_VALIDATOR_SIGNER: Address = address!("0x9999999999999999999999999999999999999999");
const GRANTED_SELECTOR: [u8; 4] = [0x11, 0x22, 0x33, 0x44];

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
        to: alloy_primitives::TxKind::Call(to),
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

fn bare_view(signer: Address) -> ZeroFeeTransaction<'static> {
    ZeroFeeTransaction {
        signer,
        call: TransactionCallFields {
            to: Some(Address::ZERO),
            value: U256::ZERO,
            input: &[],
            gas_limit: 21_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: Some(0),
        },
    }
}

/// Test policy. Each group sets only the answers that group reads.
#[derive(Debug, Default)]
struct ScriptedPolicy {
    classify: ClassifyAnswer,
    sponsorship_ok: bool,
    precheck: PrecheckAnswer,
    bootstrap: bool,
    authorize_ok: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ClassifyAnswer {
    #[default]
    None,
    HookWhenInputLongerThanSelector,
}

#[derive(Debug, Clone, Copy, Default)]
enum PrecheckAnswer {
    #[default]
    Ok,
    UnauthorizedSigner,
}

impl TxPoolZeroFeePolicy for ScriptedPolicy {
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        match self.classify {
            ClassifyAnswer::None => Ok(None),
            ClassifyAnswer::HookWhenInputLongerThanSelector => {
                let hook = match tx.call.to {
                    Some(address) if address == ORACLE_ADDRESS => ZeroFeeHookId::OracleSubmitVote,
                    Some(address) if address == HYPERLANE_CONTROLLER_ADDRESS => {
                        ZeroFeeHookId::HyperlaneSubmitCheckpoint
                    }
                    Some(address) if address == TEE_REGISTRY_ADDRESS => ZeroFeeHookId::RenewEnclave,
                    _ => return Ok(None),
                };
                if tx.call.input.len() <= 4 {
                    return Err(ZeroFeePolicyError::MalformedCalldata("short".into()));
                }
                Ok(Some(ZeroFeeCandidate::new(hook, tx.signer)))
            }
        }
    }

    fn authorize(
        &self,
        _storage: outbe_primitives::storage::StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        if self.authorize_ok {
            Ok(ZeroFeeAuthorization {
                hook: candidate.hook,
                subject: candidate.signer,
            })
        } else {
            Err(ZeroFeePolicyError::UnauthorizedSigner)
        }
    }

    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        self.bootstrap.then_some(BootstrapCandidate {
            signer: tx.signer,
            nonce: tx.nonce,
        })
    }

    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool {
        !account.balance.is_zero() && account.nonce == candidate.nonce && account.code_empty
    }

    fn classify_sponsorship(&self, tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
        if self.sponsorship_ok && tx.call.input.get(..4) == Some(&GRANTED_SELECTOR) {
            return Ok(());
        }
        if self.sponsorship_ok && self.classify == ClassifyAnswer::None && tx.call.input.is_empty()
        {
            return Ok(());
        }
        Err(ZeroFeePolicyError::FeeCapTooLow {
            max_fee_per_gas: tx.call.max_fee_per_gas,
            minimum: 1,
        })
    }

    fn precheck_sponsorship(&self, signer: Address) -> Result<(), ZeroFeePolicyError> {
        match self.precheck {
            PrecheckAnswer::Ok if signer != ZEROFEE_ADDRESS => Ok(()),
            PrecheckAnswer::UnauthorizedSigner | PrecheckAnswer::Ok => {
                Err(ZeroFeePolicyError::UnauthorizedSigner)
            }
        }
    }
}

fn policy(script: ScriptedPolicy) -> Arc<dyn TxPoolZeroFeePolicy> {
    Arc::new(script)
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
    let tx = TxEip7702 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: outbe_primitives::zero_fee_types::FREE_TX_BOOTSTRAP_GAS_LIMIT,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: ZEROFEE_ADDRESS,
        value: U256::ZERO,
        access_list: Default::default(),
        authorization_list: vec![signed_authorization],
        input: Bytes::from_static(&GRANTED_SELECTOR),
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

fn validator_with(
    provider: reth_provider::test_utils::MockEthProvider,
    script: ScriptedPolicy,
) -> OutbeTransactionValidator<
    reth_provider::test_utils::MockEthProvider,
    EthPooledTransaction,
    reth_ethereum::evm::EthEvmConfig,
> {
    use reth_transaction_pool::{
        blobstore::InMemoryBlobStore, validate::EthTransactionValidatorBuilder,
    };
    let inner =
        EthTransactionValidatorBuilder::new(provider, reth_ethereum::evm::EthEvmConfig::mainnet())
            .set_block_gas_limit(30_000_000)
            .set_eip7702(true)
            .disable_balance_check()
            .build(InMemoryBlobStore::default());
    OutbeTransactionValidator::new(inner, policy(script))
}

fn funded_provider(
    signer: Address,
    balance: U256,
    bytecode: Option<Bytes>,
) -> reth_provider::test_utils::MockEthProvider {
    use reth_provider::test_utils::ExtendedAccount;
    let chain_spec = reth_chainspec::ChainSpecBuilder::mainnet()
        .prague_activated()
        .build();
    let provider = reth_provider::test_utils::MockEthProvider::new()
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

#[test]
fn pool_admits_bootstrap_when_policy_accepts_one_atomic_unit() {
    let (transaction, signer) = bootstrap_pooled_tx();
    let provider = funded_provider(signer, U256::from(1), None);
    let validator = validator_with(
        provider,
        ScriptedPolicy {
            bootstrap: true,
            ..ScriptedPolicy::default()
        },
    );
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
        panic!("accepted bootstrap must be admitted, got {outcome:?}");
    };
    assert_eq!(balance, U256::MAX, "bootstrap must receive the pool waiver");
}

#[test]
fn pool_rejects_bootstrap_when_policy_rejects_zero_balance() {
    let (transaction, signer) = bootstrap_pooled_tx();
    let provider = funded_provider(signer, U256::ZERO, None);
    let validator = validator_with(
        provider,
        ScriptedPolicy {
            bootstrap: true,
            ..ScriptedPolicy::default()
        },
    );
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Invalid(_, error) = outcome else {
        panic!("rejected bootstrap must fall through to overdraft, got {outcome:?}");
    };
    assert!(matches!(
        error,
        InvalidPoolTransactionError::Overdraft { balance, .. } if balance.is_zero()
    ));
}

#[test]
fn zero_fee_hooks_order_above_any_fee_paying_transaction() {
    let script = ScriptedPolicy {
        classify: ClassifyAnswer::HookWhenInputLongerThanSelector,
        ..ScriptedPolicy::default()
    };
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::new(policy(script));
    let expensive_normal_tx = pooled_tx(Address::ZERO, Bytes::new(), u128::MAX, u128::MAX);
    let payload = Bytes::from_static(&[1, 2, 3, 4, 5]);

    for (hook, target) in [
        (ZeroFeeHookId::OracleSubmitVote, ORACLE_ADDRESS),
        (
            ZeroFeeHookId::HyperlaneSubmitCheckpoint,
            HYPERLANE_CONTROLLER_ADDRESS,
        ),
        (ZeroFeeHookId::RenewEnclave, TEE_REGISTRY_ADDRESS),
    ] {
        assert_eq!(
            TxPoolZeroFeePolicy::priority_class(ordering.policy.as_ref(), hook),
            Some(1)
        );
        let zero_fee_tx = pooled_tx(target, payload.clone(), MIN_PROTOCOL_BASE_FEE as u128, 0);
        assert_eq!(ordering.priority(&zero_fee_tx, 0), Priority::Value((1, 0)));
        assert!(ordering.priority(&zero_fee_tx, 0) > ordering.priority(&expensive_normal_tx, 0));
    }
}

#[test]
fn malformed_zero_fee_marker_gets_no_pool_priority() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::new(policy(ScriptedPolicy {
        classify: ClassifyAnswer::HookWhenInputLongerThanSelector,
        ..ScriptedPolicy::default()
    }));
    for target in [ORACLE_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS] {
        let malformed = pooled_tx(
            target,
            Bytes::copy_from_slice(&GRANTED_SELECTOR),
            MIN_PROTOCOL_BASE_FEE as u128,
            0,
        );
        assert_eq!(ordering.priority(&malformed, 0), Priority::None);
    }
}

#[test]
fn default_policy_keeps_normal_tip_priority() {
    let ordering = OutbeTransactionOrdering::<EthPooledTransaction>::default();
    let tx = pooled_tx(Address::ZERO, Bytes::new(), u128::MAX, 1);
    assert!(matches!(ordering.priority(&tx, 0), Priority::Value((0, _))));
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

#[test]
fn decision_not_sponsored_when_no_delegation() {
    let script = ScriptedPolicy {
        sponsorship_ok: true,
        ..ScriptedPolicy::default()
    };
    let out = sponsorship_decision(
        &script,
        NON_VALIDATOR_SIGNER,
        None,
        &bare_view(NON_VALIDATOR_SIGNER),
    )
    .expect("no delegation must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_not_sponsored_when_delegated_elsewhere() {
    let script = ScriptedPolicy {
        sponsorship_ok: true,
        ..ScriptedPolicy::default()
    };
    let out = sponsorship_decision(
        &script,
        NON_VALIDATOR_SIGNER,
        Some(ORACLE_ADDRESS),
        &bare_view(NON_VALIDATOR_SIGNER),
    )
    .expect("foreign delegation must not error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_accepts_when_policy_accepts_delegated_envelope() {
    let script = ScriptedPolicy {
        sponsorship_ok: true,
        ..ScriptedPolicy::default()
    };
    let out = sponsorship_decision(
        &script,
        NON_VALIDATOR_SIGNER,
        Some(ZEROFEE_ADDRESS),
        &bare_view(NON_VALIDATOR_SIGNER),
    )
    .expect("accepted sponsorship must be admitted");
    assert_eq!(out, SponsorshipOutcome::Accepted);
}

#[test]
fn decision_falls_through_when_policy_rejects_the_envelope() {
    let script = ScriptedPolicy::default();
    let out = sponsorship_decision(
        &script,
        NON_VALIDATOR_SIGNER,
        Some(ZEROFEE_ADDRESS),
        &bare_view(NON_VALIDATOR_SIGNER),
    )
    .expect("envelope mismatch must fall back, not return a policy error");
    assert_eq!(out, SponsorshipOutcome::NotSponsored);
}

#[test]
fn decision_rejects_self_sponsorship() {
    let script = ScriptedPolicy {
        sponsorship_ok: true,
        precheck: PrecheckAnswer::UnauthorizedSigner,
        ..ScriptedPolicy::default()
    };
    let err = sponsorship_decision(
        &script,
        ZEROFEE_ADDRESS,
        Some(ZEROFEE_ADDRESS),
        &bare_view(ZEROFEE_ADDRESS),
    )
    .expect_err("self-sponsorship must be rejected in pool precheck");
    assert!(
        err.to_string()
            .contains("zero-fee signer is not authorized"),
        "{err}"
    );
}

#[test]
fn pool_selector_controls_sponsored_balance_waiver_and_paid_fallback() {
    use outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS;

    let gas_limit = 600_000;
    let funded_balance = U256::from(gas_limit) * U256::from(MIN_PROTOCOL_BASE_FEE);
    for (input, account_balance, sponsored) in [
        (Bytes::copy_from_slice(&GRANTED_SELECTOR), U256::ZERO, true),
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
        let provider = funded_provider(signer, account_balance, Some(zerofee_delegation()));
        let validator = validator_with(
            provider,
            ScriptedPolicy {
                sponsorship_ok: true,
                ..ScriptedPolicy::default()
            },
        );
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
fn pool_admits_delegated_sponsored_tx_without_a_quota_read() {
    use outbe_primitives::addresses::AGENT_REWARD_ADDRESS;

    let transaction = pooled_tx_with_gas(
        AGENT_REWARD_ADDRESS,
        Bytes::new(),
        MIN_PROTOCOL_BASE_FEE as u128,
        0,
        100_000,
    );
    let signer = transaction.sender();
    let provider = funded_provider(signer, U256::ZERO, Some(zerofee_delegation()));
    let validator = validator_with(
        provider,
        ScriptedPolicy {
            sponsorship_ok: true,
            ..ScriptedPolicy::default()
        },
    );
    let outcome = futures::executor::block_on(
        validator.validate_transaction(TransactionOrigin::External, transaction),
    );
    let TransactionValidationOutcome::Valid { balance, .. } = outcome else {
        panic!("delegated sponsored tx must be admitted without a quota read, got {outcome:?}");
    };
    assert_eq!(balance, U256::MAX);
}
