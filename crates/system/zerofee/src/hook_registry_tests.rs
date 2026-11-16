//! Characterization tests for every registered validator hook.
//!
//! Each hook checks the same transaction fields in a fixed order. These tests
//! pin that order through the public registry, so a refactor cannot change
//! which rejection a transaction with several defects receives. They also pin
//! which signer each hook authorizes.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_oracle::precompile::IOracle;
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::transaction_call::TransactionCallFields;
use outbe_system_common::addresses::{HYPERLANE_CONTROLLER_ADDRESS, ORACLE_ADDRESS};
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHookTag, ZeroFeePolicyError, ZeroFeeTransaction,
    HYPERLANE_SUBMIT_CHECKPOINT_HOOK, ORACLE_SUBMIT_VOTE_HOOK,
};
use crate::test_support::activate_validator_with_delegate;

const SIGNER: Address = Address::repeat_byte(0x44);
const VALIDATOR: Address = Address::repeat_byte(0x11);
const STRANGER: Address = Address::repeat_byte(0x55);

/// The envelope limits and a valid calldata sample of one hook.
struct HookCase {
    hook: ZeroFeeHookTag,
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
        .map(|candidate| crate::registry().authorize_fee_waiver(storage, tx, candidate))
        .transpose()
}

fn hook_cases() -> Vec<HookCase> {
    vec![
        HookCase {
            hook: ORACLE_SUBMIT_VOTE_HOOK,
            target: ORACLE_ADDRESS,
            selector: IOracle::submitVoteCall::SELECTOR,
            valid_input: IOracle::submitVoteCall {
                tuples: vec![],
                gas: vec![],
            }
            .abi_encode(),
            min_max_fee_per_gas: crate::oracle::MIN_ZERO_FEE_ORACLE_MAX_FEE_PER_GAS,
            max_calldata_bytes: crate::oracle::MAX_ZERO_FEE_ORACLE_CALLDATA_BYTES,
            max_gas_limit: crate::oracle::MAX_ZERO_FEE_ORACLE_GAS_LIMIT,
            malformed_reason: "zero-fee oracle vote calldata is malformed: \
                submitVote(ExchangeRateTuple[],GasTuple[]) decode failed",
            delegate_role: ValidatorDelegateRole::Oracle,
        },
        HookCase {
            hook: HYPERLANE_SUBMIT_CHECKPOINT_HOOK,
            target: HYPERLANE_CONTROLLER_ADDRESS,
            selector: IHyperlaneController::submitCheckpointCall::SELECTOR,
            valid_input: crate::test_support::checkpoint_calldata().to_vec(),
            min_max_fee_per_gas: crate::hyperlane::MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
            max_calldata_bytes: crate::hyperlane::MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES,
            max_gas_limit: crate::hyperlane::MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT,
            malformed_reason: "zero-fee oracle vote calldata is malformed: \
                submitCheckpoint(uint32,bytes32,uint32,bytes32,bytes) decode failed",
            delegate_role: ValidatorDelegateRole::Hyperlane,
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
                ZeroFeePolicyError::MalformedCalldata(case.malformed_reason),
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
            let other_role = match case.delegate_role {
                ValidatorDelegateRole::Oracle => ValidatorDelegateRole::Hyperlane,
                ValidatorDelegateRole::Hyperlane => ValidatorDelegateRole::Oracle,
            };
            outbe_validatorset::contract::ValidatorSet::new(storage.clone())
                .set_delegate(VALIDATOR, other_role, STRANGER)
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
                waive_fee(storage.clone(), &stranger),
                Err(ZeroFeePolicyError::UnauthorizedSigner),
                "{:?}",
                case.hook
            );
            let validator = ZeroFeeTransaction {
                signer: VALIDATOR,
                ..delegated
            };
            assert_eq!(
                waive_fee(storage, &validator),
                Err(ZeroFeePolicyError::UnauthorizedSigner),
                "explicit {:?} delegation disables validator-address fallback",
                case.delegate_role
            );
        });
    }
}

#[test]
fn oracle_vote_waiver_ends_when_the_validator_has_voted() {
    let cases = hook_cases();
    let case = &cases[0];
    assert_eq!(case.hook, ORACLE_SUBMIT_VOTE_HOOK);
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
        let err = waive_fee(storage, &tx).unwrap_err();
        assert_eq!(
            err,
            ZeroFeePolicyError::HookConflict("zero-fee oracle vote already exists for validator")
        );
        assert_eq!(
            err.to_string(),
            "zero-fee oracle vote already exists for validator"
        );
    });
}

mod equivocation {
    use super::*;
    use alloy_primitives::{keccak256, Bytes};
    use outbe_primitives::equivocation_evidence::{
        canonical_equivocation_evidence_hash, EquivocationEvidenceKind,
    };
    use outbe_primitives::error::PrecompileError;
    use outbe_slashindicator::{precompile::ISlashIndicator, schema::SlashIndicator};
    use outbe_system_common::addresses::SLASH_INDICATOR_ADDRESS;
    use outbe_validatorset::contract::ValidatorSet;

    const GAS: u64 = 1_500_000;
    const LIMIT: usize = 16_384;

    // Wire admission is stateless: signatures are intentionally invalid here.
    fn block(epoch: u8, digest: u8) -> Vec<u8> {
        [vec![1; 144], vec![epoch, 5, 4], vec![digest; 32]].concat()
    }

    fn calldata(selector: usize, first: &[u8], second: &[u8]) -> Vec<u8> {
        let block1 = Bytes::copy_from_slice(first);
        let block2 = Bytes::copy_from_slice(second);
        match selector {
            0 => ISlashIndicator::submitConflictingNotarizeEvidenceCall { block1, block2 }
                .abi_encode(),
            1 => ISlashIndicator::submitConflictingFinalizeEvidenceCall { block1, block2 }
                .abi_encode(),
            _ => ISlashIndicator::submitNullifyFinalizeEvidenceCall {
                nullifyBlock: block1,
                finalizeBlock: block2,
            }
            .abi_encode(),
        }
    }

    fn tx(input: &[u8]) -> ZeroFeeTransaction<'_> {
        ZeroFeeTransaction {
            signer: SIGNER,
            call: TransactionCallFields {
                to: Some(SLASH_INDICATOR_ADDRESS),
                value: U256::ZERO,
                input,
                gas_limit: GAS,
                max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE.into(),
                max_priority_fee_per_gas: Some(0),
            },
        }
    }

    #[test]
    fn slash_selectors_preserve_submitter_and_hook_limits() {
        for selector in 0..3 {
            let input = calldata(selector, &block(1, 1), &block(1, 2));
            let accepted = tx(&input);
            let candidate = crate::registry().classify(&accepted).unwrap().unwrap();
            assert_eq!(candidate.signer, SIGNER);
            assert_eq!(candidate.hook.name(), "SlashEquivocation");
            assert_eq!(candidate.max_soft_failures_per_block(), Some(4));
            assert_eq!(candidate.hook.priority_class(), Some(1));
            let mut other = accepted;
            other.call.to = Some(Address::ZERO);
            assert_eq!(crate::registry().classify(&other), Ok(None));
            other.call.to = None;
            assert_eq!(crate::registry().classify(&other), Ok(None));
        }
    }

    #[test]
    fn slash_rejects_value_gas_size_and_noncanonical_abi() {
        for selector in 0..3 {
            let input = calldata(selector, &block(1, 1), &block(1, 2));
            let mut funded = tx(&input);
            funded.call.value = U256::from(1);
            assert_eq!(
                crate::registry().classify(&funded),
                Err(ZeroFeePolicyError::NonZeroValue)
            );
            let mut greedy = tx(&input);
            greedy.call.gas_limit += 1;
            assert_eq!(
                crate::registry().classify(&greedy),
                Err(ZeroFeePolicyError::GasLimitTooHigh {
                    gas_limit: GAS + 1,
                    limit: GAS,
                })
            );
            let mut oversized = input[..4].to_vec();
            oversized.resize(LIMIT + 1, 0);
            assert_eq!(
                crate::registry().classify(&tx(&oversized)),
                Err(ZeroFeePolicyError::CalldataTooLarge {
                    size: LIMIT + 1,
                    limit: LIMIT
                })
            );
            let mut trailing = input.clone();
            trailing.push(0);
            // A valid ABI value with a noncanonical gap between head and tails.
            let mut gapped = input.clone();
            for offset in [4 + 31, 4 + 63] {
                gapped[offset] += 32;
            }
            gapped.splice(68..68, [0; 32]);
            for malformed in [&input[..4], &trailing, &gapped] {
                assert!(matches!(
                    crate::registry().classify(&tx(malformed)),
                    Err(ZeroFeePolicyError::MalformedCalldata(_))
                ));
            }
        }
    }

    #[test]
    fn slash_wire_rejects_different_keys_and_epochs() {
        for selector in 0..3 {
            let first = block(1, 1);
            let mut other_key = block(1, 2);
            other_key[0] = 2;
            for second in [other_key, block(2, 2)] {
                let input = calldata(selector, &first, &second);
                assert!(matches!(
                    crate::registry().classify(&tx(&input)),
                    Err(ZeroFeePolicyError::MalformedCalldata(_))
                ));
            }
        }
    }

    #[test]
    fn slash_acl_replay_epoch_and_lifecycle_fail_before_committee_reads() {
        let first = block(99, 1);
        let second = block(99, 2);
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            let input = calldata(0, &first, &second);
            let transaction = tx(&input);
            assert_eq!(
                waive_fee(storage.clone(), &transaction).unwrap_err().code(),
                107
            );
            let owner = Address::repeat_byte(0x99);
            let mut vs = ValidatorSet::new(storage.clone());
            vs.test_configure_registry(owner).unwrap();
            vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
            vs.test_register_active_validator(SIGNER, &[2; 48], U256::from(1))
                .unwrap();
            vs.test_register_active_validator(VALIDATOR, &[1; 48], U256::from(100))
                .unwrap();
            let si = SlashIndicator::new(storage.clone());
            let hash = canonical_equivocation_evidence_hash(&first, &second);
            si.evidence_processed.write(&hash, true).unwrap();
            assert_eq!(
                waive_fee(storage.clone(), &transaction).unwrap_err(),
                ZeroFeePolicyError::HookConflict("equivocation evidence already processed")
            );
            si.evidence_processed.write(&hash, false).unwrap();
            let key = keccak256(
                [
                    b"outbe:equivocation-epoch-felony:v1".as_slice(),
                    VALIDATOR.as_slice(),
                    &99u64.to_be_bytes(),
                ]
                .concat(),
            );
            si.evidence_processed.write(&key, true).unwrap();
            let distinct = calldata(1, &first, &block(99, 3));
            assert_eq!(
                waive_fee(storage.clone(), &tx(&distinct)).unwrap_err(),
                ZeroFeePolicyError::HookConflict(
                    "validator already penalized for equivocation epoch"
                )
            );
            assert!(!si.is_evidence_processed(hash).unwrap());
            si.evidence_processed.write(&key, false).unwrap();
            // A registered, never-activated accused must be rejected before snapshots/BLS.
            let inactive = Address::repeat_byte(0x66);
            vs.test_register_validator_without_pop(inactive, &[3; 48])
                .unwrap();
            let mut a = first.clone();
            let mut b = second.clone();
            a[..48].fill(3);
            b[..48].fill(3);
            let input = calldata(2, &a, &b);
            assert!(matches!(
                outbe_slashindicator::preflight_equivocation_evidence(
                    storage.clone(),
                    SIGNER,
                    EquivocationEvidenceKind::NullifyFinalize,
                    &a,
                    &b,
                ),
                Err(outbe_slashindicator::EquivocationPreflightError::Rejected(
                    outbe_slashindicator::errors::SlashIndicatorError::ValidatorNotSlashable { .. }
                ))
            ));
            assert_eq!(
                waive_fee(storage.clone(), &tx(&input)).unwrap_err(),
                ZeroFeePolicyError::HookConflict("equivocation evidence rejected")
            );
            assert!(!si
                .is_evidence_processed(canonical_equivocation_evidence_hash(&a, &b))
                .unwrap());
            assert_eq!(si.get_felony_count(VALIDATOR).unwrap(), 0);
        });
    }

    #[test]
    fn slash_registry_reports_provider_failure_as_109() {
        let input = calldata(0, &block(1, 1), &block(1, 2));
        let mut provider = HashMapStorageProvider::new(1);
        provider.enable_production_storage_gas_metering();
        provider.set_gas_limit(0);
        provider.enter(|storage| {
            let error = waive_fee(storage, &tx(&input)).unwrap_err();
            assert_eq!(error.code(), 109);
            assert!(matches!(error, ZeroFeePolicyError::Storage(_)));
        });
    }

    #[test]
    fn slash_provider_mapping_preserves_error_sources_and_code_109() {
        use std::error::Error;
        for error in [
            PrecompileError::Storage("storage sentinel".into()),
            PrecompileError::Fatal("fatal sentinel".into()),
            PrecompileError::FatalWithCause(Box::new(std::io::Error::other("typed sentinel"))),
        ] {
            let error = crate::equivocation::map_preflight_error(
                outbe_slashindicator::EquivocationPreflightError::Precompile(error),
            );
            assert_eq!(error.code(), 109);
            assert!(error.source().is_some());
            let ZeroFeePolicyError::Storage(source) = error else {
                panic!("expected retained storage cause");
            };
            assert!(source.source().is_some());
            let source = source.0.as_ref();
            match source {
                PrecompileError::Storage(message) => assert_eq!(message, "storage sentinel"),
                PrecompileError::Fatal(message) => assert_eq!(message, "fatal sentinel"),
                PrecompileError::FatalWithCause(cause) => assert!(cause.is::<std::io::Error>()),
                other => panic!("lost provider cause: {other:?}"),
            }
        }
    }

    #[test]
    fn slash_valid_bls_authorizes_submitter_without_writes_or_cached_grants() {
        use outbe_validatorset::state::{CommitteeEntry, CommitteeSnapshot};
        // Frozen signer-side vectors: test_signing keypair(77), committee seeds 1..=4,
        // epoch 1/view 5, proposal parent 4 and digest 1/2, Simplex POP namespaces.
        let accused_key = alloy_primitives::hex!(
            "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
            "9121da910ffdb501b8908de3"
        );
        let mut keys = [
            alloy_primitives::hex!(
                "b82eaa7fbc7f9cf9d60826e5155ca8ccc46e13d87f64f7bcdcaa2972c370766b87635334"
                "bfc49b8fba7fb784e763d44e"
            ),
            alloy_primitives::hex!(
                "b806a3638975b2c33ccdda9cc55231e1328a0d1f70a3dde804e473e22ec21d5703ee3426"
                "4c702b971cd0f9d5892434e8"
            ),
            alloy_primitives::hex!(
                "83c50f2fa828c5e132a2ac00792ef9c892372976ff26400711b7061bdb045f0751559b4a"
                "04cfd7399555a2aed5b54f3f"
            ),
            alloy_primitives::hex!(
                "95498b0f2d5fc3c804c2d64ddf3c50959fb229649841f904f85c4329de6d2a1d201e25c3"
                "63eafe5b7cfc334a50d29ed9"
            ),
        ];
        keys.sort();
        let snapshot = CommitteeSnapshot {
            committee: keys
                .into_iter()
                .enumerate()
                .map(|(i, consensus_pubkey)| CommitteeEntry {
                    address: Address::with_last_byte((i + 1) as u8),
                    consensus_pubkey,
                })
                .collect(),
            vrf_material_version: 1,
            vrf_group_public_key_bytes: vec![0x11; 96],
            vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
        };
        let pairs = [
            (
                0,
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de38df753f7fd4f1cc72b6dc20ceb2a157e60e1aa0c65f6a896"
                    "976e342d84e0a0c7655ce8de928c8c3bce9dfc3889ecfb8e186226f0d936d422b05e2168"
                    "6263db30fbf3c0fc50206cc553b9c60965bf308d7ec2f9f9fbfb27367d313c05fecc5960"
                    "0105040101010101010101010101010101010101010101010101010101010101010101"
                )
                .to_vec(),
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de3abe4a4bda1e9c0daf1774d972e9b2ff9c79be70b51ec54ae"
                    "0aae200b4c87b5ad7064526a8a95cd20251c7051a6a1ece319dfd2ee0c1bd54ab771ebc5"
                    "89dde1722ecca132dc7eb077d60e1ee07223a9f5ab93cc7bf7511e2ff13921e19d8f95d5"
                    "0105040202020202020202020202020202020202020202020202020202020202020202"
                )
                .to_vec(),
            ),
            (
                1,
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de398aef65eae98ee87238b9ea9675f43f454b3e20f13dc4093"
                    "bcc9f51dbe4cc997cbd9c3a56bde0681196c97a50a87dabd0edb2671d13cea0d8ab1f6ef"
                    "31921453e78d5533e8eeea77f132e20a3176349f110089f39ef3fd4539875297a479cae6"
                    "0105040101010101010101010101010101010101010101010101010101010101010101"
                )
                .to_vec(),
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de388a7c1578093f7b419b9cb43a7da4de775c337b1bf1b5f76"
                    "88451147aba8f1fbcb0110a1e2fdecef3fc1428b8d6ab1660f4157de2da941e43b1c5ab9"
                    "0aaba08731e459c63422ab18cd728fe9642d8ab686301e838c456dc2fe985ad9d3bd1229"
                    "0105040202020202020202020202020202020202020202020202020202020202020202"
                )
                .to_vec(),
            ),
            (
                2,
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de3b03b52265f9c18cb5aab055c2a0f3a626285dcade52090f7"
                    "671e59146c0040e50aba46752229a010389063369966766508fc0553888a2237c7508aad"
                    "21dcae965b3a12e0bb0d9ca657f071cc256dd8699b9acbfb8a92dda7177a71ca71c21eaa"
                    "0105"
                )
                .to_vec(),
                alloy_primitives::hex!(
                    "991267617a6538502ac9ef5837e1f857cd64bf7f9aa19afa3080473925d5d4017d36e20e"
                    "9121da910ffdb501b8908de388a7c1578093f7b419b9cb43a7da4de775c337b1bf1b5f76"
                    "88451147aba8f1fbcb0110a1e2fdecef3fc1428b8d6ab1660f4157de2da941e43b1c5ab9"
                    "0aaba08731e459c63422ab18cd728fe9642d8ab686301e838c456dc2fe985ad9d3bd1229"
                    "0105040202020202020202020202020202020202020202020202020202020202020202"
                )
                .to_vec(),
            ),
        ];
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            let owner = Address::repeat_byte(0x99);
            let mut vs = ValidatorSet::new(storage.clone());
            vs.test_configure_registry(owner).unwrap();
            vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
            vs.test_register_active_validator(SIGNER, &[2; 48], U256::from(1))
                .unwrap();
            vs.test_register_active_validator(VALIDATOR, &accused_key, U256::from(100))
                .unwrap();
            for member in &snapshot.committee {
                vs.test_register_active_validator(
                    member.address,
                    &member.consensus_pubkey,
                    U256::from(1),
                )
                .unwrap();
            }
            vs.test_seed_committee_ring(owner, &snapshot).unwrap();
            let si = SlashIndicator::new(storage.clone());
            for (selector, first, second) in pairs {
                let input = calldata(selector, &first, &second);
                let transaction = tx(&input);
                let candidate = crate::registry().classify(&transaction).unwrap().unwrap();
                let hash = canonical_equivocation_evidence_hash(&first, &second);
                assert_eq!(
                    waive_fee(storage.clone(), &transaction),
                    Ok(Some(ZeroFeeAuthorization {
                        hook: candidate.hook,
                        subject: SIGNER,
                    }))
                );
                assert!(!si.is_evidence_processed(hash).unwrap());
                assert_eq!(si.get_felony_count(VALIDATOR).unwrap(), 0);
                // Fresh state, not an earlier successful authorization, determines the result.
                si.evidence_processed.write(&hash, true).unwrap();
                assert_eq!(
                    waive_fee(storage.clone(), &transaction).unwrap_err().code(),
                    108
                );
                si.evidence_processed.write(&hash, false).unwrap();
                let mut bad_signature = first.clone();
                bad_signature[48..144].fill(0);
                let input = calldata(selector, &bad_signature, &second);
                assert_eq!(
                    waive_fee(storage.clone(), &tx(&input)).unwrap_err().code(),
                    108
                );
            }
        });
    }
}
