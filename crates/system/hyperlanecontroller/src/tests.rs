//! Unit tests for the Hyperlane controller precompile.
//!
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs: `stub_sub_call_at_selector` pins a target's reply per
//! selector and `enable_sub_call_stub()` makes every other sub-call succeed
//! with empty returndata.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::errors::HyperlaneControllerError;
use crate::precompile::{dispatch, IHyperlaneController};
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use crate::RemoteCall;

const CHAIN_ID: u64 = 54_322_345;
const LOCAL: u32 = CHAIN_ID as u32;
const SEPOLIA: u32 = 11_155_111;
const BSC: u32 = 97;

fn deployer() -> Address {
    address!("0x00000000000000000000000000000000000000d1")
}
fn stranger() -> Address {
    address!("0x00000000000000000000000000000000000000b0")
}
fn router() -> Address {
    address!("0x0000000000000000000000000000000000000626")
}
fn local_ism() -> Address {
    address!("0x0000000000000000000000000000000000000151")
}
fn sepolia_ism() -> Address {
    address!("0x0000000000000000000000000000000000000152")
}
fn bsc_ism() -> Address {
    address!("0x0000000000000000000000000000000000000153")
}
fn v(n: u8) -> Address {
    Address::repeat_byte(n)
}

fn word_addr(value: Address) -> Bytes {
    Bytes::from(value.abi_encode())
}

fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(CHAIN_ID)
}

/// Deployer still owns the local ISM, controller is its pending owner and
/// already owns the router: the state right after `mise run owner:transfer`.
fn stub_pre_initialize(p: &mut HashMapStorageProvider) {
    p.stub_sub_call_at_selector(
        local_ism(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(deployer()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::pendingOwnerCall::SELECTOR,
        word_addr(HYPERLANE_CONTROLLER_ADDRESS),
    );
    p.stub_sub_call_at_selector(
        router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(HYPERLANE_CONTROLLER_ADDRESS),
    );
    p.enable_sub_call_stub();
}

fn hook(n: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[18] = 0x0a;
    bytes[19] = n;
    Address::from(bytes)
}

fn initialize_call() -> Bytes {
    IHyperlaneController::initializeCall {
        router: router(),
        domains: vec![LOCAL, SEPOLIA, BSC],
        isms: vec![local_ism(), sepolia_ism(), bsc_ism()],
        hooks: vec![hook(1), hook(2), hook(3)],
    }
    .abi_encode()
    .into()
}

fn initialized_provider() -> HashMapStorageProvider {
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap();
    });
    p
}

fn stub_router_and_ism(
    p: &mut HashMapStorageProvider,
    fee: U256,
    validators: &[Address],
    threshold: u8,
) {
    p.stub_sub_call_at_selector(
        router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(fee.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::validatorsAndThresholdCall::SELECTOR,
        Bytes::from(
            IStorageMultisigIsm::validatorsAndThresholdCall::abi_encode_returns(
                &IStorageMultisigIsm::validatorsAndThresholdReturn {
                    _0: validators.to_vec(),
                    _1: threshold,
                },
            ),
        ),
    );
}

fn topics(p: &HashMapStorageProvider) -> Vec<B256> {
    p.get_events(HYPERLANE_CONTROLLER_ADDRESS)
        .iter()
        .map(|log| log.topics()[0])
        .collect()
}

fn revert_reason(err: outbe_primitives::error::PrecompileError) -> String {
    match err {
        outbe_primitives::error::PrecompileError::Revert(reason) => reason,
        other => panic!("expected revert, got {other:?}"),
    }
}

#[test]
fn initialize_accepts_ism_ownership_and_stores_table() {
    let p = initialized_provider();
    let mut p = p;
    StorageHandle::enter(&mut p, |storage| {
        let c = HyperlaneControllerContract::new(storage);
        assert_eq!(c.router.read().unwrap(), router());
        assert_eq!(c.ism_by_domain.read(&LOCAL).unwrap(), local_ism());
        assert_eq!(c.ism_by_domain.read(&SEPOLIA).unwrap(), sepolia_ism());
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, SEPOLIA, BSC]);
    });
    let t = topics(&p);
    assert_eq!(
        t.iter()
            .filter(|h| **h == IHyperlaneController::DomainAdded::SIGNATURE_HASH)
            .count(),
        3
    );
    assert_eq!(
        *t.last().unwrap(),
        IHyperlaneController::Initialized::SIGNATURE_HASH
    );
}

#[test]
fn initialize_is_gated_and_one_shot() {
    // Wrong caller.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), stranger(), U256::ZERO).unwrap_err();
        assert!(revert_reason(err).contains("is not the current owner"));
    });

    // Pending owner is not the controller.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::pendingOwnerCall::SELECTOR,
        word_addr(stranger()),
    );
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert!(revert_reason(err).contains("pending owner"));
    });

    // Router not yet transferred.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    p.stub_sub_call_at_selector(
        router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(deployer()),
    );
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert!(revert_reason(err).contains("expected the controller"));
    });

    // Table without the local domain.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        let call: Bytes = IHyperlaneController::initializeCall {
            router: router(),
            domains: vec![SEPOLIA],
            isms: vec![sepolia_ism()],
            hooks: vec![hook(2)],
        }
        .abi_encode()
        .into();
        let err = dispatch(storage, &call, deployer(), U256::ZERO).unwrap_err();
        assert!(revert_reason(err).contains("must be included"));
    });

    // Second call.
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert!(revert_reason(err).contains("already initialized"));
    });
}

#[test]
fn fund_is_the_only_payable_selector() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = IHyperlaneController::fundCall {}.abi_encode().into();
        dispatch(storage.clone(), &fund, stranger(), U256::from(5)).unwrap();
        assert!(dispatch(storage.clone(), &fund, stranger(), U256::ZERO).is_err());
        let view: Bytes = IHyperlaneController::routerCall {}.abi_encode().into();
        assert!(dispatch(storage, &view, stranger(), U256::from(1)).is_err());
    });
    assert_eq!(
        *topics(&p).last().unwrap(),
        IHyperlaneController::Funded::SIGNATURE_HASH
    );
}

#[test]
fn rotation_dispatches_every_remote_ism_then_the_local_one() {
    let mut p = initialized_provider();
    let fee = U256::from(7);
    stub_router_and_ism(&mut p, fee, &[v(1), v(2)], 1);
    p.set_balance(HYPERLANE_CONTROLLER_ADDRESS, fee * U256::from(2));
    p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        c.set_validators_and_threshold(&[v(1), v(2), v(3)], 2)
            .unwrap();
    });
    let t = topics(&p);
    assert_eq!(
        t,
        vec![
            IHyperlaneController::RemoteCallDispatched::SIGNATURE_HASH,
            IHyperlaneController::RemoteCallDispatched::SIGNATURE_HASH,
            IHyperlaneController::ValidatorsAndThresholdApplied::SIGNATURE_HASH,
        ]
    );
    let dispatched = IHyperlaneController::RemoteCallDispatched::decode_log_data(
        &p.get_events(HYPERLANE_CONTROLLER_ADDRESS)[0],
    )
    .unwrap();
    assert_eq!(dispatched.domain, SEPOLIA);
    assert_eq!(dispatched.fee, fee);
}

#[test]
fn rotation_without_fee_balance_changes_nothing() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::from(7), &[v(1)], 1);
    p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        let err = c
            .set_validators_and_threshold(&[v(1), v(2)], 2)
            .unwrap_err();
        assert!(revert_reason(err).contains("is below the required"));
    });
    assert!(topics(&p).is_empty());
}

#[test]
fn add_remove_and_threshold_derive_from_the_local_ism() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::ZERO, &[v(1), v(2)], 2);
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        c.add_validator(v(3), None).unwrap();
        c.add_validator(v(4), Some(3)).unwrap();
        c.remove_validator(v(2), Some(1)).unwrap();
        c.set_threshold(2).unwrap();

        // Already present / absent.
        assert!(c.add_validator(v(1), None).is_err());
        assert!(c.remove_validator(v(9), None).is_err());
        // Keeping threshold 2 after removing down to one validator is invalid.
        assert!(c.remove_validator(v(1), None).is_err());
    });
}

#[test]
fn remote_and_local_generic_calls() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::ZERO, &[v(1)], 1);
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        let calls = [RemoteCall {
            to: sepolia_ism(),
            value: U256::ZERO,
            data: IStorageMultisigIsm::acceptOwnershipCall {}
                .abi_encode()
                .into(),
        }];
        assert_eq!(
            c.call_remote(SEPOLIA, &calls).unwrap(),
            B256::repeat_byte(0xaa)
        );
        assert!(c.call_remote(LOCAL, &calls).is_err());
        assert!(c.call_remote(SEPOLIA, &[]).is_err());

        c.call_local(local_ism(), U256::ZERO, Bytes::from_static(&[1, 2, 3, 4]))
            .unwrap();
        assert!(c
            .call_local(local_ism(), U256::from(1), Bytes::new())
            .is_err());
    });
}

#[test]
fn domains_can_be_added_and_removed_except_the_local_one() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        c.add_domain(1, v(5), hook(5)).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, SEPOLIA, BSC, 1]);
        c.add_domain(1, v(6), hook(6)).unwrap();
        assert_eq!(c.ism_by_domain.read(&1).unwrap(), v(6));
        assert_eq!(c.domains.read_all().unwrap().len(), 4);

        c.remove_domain(SEPOLIA).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, BSC, 1]);
        assert_eq!(c.ism_by_domain.read(&SEPOLIA).unwrap(), Address::ZERO);

        assert!(c.remove_domain(SEPOLIA).is_err());
        assert!(c.remove_domain(LOCAL).is_err());
        assert!(c.add_domain(LOCAL, v(7), hook(7)).is_err());
        assert!(c.add_domain(0, v(7), hook(7)).is_err());
    });
}

#[test]
fn operations_require_initialization() {
    let mut p = provider();
    p.enable_sub_call_stub();
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        assert!(matches!(
            c.set_validators_and_threshold(&[v(1)], 1),
            Err(outbe_primitives::error::PrecompileError::Revert(_))
        ));
        assert!(c
            .call_remote(
                SEPOLIA,
                &[RemoteCall {
                    to: v(1),
                    value: U256::ZERO,
                    data: Bytes::new()
                }]
            )
            .is_err());
        assert!(c.add_domain(SEPOLIA, v(1), hook(1)).is_err());
    });
}

#[test]
fn validator_shape_checks() {
    assert!(validate_validators(&[v(1), v(2)], 2).is_ok());
    assert!(matches!(
        validate_validators(&[], 1),
        Err(HyperlaneControllerError::InvalidValidatorCount { .. })
    ));
    assert!(matches!(
        validate_validators(&[v(1)], 0),
        Err(HyperlaneControllerError::InvalidThreshold { .. })
    ));
    assert!(matches!(
        validate_validators(&[v(1)], 2),
        Err(HyperlaneControllerError::InvalidThreshold { .. })
    ));
    assert!(matches!(
        validate_validators(&[v(1), v(1)], 1),
        Err(HyperlaneControllerError::InvalidValidator { .. })
    ));
    assert!(matches!(
        validate_validators(&[Address::ZERO], 1),
        Err(HyperlaneControllerError::InvalidValidator { .. })
    ));
    let many: Vec<Address> = (0..=255u16)
        .map(|i| Address::from_word(B256::from(U256::from(i + 1))))
        .collect();
    assert!(matches!(
        validate_validators(&many, 1),
        Err(HyperlaneControllerError::InvalidValidatorCount { .. })
    ));
}

mod sync {
    use super::*;
    use outbe_validatorset::contract::ValidatorSet;

    const VALIDATOR_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

    fn activate(storage: StorageHandle<'_>, addr: Address, seed: u8) {
        let mut vs = ValidatorSet::new(storage);
        if vs.config_owner.read().unwrap().is_zero() {
            vs.config_owner.write(VALIDATOR_OWNER).unwrap();
            vs.set_config_max_validators(100).unwrap();
        }
        let mut pubkey = [0u8; 48];
        pubkey[0] = seed;
        vs.register_validator(VALIDATOR_OWNER, addr, &pubkey)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(addr).unwrap();
    }

    #[test]
    fn threshold_is_two_thirds_rounded_up() {
        assert_eq!(crate::consensus_threshold(1).unwrap(), 1);
        assert_eq!(crate::consensus_threshold(2).unwrap(), 2);
        assert_eq!(crate::consensus_threshold(3).unwrap(), 2);
        assert_eq!(crate::consensus_threshold(4).unwrap(), 3);
        assert_eq!(crate::consensus_threshold(255).unwrap(), 170);
        assert!(crate::consensus_threshold(0).is_err());
        assert!(crate::consensus_threshold(256).is_err());
    }

    #[test]
    fn sync_mirrors_the_active_set_and_is_idempotent() {
        let mut p = initialized_provider();
        // ISM currently holds a stale set {v1} / 1.
        stub_router_and_ism(&mut p, U256::ZERO, &[v(1)], 1);
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(1), v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 1);
            }
        });
        p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
        StorageHandle::enter(&mut p, |storage| {
            let call: Bytes = IHyperlaneController::syncCall {}.abi_encode().into();
            let ret = dispatch(storage, &call, stranger(), U256::ZERO).unwrap();
            assert!(IHyperlaneController::syncCall::abi_decode_returns(&ret).unwrap());
        });
        let t = topics(&p);
        assert_eq!(
            *t.last().unwrap(),
            IHyperlaneController::ValidatorsAndThresholdApplied::SIGNATURE_HASH
        );
        let applied = IHyperlaneController::ValidatorsAndThresholdApplied::decode_log_data(
            p.get_events(HYPERLANE_CONTROLLER_ADDRESS).last().unwrap(),
        )
        .unwrap();
        assert_eq!(applied.threshold, 3);
        assert_eq!(applied.validatorCount, U256::from(4));

        // ISM now reports the mirrored set (any order): nothing to do.
        stub_router_and_ism(&mut p, U256::ZERO, &[v(4), v(3), v(2), v(1)], 3);
        p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(!c.sync().unwrap());
        });
        assert!(topics(&p).is_empty());
    }
}

mod liveness {
    use super::*;
    use crate::runtime::{checkpoint_digest, GRACE_BLOCKS, MAX_MISSES};
    use crate::schema::validator_domain_key;
    use alloy_primitives::{b256, hex};
    use outbe_primitives::tee_signatures::recover_signer;
    use outbe_validatorset::contract::ValidatorSet;

    /// Real checkpoints signed on outbetestnet by validator 0x4fe927… for the
    /// (since redeployed) MerkleTreeHook 0x6543cef9…, taken from its S3 bucket.
    const FIXTURE_HOOK: Address = address!("0x6543cef9bbe42d66b5b36ccaf9374de2b55f9cbc");
    const FIXTURE_VALIDATOR: Address = address!("0x4fe927ab711793954b3a29969ecd4a60d6d265d0");
    const VALIDATOR_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

    struct Checkpoint {
        root: B256,
        index: u32,
        message_id: B256,
        signature: [u8; 65],
    }

    fn signature(hex_str: &str) -> [u8; 65] {
        hex::decode(hex_str).unwrap().try_into().unwrap()
    }

    fn checkpoint_0() -> Checkpoint {
        Checkpoint {
            root: b256!("0xb8f3591fc1f80eaecba71902c5a172bb89783f96eb34dacf4fec14ef5d8e4765"),
            index: 0,
            message_id: b256!("0x6cf30153c1583380a6bab73348f2f33f7aa36a249f3ffe3868d4d0a8694c8c1f"),
            signature: signature("1d728977b83ac16088ea0db249f9c4eac6b58818b42676964b238ac37228e48520858022b5e514ea711b3b2bd55be4381efeb8d7bb907c75525bd760decad2781b"),
        }
    }

    fn checkpoint_2() -> Checkpoint {
        Checkpoint {
            root: b256!("0x5cd1ceeff6f033960677157f13b472fcfb1617638dbdfb0ab0cde36cd94bf050"),
            index: 2,
            message_id: b256!("0x3eae2775d2a39169fd82693c9883351a72761c9cdf4cddedaf95bf62e454250e"),
            signature: signature("82079df9c6de4d2ab039216e3275cf803d245819150512e50dde105e2802f98976b8041d134256ef7df29fa8a8613a5c042af46c3468ab011410e19e6e1cb4931c"),
        }
    }

    fn submit_call(domain: u32, checkpoint: &Checkpoint, index: u32) -> Bytes {
        IHyperlaneController::submitCheckpointCall {
            domain,
            root: checkpoint.root,
            index,
            messageId: checkpoint.message_id,
            signature: Bytes::copy_from_slice(&checkpoint.signature),
        }
        .abi_encode()
        .into()
    }

    fn activate(storage: StorageHandle<'_>, addr: Address, seed: u8) {
        let mut vs = ValidatorSet::new(storage);
        if vs.config_owner.read().unwrap().is_zero() {
            vs.config_owner.write(VALIDATOR_OWNER).unwrap();
            vs.set_config_max_validators(100).unwrap();
        }
        let mut pubkey = [0u8; 48];
        pubkey[0] = seed;
        vs.register_validator(VALIDATOR_OWNER, addr, &pubkey)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(addr).unwrap();
    }

    /// Initialized controller with the fixture hook on the local domain and
    /// the fixture validator active.
    fn liveness_provider() -> HashMapStorageProvider {
        let mut p = provider();
        stub_pre_initialize(&mut p);
        StorageHandle::enter(&mut p, |storage| {
            let call: Bytes = IHyperlaneController::initializeCall {
                router: router(),
                domains: vec![LOCAL, SEPOLIA],
                isms: vec![local_ism(), sepolia_ism()],
                hooks: vec![FIXTURE_HOOK, hook(2)],
            }
            .abi_encode()
            .into();
            dispatch(storage.clone(), &call, deployer(), U256::ZERO).unwrap();
            activate(storage, FIXTURE_VALIDATOR, 1);
        });
        p
    }

    fn write_submission(
        c: &HyperlaneControllerContract<'_>,
        validator: Address,
        domain: u32,
        index: u32,
        block: u64,
    ) {
        let key = validator_domain_key(validator, domain);
        c.submitted_index.write(&key, index).unwrap();
        c.submitted_block.write(&key, block).unwrap();
    }

    #[test]
    fn digest_recovers_the_agent_key_from_real_checkpoints() {
        for checkpoint in [checkpoint_0(), checkpoint_2()] {
            let digest = checkpoint_digest(
                LOCAL,
                FIXTURE_HOOK,
                checkpoint.root,
                checkpoint.index,
                checkpoint.message_id,
            );
            assert_eq!(
                recover_signer(&digest, &checkpoint.signature).unwrap(),
                FIXTURE_VALIDATOR
            );
        }
    }

    #[test]
    fn submit_accepts_own_signature_and_rejects_the_rest() {
        let mut p = liveness_provider();
        p.set_block_number(100);
        p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
        StorageHandle::enter(&mut p, |storage| {
            let two = checkpoint_2();
            dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap();
            let view: Bytes = IHyperlaneController::submittedIndexCall {
                validator: FIXTURE_VALIDATOR,
                domain: LOCAL,
            }
            .abi_encode()
            .into();
            let ret = dispatch(storage.clone(), &view, stranger(), U256::ZERO).unwrap();
            assert_eq!(
                IHyperlaneController::submittedIndexCall::abi_decode_returns(&ret).unwrap(),
                2
            );

            let not_validator = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 2),
                stranger(),
                U256::ZERO,
            )
            .unwrap_err();
            assert!(revert_reason(not_validator).contains("not an active validator"));

            let no_hook = dispatch(
                storage.clone(),
                &submit_call(BSC, &two, 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert!(revert_reason(no_hook).contains("no MerkleTreeHook"));

            let tampered = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 3),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert!(revert_reason(tampered).contains("does not match"));

            let older = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &checkpoint_0(), 0),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert!(revert_reason(older).contains("is not newer"));

            // Re-submitting the same index is a harmless no-op.
            dispatch(
                storage,
                &submit_call(LOCAL, &two, 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap();
        });
        let submitted = topics(&p)
            .iter()
            .filter(|h| **h == IHyperlaneController::CheckpointSubmitted::SIGNATURE_HASH)
            .count();
        assert_eq!(submitted, 2);
    }

    #[test]
    fn registered_signer_replaces_the_validator_address() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            // A stranger key cannot register a signer, the validator can.
            let set: Bytes = IHyperlaneController::setHyperlaneSignerCall { signer: v(9) }
                .abi_encode()
                .into();
            assert!(dispatch(storage.clone(), &set, stranger(), U256::ZERO).is_err());
            dispatch(storage.clone(), &set, FIXTURE_VALIDATOR, U256::ZERO).unwrap();
            // The fixture is signed by the validator key, not v(9): rejected now.
            let err = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &checkpoint_2(), 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert!(revert_reason(err).contains("does not match"));
            // Reset to the validator address.
            let reset: Bytes = IHyperlaneController::setHyperlaneSignerCall {
                signer: Address::ZERO,
            }
            .abi_encode()
            .into();
            dispatch(storage.clone(), &reset, FIXTURE_VALIDATOR, U256::ZERO).unwrap();
            dispatch(
                storage,
                &submit_call(LOCAL, &checkpoint_2(), 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap();
        });
    }

    /// Four validators: the reference is the third-highest settled index, so
    /// one inflated submission cannot move it and fresh ones are ignored.
    #[test]
    fn reference_index_is_quorum_based_and_grace_gated() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2);
            }
            let c = HyperlaneControllerContract::new(storage);
            let old = 1_000 - GRACE_BLOCKS;
            write_submission(&c, FIXTURE_VALIDATOR, LOCAL, 500, old);
            write_submission(&c, v(2), LOCAL, 498, old);
            write_submission(&c, v(3), LOCAL, 120, old);
            write_submission(&c, v(4), LOCAL, 999_999, old);
            for addr in [FIXTURE_VALIDATOR, v(2), v(3), v(4)] {
                write_submission(&c, addr, SEPOLIA, 10, old);
            }
        });
        p.set_block_number(1_000);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(c.check_liveness().unwrap().is_empty());
            assert_eq!(c.miss_count.read(&v(3)).unwrap(), 1);
            for addr in [FIXTURE_VALIDATOR, v(2), v(4)] {
                assert_eq!(c.miss_count.read(&addr).unwrap(), 0);
            }
            // v(3) catches up with a fresh submission: it is not part of the
            // reference yet, but v(3) itself is no longer behind.
            write_submission(&c, v(3), LOCAL, 600, 999);
            assert!(c.check_liveness().unwrap().is_empty());
            assert_eq!(c.miss_count.read(&v(3)).unwrap(), 0);
        });
    }

    #[test]
    fn newcomer_is_stamped_and_evaluated_from_the_next_boundary() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2);
            }
            let c = HyperlaneControllerContract::new(storage);
            for addr in [FIXTURE_VALIDATOR, v(2), v(3)] {
                write_submission(&c, addr, LOCAL, 50, 100);
                write_submission(&c, addr, SEPOLIA, 50, 100);
            }
        });
        p.set_block_number(1_000);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            c.check_liveness().unwrap();
            assert_eq!(c.miss_count.read(&v(4)).unwrap(), 0);
            assert_eq!(
                c.submitted_block
                    .read(&validator_domain_key(v(4), LOCAL))
                    .unwrap(),
                1_000
            );
        });
        p.set_block_number(2_200);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            c.check_liveness().unwrap();
            assert_eq!(c.miss_count.read(&v(4)).unwrap(), 1);
        });
    }

    #[test]
    fn idle_bridge_and_below_quorum_data_produce_no_misses() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2);
            }
            let c = HyperlaneControllerContract::new(storage);
            // Only two settled submissions: below the threshold of three,
            // so there is no reference and nobody can be behind.
            write_submission(&c, FIXTURE_VALIDATOR, LOCAL, 500, 100);
            write_submission(&c, v(2), LOCAL, 500, 100);
            write_submission(&c, v(3), LOCAL, 1, 100);
            write_submission(&c, v(4), LOCAL, 1, 100);
            for addr in [FIXTURE_VALIDATOR, v(2), v(3), v(4)] {
                write_submission(&c, addr, SEPOLIA, 0, 100);
            }
        });
        p.set_block_number(1_000);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            // Reference for LOCAL is the third highest: 1. Everyone is at or
            // above it; SEPOLIA never moved. No misses.
            assert!(c.check_liveness().unwrap().is_empty());
            for addr in [FIXTURE_VALIDATOR, v(2), v(3), v(4)] {
                assert_eq!(c.miss_count.read(&addr).unwrap(), 0);
            }
        });
    }

    #[test]
    fn consecutive_misses_jail_and_sync_drops_the_validator() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2);
            }
            let c = HyperlaneControllerContract::new(storage);
            for addr in [FIXTURE_VALIDATOR, v(2), v(4)] {
                write_submission(&c, addr, LOCAL, 500, 100);
                write_submission(&c, addr, SEPOLIA, 5, 100);
            }
            write_submission(&c, v(3), LOCAL, 7, 100);
            write_submission(&c, v(3), SEPOLIA, 5, 100);
        });
        let mut jailed = Vec::new();
        for boundary in 1..=MAX_MISSES {
            p.set_block_number(1_000 * u64::from(boundary));
            StorageHandle::enter(&mut p, |storage| {
                let mut c = HyperlaneControllerContract::new(storage);
                jailed = c.check_liveness().unwrap();
            });
        }
        assert_eq!(jailed, vec![v(3)]);
        assert_eq!(
            *topics(&p).last().unwrap(),
            IHyperlaneController::LivenessJailed::SIGNATURE_HASH
        );
        StorageHandle::enter(&mut p, |storage| {
            let vs = ValidatorSet::new(storage.clone());
            assert!(!vs.validator_lifecycle(v(3)).unwrap().is_active_status());
            let active: Vec<Address> = vs
                .get_active_validators()
                .unwrap()
                .into_iter()
                .map(|r| r.validator_address)
                .collect();
            assert_eq!(active.len(), 3);
            assert!(!active.contains(&v(3)));
            let c = HyperlaneControllerContract::new(storage);
            assert_eq!(c.miss_count.read(&v(3)).unwrap(), 0);
        });
    }
}
