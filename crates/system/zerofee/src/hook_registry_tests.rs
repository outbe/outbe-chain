//! Characterization tests for every registered validator hook.
//!
//! Each hook checks the same transaction fields in a fixed order. These tests
//! pin that order through the public registry, so a refactor cannot change
//! which rejection a transaction with several defects receives. They also pin
//! which signer each hook authorizes.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_intexfactory::precompile::IIntexFactory;
use outbe_ocomp_protocol::transaction_call::TransactionCallFields;
use outbe_oracle::precompile::IOracle;
use outbe_primitives::addresses::{
    HYPERLANE_CONTROLLER_ADDRESS, INTEX_FACTORY_ADDRESS, ORACLE_ADDRESS,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction,
};
use crate::test_support::activate_validator_with_delegate;

const SIGNER: Address = Address::repeat_byte(0x44);
const VALIDATOR: Address = Address::repeat_byte(0x11);
const STRANGER: Address = Address::repeat_byte(0x55);

/// The envelope limits and a valid calldata sample of one hook.
struct HookCase {
    hook: ZeroFeeHookId,
    target: Address,
    selector: [u8; 4],
    valid_input: Vec<u8>,
    min_max_fee_per_gas: u128,
    max_calldata_bytes: usize,
    max_gas_limit: u64,
    malformed_reason: &'static str,
    /// Delegate role that lets a signer act for its validator.
    delegate_role: ValidatorDelegateRole,
}

impl HookCase {
    /// A transaction that passes every stateless check of this hook.
    fn accepted_tx<'a>(&self, input: &'a [u8]) -> ZeroFeeTransaction<'a> {
        ZeroFeeTransaction {
            signer: SIGNER,
            call: TransactionCallFields {
                to: Some(self.target),
                value: U256::ZERO,
                input,
                gas_limit: self.max_gas_limit,
                max_fee_per_gas: self.min_max_fee_per_gas,
                max_priority_fee_per_gas: Some(0),
            },
        }
    }

    /// Calldata with the hook selector that is one byte above the size limit.
    fn oversized_input(&self) -> Vec<u8> {
        let mut input = self.selector.to_vec();
        input.resize(self.max_calldata_bytes + 1, 0);
        input
    }
}

/// Classifies `tx` with the registry and authorizes its fee waiver.
/// Returns `Ok(None)` when no hook claims `tx`.
fn waive_fee(
    storage: StorageHandle<'_>,
    tx: &ZeroFeeTransaction<'_>,
) -> Result<Option<ZeroFeeAuthorization>, ZeroFeePolicyError> {
    crate::registry()
        .classify(tx)?
        .map(|candidate| crate::registry().authorize_fee_waiver(storage, candidate))
        .transpose()
}

fn hook_cases() -> Vec<HookCase> {
    vec![
        HookCase {
            hook: ZeroFeeHookId::OracleSubmitVote,
            target: ORACLE_ADDRESS,
            selector: IOracle::submitVoteCall::SELECTOR,
            valid_input: IOracle::submitVoteCall { tuples: vec![] }.abi_encode(),
            min_max_fee_per_gas: crate::oracle::MIN_ZERO_FEE_ORACLE_MAX_FEE_PER_GAS,
            max_calldata_bytes: crate::oracle::MAX_ZERO_FEE_ORACLE_CALLDATA_BYTES,
            max_gas_limit: crate::oracle::MAX_ZERO_FEE_ORACLE_GAS_LIMIT,
            malformed_reason: "submitVote(ExchangeRateTuple[]) decode failed",
            delegate_role: ValidatorDelegateRole::Oracle,
        },
        HookCase {
            hook: ZeroFeeHookId::IntexFactoryPayContributorBatch,
            target: INTEX_FACTORY_ADDRESS,
            selector: IIntexFactory::payContributorBatchCall::SELECTOR,
            valid_input: IIntexFactory::payContributorBatchCall {
                worldwideDay: 20_260_725,
                startIndex: 0,
                leaves: vec![],
                proof: vec![],
            }
            .abi_encode(),
            min_max_fee_per_gas:
                crate::intexfactory::MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS,
            max_calldata_bytes: crate::intexfactory::MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES,
            max_gas_limit: crate::intexfactory::MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT,
            malformed_reason: "payContributorBatch decode failed",
            delegate_role: ValidatorDelegateRole::Ocomp,
        },
        HookCase {
            hook: ZeroFeeHookId::HyperlaneSubmitCheckpoint,
            target: HYPERLANE_CONTROLLER_ADDRESS,
            selector: IHyperlaneController::submitCheckpointCall::SELECTOR,
            valid_input: crate::test_support::checkpoint_calldata().to_vec(),
            min_max_fee_per_gas: crate::hyperlane::MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
            max_calldata_bytes: crate::hyperlane::MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES,
            max_gas_limit: crate::hyperlane::MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT,
            malformed_reason: "submitCheckpoint(uint32,bytes32,uint32,bytes32,bytes) decode failed",
            delegate_role: ValidatorDelegateRole::Oracle,
        },
    ]
}

#[test]
fn accepted_envelope_names_the_hook_and_the_signer() {
    for case in hook_cases() {
        let tx = case.accepted_tx(&case.valid_input);
        assert_eq!(
            crate::registry().classify(&tx),
            Ok(Some(ZeroFeeCandidate::new(case.hook, SIGNER))),
            "{:?}",
            case.hook
        );
    }
}

#[test]
fn unclaimed_shapes_use_the_normal_fee_path_before_any_limit_check() {
    for case in hook_cases() {
        let oversized = case.oversized_input();
        let mut defective = case.accepted_tx(&oversized);
        defective.call.value = U256::from(1_u64);
        defective.call.max_fee_per_gas = 0;
        defective.call.gas_limit = case.max_gas_limit + 1;

        let mut wrong_target = defective;
        wrong_target.call.to = Some(Address::ZERO);
        let mut creation = defective;
        creation.call.to = None;
        let mut short_selector = case.accepted_tx(&case.selector[..3]);
        short_selector.call.value = U256::from(1_u64);
        let mut tipped = defective;
        tipped.call.max_priority_fee_per_gas = Some(1);
        let mut legacy_fee = defective;
        legacy_fee.call.max_priority_fee_per_gas = None;

        for tx in [wrong_target, creation, short_selector, tipped, legacy_fee] {
            assert_eq!(crate::registry().classify(&tx), Ok(None), "{:?}", case.hook);
        }
    }
}

#[test]
fn claimed_envelope_rejections_follow_the_fixed_check_order() {
    for case in hook_cases() {
        let oversized = case.oversized_input();
        let selector_only = case.selector.to_vec();

        let mut low_fee_cap = case.accepted_tx(&oversized);
        low_fee_cap.call.max_fee_per_gas = case.min_max_fee_per_gas - 1;
        low_fee_cap.call.value = U256::from(1_u64);
        low_fee_cap.call.gas_limit = case.max_gas_limit + 1;

        let mut funded = case.accepted_tx(&oversized);
        funded.call.value = U256::from(1_u64);
        funded.call.gas_limit = case.max_gas_limit + 1;

        let mut too_large = case.accepted_tx(&oversized);
        too_large.call.gas_limit = case.max_gas_limit + 1;

        let mut greedy = case.accepted_tx(&selector_only);
        greedy.call.gas_limit = case.max_gas_limit + 1;

        let malformed = case.accepted_tx(&selector_only);

        let expected = [
            (
                low_fee_cap,
                ZeroFeePolicyError::FeeCapTooLow {
                    max_fee_per_gas: case.min_max_fee_per_gas - 1,
                    minimum: case.min_max_fee_per_gas,
                },
            ),
            (funded, ZeroFeePolicyError::NonZeroValue),
            (
                too_large,
                ZeroFeePolicyError::CalldataTooLarge {
                    size: case.max_calldata_bytes + 1,
                    limit: case.max_calldata_bytes,
                },
            ),
            (
                greedy,
                ZeroFeePolicyError::GasLimitTooHigh {
                    gas_limit: case.max_gas_limit + 1,
                    limit: case.max_gas_limit,
                },
            ),
            (
                malformed,
                ZeroFeePolicyError::MalformedCalldata(case.malformed_reason.to_string()),
            ),
        ];
        for (tx, error) in expected {
            assert_eq!(
                crate::registry().classify(&tx),
                Err(error),
                "{:?}",
                case.hook
            );
        }
    }
}

#[test]
fn only_a_delegate_for_the_hook_role_acts_for_its_validator() {
    for case in hook_cases() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let delegated = case.accepted_tx(&case.valid_input);
            let stranger = ZeroFeeTransaction {
                signer: STRANGER,
                ..delegated
            };
            assert_eq!(
                waive_fee(storage.clone(), &delegated),
                Err(ZeroFeePolicyError::UnauthorizedSigner),
                "{:?}",
                case.hook
            );

            activate_validator_with_delegate(
                storage.clone(),
                VALIDATOR,
                case.delegate_role,
                SIGNER,
            )
            .unwrap();
            let waived = ZeroFeeAuthorization {
                hook: case.hook,
                subject: VALIDATOR,
            };
            assert_eq!(
                waive_fee(storage.clone(), &delegated),
                Ok(Some(waived)),
                "{:?}",
                case.hook
            );
            assert_eq!(
                waive_fee(storage, &stranger),
                Err(ZeroFeePolicyError::UnauthorizedSigner),
                "{:?}",
                case.hook
            );
        });
    }
}

#[test]
fn oracle_vote_waiver_ends_when_the_validator_has_voted() {
    let cases = hook_cases();
    let case = &cases[0];
    assert_eq!(case.hook, ZeroFeeHookId::OracleSubmitVote);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        activate_validator_with_delegate(
            storage.clone(),
            VALIDATOR,
            ValidatorDelegateRole::Oracle,
            SIGNER,
        )
        .unwrap();
        let tx = case.accepted_tx(&case.valid_input);
        assert!(waive_fee(storage.clone(), &tx).unwrap().is_some());

        outbe_oracle::schema::OracleContract::new(storage.clone())
            .vote_exists
            .write(&VALIDATOR, true)
            .unwrap();
        assert_eq!(
            waive_fee(storage, &tx),
            Err(ZeroFeePolicyError::AlreadyVoted)
        );
    });
}
