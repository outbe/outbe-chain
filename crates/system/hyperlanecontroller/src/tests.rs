//! Unit tests for the Hyperlane controller module.
//!
//! A [`RecordingOwner`] stands in for the CrosschainGov precompile. It
//! records the owner calls and does not run them.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;

use crate::abi::ICrosschainGov;
use crate::errors::HyperlaneControllerError;
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IOwnable, IStorageMultisigIsm};
use crate::test_support::*;
use crate::RemoteCall;

/// Runs a Hyperlane selector with a fresh [`RecordingOwner`].
fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
) -> Result<Bytes, PrecompileError> {
    dispatch_with(storage, data, caller, &mut RecordingOwner::default())
}

fn set_validators_call(validators: &[Address], threshold: u8) -> Bytes {
    IStorageMultisigIsm::setValidatorsAndThresholdCall {
        _validators: validators.to_vec(),
        _threshold: threshold,
    }
    .abi_encode()
    .into()
}

fn accept_ownership() -> Bytes {
    IStorageMultisigIsm::acceptOwnershipCall {}
        .abi_encode()
        .into()
}

#[test]
fn initialize_accepts_ism_ownership_and_stores_table() {
    let mut p = provider();
    stub_pre_initialize(&mut p);
    let mut owner = RecordingOwner::default();
    StorageHandle::enter(&mut p, |storage| {
        dispatch_with(storage.clone(), &initialize_call(), deployer(), &mut owner).unwrap();
        let c = HyperlaneControllerContract::new(storage);
        assert_eq!(c.ica_router.read().unwrap(), ica_router());
        assert_eq!(c.validator_announce.read().unwrap(), validator_announce());
        assert_eq!(c.ism_by_domain.read(&LOCAL).unwrap(), local_ism());
        assert_eq!(c.ism_by_domain.read(&SEPOLIA).unwrap(), sepolia_ism());
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, SEPOLIA, BSC]);
    });
    // The owner accepts the local ISM on Outbe, then the two remote ISMs
    // through its Interchain Account.
    assert_eq!(
        owner.local,
        vec![(local_ism(), U256::ZERO, accept_ownership())]
    );
    assert_eq!(
        owner.remote,
        vec![
            (
                SEPOLIA,
                vec![RemoteCall::new(sepolia_ism(), accept_ownership())]
            ),
            (BSC, vec![RemoteCall::new(bsc_ism(), accept_ownership())]),
        ]
    );
    let t = topics(&p);
    assert_eq!(
        t.iter()
            .filter(|h| **h == ICrosschainGov::DomainAdded::SIGNATURE_HASH)
            .count(),
        3
    );
    assert_eq!(
        *t.last().unwrap(),
        ICrosschainGov::Initialized::SIGNATURE_HASH
    );
}

#[test]
fn initialize_is_gated_and_one_shot() {
    // Not the bootstrap admin, even when it owns the ISM from calldata.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    p.stub_sub_call_at_selector(
        local_ism(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(stranger()),
    );
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), stranger()).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::NotBootstrapAdmin::SELECTOR
        );
    });

    // No admin seeded: nobody can initialize.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        HyperlaneControllerContract::new(storage.clone())
            .bootstrap_admin
            .write(Address::ZERO)
            .unwrap();
        let err = dispatch(storage, &initialize_call(), Address::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::NotBootstrapAdmin::SELECTOR
        );
    });

    // Admin, but not the local ISM owner.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    p.stub_sub_call_at_selector(
        local_ism(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(stranger()),
    );
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer()).unwrap_err();
        assert_eq!(revert_selector(err), ICrosschainGov::NotIsmOwner::SELECTOR);
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
        let err = dispatch(storage, &initialize_call(), deployer()).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::IsmNotPendingToController::SELECTOR
        );
    });

    // Router not yet transferred.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    p.stub_sub_call_at_selector(
        ica_router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(deployer()),
    );
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer()).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::NotOwnedByController::SELECTOR
        );
    });

    // Table without the local domain.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        let call: Bytes = ICrosschainGov::initializeCall {
            icaRouter: ica_router(),
            validatorAnnounce: validator_announce(),
            domains: vec![SEPOLIA],
            isms: vec![sepolia_ism()],
            hooks: vec![hook(2)],
        }
        .abi_encode()
        .into();
        let err = dispatch(storage, &call, deployer()).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::LocalDomainMissing::SELECTOR
        );
    });

    // Second call.
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer()).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::AlreadyInitialized::SELECTOR
        );
    });
}

#[test]
fn rotation_sends_every_remote_ism_then_the_local_one_through_the_owner() {
    let mut p = initialized_provider();
    p.clear_events(CROSSCHAIN_GOV_ADDRESS);
    let mut owner = RecordingOwner::default();
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        c.set_validators_and_threshold(&[v(1), v(2), v(3)], 2, &mut owner)
            .unwrap();
    });
    let data = set_validators_call(&[v(1), v(2), v(3)], 2);
    assert_eq!(
        owner.remote,
        vec![
            (SEPOLIA, vec![RemoteCall::new(sepolia_ism(), data.clone())]),
            (BSC, vec![RemoteCall::new(bsc_ism(), data.clone())]),
        ]
    );
    assert_eq!(owner.local, vec![(local_ism(), U256::ZERO, data)]);
    assert_eq!(
        topics(&p),
        vec![ICrosschainGov::ValidatorsAndThresholdApplied::SIGNATURE_HASH]
    );
}

#[test]
fn domains_can_be_added_and_removed_except_the_local_one() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let mut owner = RecordingOwner::default();
        let mut c = HyperlaneControllerContract::new(storage);
        c.add_domain(1, (v(5), hook(5)), &mut owner).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, SEPOLIA, BSC, 1]);
        c.add_domain(1, (v(6), hook(6)), &mut owner).unwrap();
        assert_eq!(c.ism_by_domain.read(&1).unwrap(), v(6));
        assert_eq!(c.domains.read_all().unwrap().len(), 4);

        c.remove_domain(SEPOLIA).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, BSC, 1]);
        assert_eq!(c.ism_by_domain.read(&SEPOLIA).unwrap(), Address::ZERO);

        assert!(c.remove_domain(SEPOLIA).is_err());
        assert!(c.remove_domain(LOCAL).is_err());
        assert!(c.add_domain(LOCAL, (v(7), hook(7)), &mut owner).is_err());
        assert!(c.add_domain(0, (v(7), hook(7)), &mut owner).is_err());
        // Each accepted add sends `acceptOwnership()` to the new ISM.
        assert_eq!(
            owner.remote,
            vec![
                (1, vec![RemoteCall::new(v(5), accept_ownership())]),
                (1, vec![RemoteCall::new(v(6), accept_ownership())]),
            ]
        );
    });
}

#[test]
fn operations_require_initialization() {
    let mut p = provider();
    p.enable_sub_call_stub();
    let mut owner = RecordingOwner::default();
    StorageHandle::enter(&mut p, |storage| {
        let mut c = HyperlaneControllerContract::new(storage);
        assert!(matches!(
            c.set_validators_and_threshold(&[v(1)], 1, &mut owner),
            Err(PrecompileError::RevertBytes(_))
        ));
        assert!(c.add_domain(SEPOLIA, (v(1), hook(1)), &mut owner).is_err());
        assert!(c.require_initialized().is_err());
    });
    assert!(owner.local.is_empty() && owner.remote.is_empty());
}

#[test]
fn crosschaingov_selectors_are_not_hyperlane_selectors() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = ICrosschainGov::fundCall {}.abi_encode().into();
        assert!(dispatch(storage.clone(), &fund, stranger()).is_err());
        let account: Bytes = ICrosschainGov::remoteAccountCall { domain: SEPOLIA }
            .abi_encode()
            .into();
        assert!(dispatch(storage, &account, stranger()).is_err());
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

const VALIDATOR_OWNER: Address = address!("0xffffffffffffffffffffffffffffffffffffffff");

/// Registers `addr` with a BLS key that starts with `seed` and activates it
/// through a boundary. The first call also configures the validator set.
fn activate(storage: StorageHandle<'_>, addr: Address, seed: u8) -> Result<(), PrecompileError> {
    let mut vs = ValidatorSet::new(storage);
    if vs.config_owner.read()?.is_zero() {
        vs.test_configure_registry(VALIDATOR_OWNER)?;
    }
    let mut pubkey = [0u8; 48];
    pubkey[0] = seed;
    let minimum = U256::from(1_000u64);
    vs.test_seed_authoritative_min_stake(minimum)?;
    vs.register_validator(VALIDATOR_OWNER, addr, &pubkey)?;
    vs.test_seed_authoritative_bonded(addr, minimum)?;
    vs.activate_validator_via_boundary_for_test(addr)?;
    Ok(())
}

mod sync {
    use super::*;

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
        stub_router_and_ism(&mut p, U256::ZERO, (&[v(1)], 1));
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(1), v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 1).unwrap();
            }
        });
        // v4 signs Hyperlane checkpoints with a dedicated key.
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            c.set_hyperlane_signer(v(4), v(9)).unwrap();
        });
        p.clear_events(CROSSCHAIN_GOV_ADDRESS);
        let mut owner = RecordingOwner::default();
        StorageHandle::enter(&mut p, |storage| {
            let call: Bytes = ICrosschainGov::syncCall {}.abi_encode().into();
            let ret = dispatch_with(storage, &call, stranger(), &mut owner).unwrap();
            assert!(ICrosschainGov::syncCall::abi_decode_returns(&ret).unwrap());
        });
        // One `setValidatorsAndThreshold` to each remote ISM, then one to the
        // local ISM. The set holds the Hyperlane signer of each validator.
        let data = set_validators_call(&[v(1), v(2), v(3), v(9)], 3);
        assert_eq!(
            owner.remote,
            vec![
                (SEPOLIA, vec![RemoteCall::new(sepolia_ism(), data.clone())]),
                (BSC, vec![RemoteCall::new(bsc_ism(), data.clone())]),
            ]
        );
        assert_eq!(owner.local, vec![(local_ism(), U256::ZERO, data)]);
        let t = topics(&p);
        assert_eq!(
            *t.last().unwrap(),
            ICrosschainGov::ValidatorsAndThresholdApplied::SIGNATURE_HASH
        );
        let applied = ICrosschainGov::ValidatorsAndThresholdApplied::decode_log_data(
            p.get_events(CROSSCHAIN_GOV_ADDRESS).last().unwrap(),
        )
        .unwrap();
        assert_eq!(applied.threshold, 3);
        assert_eq!(applied.validatorCount, U256::from(4));

        // ISM now reports the mirrored set (any order): nothing to do.
        stub_router_and_ism(&mut p, U256::ZERO, (&[v(9), v(3), v(2), v(1)], 3));
        p.clear_events(CROSSCHAIN_GOV_ADDRESS);
        let mut owner = RecordingOwner::default();
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(!c.sync(&mut owner).unwrap());
        });
        assert!(topics(&p).is_empty());
        assert!(owner.local.is_empty() && owner.remote.is_empty());

        // The validator address of v4 is not its signer: that set is stale.
        stub_router_and_ism(&mut p, U256::ZERO, (&[v(4), v(3), v(2), v(1)], 3));
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(c.sync(&mut owner).unwrap());
        });
    }
}

mod liveness {
    use super::*;
    use crate::liveness::{checkpoint_digest, GRACE_BLOCKS, MAX_MISSES};
    use crate::schema::validator_domain_key;
    use alloy_primitives::{b256, hex};
    use outbe_primitives::tee_signatures::recover_signer;

    /// Real checkpoints signed on outbetestnet by validator 0x4fe927… for the
    /// (since redeployed) MerkleTreeHook 0x6543cef9…, taken from its S3 bucket.
    const FIXTURE_HOOK: Address = address!("0x6543cef9bbe42d66b5b36ccaf9374de2b55f9cbc");
    const FIXTURE_VALIDATOR: Address = address!("0x4fe927ab711793954b3a29969ecd4a60d6d265d0");
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
        ICrosschainGov::submitCheckpointCall {
            domain,
            root: checkpoint.root,
            index,
            messageId: checkpoint.message_id,
            signature: Bytes::copy_from_slice(&checkpoint.signature),
        }
        .abi_encode()
        .into()
    }

    /// Initialized controller with the fixture hook on the local domain and
    /// the fixture validator active.
    fn liveness_provider() -> HashMapStorageProvider {
        let mut p = provider();
        stub_pre_initialize(&mut p);
        StorageHandle::enter(&mut p, |storage| {
            let call: Bytes = ICrosschainGov::initializeCall {
                icaRouter: ica_router(),
                validatorAnnounce: validator_announce(),
                domains: vec![LOCAL, SEPOLIA],
                isms: vec![local_ism(), sepolia_ism()],
                hooks: vec![FIXTURE_HOOK, hook(2)],
            }
            .abi_encode()
            .into();
            dispatch(storage.clone(), &call, deployer()).unwrap();
            activate(storage, FIXTURE_VALIDATOR, 1).unwrap();
        });
        p
    }

    /// Stores a submission of `index` at `block` under the
    /// `validator_domain_key` `key`.
    fn write_submission(
        c: &HyperlaneControllerContract<'_>,
        key: B256,
        index: u32,
        block: u64,
    ) -> Result<(), PrecompileError> {
        c.submitted_index.write(&key, index)?;
        c.submitted_block.write(&key, block)
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
        p.clear_events(CROSSCHAIN_GOV_ADDRESS);
        StorageHandle::enter(&mut p, |storage| {
            let two = checkpoint_2();
            dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 2),
                FIXTURE_VALIDATOR,
            )
            .unwrap();
            let view: Bytes = ICrosschainGov::submittedIndexCall {
                validator: FIXTURE_VALIDATOR,
                domain: LOCAL,
            }
            .abi_encode()
            .into();
            let ret = dispatch(storage.clone(), &view, stranger()).unwrap();
            assert_eq!(
                ICrosschainGov::submittedIndexCall::abi_decode_returns(&ret).unwrap(),
                2
            );

            let not_validator =
                dispatch(storage.clone(), &submit_call(LOCAL, &two, 2), stranger()).unwrap_err();
            assert_eq!(
                revert_selector(not_validator),
                ICrosschainGov::NotActiveValidator::SELECTOR
            );

            let no_hook = dispatch(
                storage.clone(),
                &submit_call(BSC, &two, 2),
                FIXTURE_VALIDATOR,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(no_hook),
                ICrosschainGov::UnknownHook::SELECTOR
            );

            let tampered = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 3),
                FIXTURE_VALIDATOR,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(tampered),
                ICrosschainGov::SignerMismatch::SELECTOR
            );

            let older = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &checkpoint_0(), 0),
                FIXTURE_VALIDATOR,
            )
            .unwrap_err();
            assert_eq!(revert_selector(older), ICrosschainGov::StaleIndex::SELECTOR);

            let repeat =
                dispatch(storage, &submit_call(LOCAL, &two, 2), FIXTURE_VALIDATOR).unwrap_err();
            assert_eq!(
                revert_selector(repeat),
                ICrosschainGov::StaleIndex::SELECTOR
            );
        });
        let submitted = topics(&p)
            .iter()
            .filter(|h| **h == ICrosschainGov::CheckpointSubmitted::SIGNATURE_HASH)
            .count();
        assert_eq!(submitted, 1);
    }

    #[test]
    fn a_signer_cannot_be_shared_between_active_validators() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            activate(storage.clone(), v(2), 2).unwrap();
            let mut c = HyperlaneControllerContract::new(storage);
            c.set_hyperlane_signer(v(2), v(9)).unwrap();
            let err = c.set_hyperlane_signer(FIXTURE_VALIDATOR, v(9)).unwrap_err();
            assert_eq!(revert_selector(err), ICrosschainGov::SignerTaken::SELECTOR);
            let err = c.set_hyperlane_signer(FIXTURE_VALIDATOR, v(2)).unwrap_err();
            assert_eq!(revert_selector(err), ICrosschainGov::SignerTaken::SELECTOR);
            c.set_hyperlane_signer(v(2), Address::ZERO).unwrap();
            c.set_hyperlane_signer(FIXTURE_VALIDATOR, v(9)).unwrap();
        });
    }

    #[test]
    fn registered_signer_replaces_the_validator_address() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            // A stranger key cannot register a signer, the validator can.
            let set: Bytes = ICrosschainGov::setHyperlaneSignerCall { signer: v(9) }
                .abi_encode()
                .into();
            assert!(dispatch(storage.clone(), &set, stranger()).is_err());
            dispatch(storage.clone(), &set, FIXTURE_VALIDATOR).unwrap();
            // The fixture is signed by the validator key, not v(9): rejected now.
            let err = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &checkpoint_2(), 2),
                FIXTURE_VALIDATOR,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(err),
                ICrosschainGov::SignerMismatch::SELECTOR
            );
            // Reset to the validator address.
            let reset: Bytes = ICrosschainGov::setHyperlaneSignerCall {
                signer: Address::ZERO,
            }
            .abi_encode()
            .into();
            dispatch(storage.clone(), &reset, FIXTURE_VALIDATOR).unwrap();
            dispatch(
                storage,
                &submit_call(LOCAL, &checkpoint_2(), 2),
                FIXTURE_VALIDATOR,
            )
            .unwrap();
        });
    }

    /// Four validators: the reference is the third-highest settled index, so
    /// one inflated submission cannot move it, and the verdict ignores fresh
    /// ones.
    #[test]
    fn reference_index_is_quorum_based_and_grace_gated() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2).unwrap();
            }
            let c = HyperlaneControllerContract::new(storage);
            let old = 1_000 - GRACE_BLOCKS;
            write_submission(&c, validator_domain_key(FIXTURE_VALIDATOR, LOCAL), 500, old).unwrap();
            write_submission(&c, validator_domain_key(v(2), LOCAL), 498, old).unwrap();
            write_submission(&c, validator_domain_key(v(3), LOCAL), 120, old).unwrap();
            write_submission(&c, validator_domain_key(v(4), LOCAL), 999_999, old).unwrap();
            for addr in [FIXTURE_VALIDATOR, v(2), v(3), v(4)] {
                write_submission(&c, validator_domain_key(addr, SEPOLIA), 10, old).unwrap();
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
            // v(3) closes the gap with a fresh submission. It is not part of the
            // reference yet, but v(3) itself is no longer behind.
            write_submission(&c, validator_domain_key(v(3), LOCAL), 600, 999).unwrap();
            assert!(c.check_liveness().unwrap().is_empty());
            assert_eq!(c.miss_count.read(&v(3)).unwrap(), 0);
        });
    }

    #[test]
    fn newcomer_is_stamped_and_evaluated_from_the_next_boundary() {
        let mut p = liveness_provider();
        StorageHandle::enter(&mut p, |storage| {
            for (i, addr) in [v(2), v(3), v(4)].into_iter().enumerate() {
                activate(storage.clone(), addr, i as u8 + 2).unwrap();
            }
            let c = HyperlaneControllerContract::new(storage);
            for addr in [FIXTURE_VALIDATOR, v(2), v(3)] {
                write_submission(&c, validator_domain_key(addr, LOCAL), 50, 100).unwrap();
                write_submission(&c, validator_domain_key(addr, SEPOLIA), 50, 100).unwrap();
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
                activate(storage.clone(), addr, i as u8 + 2).unwrap();
            }
            let c = HyperlaneControllerContract::new(storage);
            // Only two settled submissions: below the threshold of three,
            // so there is no reference and nobody can be behind.
            write_submission(&c, validator_domain_key(FIXTURE_VALIDATOR, LOCAL), 500, 100).unwrap();
            write_submission(&c, validator_domain_key(v(2), LOCAL), 500, 100).unwrap();
            write_submission(&c, validator_domain_key(v(3), LOCAL), 1, 100).unwrap();
            write_submission(&c, validator_domain_key(v(4), LOCAL), 1, 100).unwrap();
            for addr in [FIXTURE_VALIDATOR, v(2), v(3), v(4)] {
                write_submission(&c, validator_domain_key(addr, SEPOLIA), 0, 100).unwrap();
            }
        });
        p.set_block_number(1_000);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            // Reference for LOCAL is the third highest: 1. Everyone is at or
            // above it. SEPOLIA never moved. No misses.
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
                activate(storage.clone(), addr, i as u8 + 2).unwrap();
            }
            let c = HyperlaneControllerContract::new(storage);
            for addr in [FIXTURE_VALIDATOR, v(2), v(4)] {
                write_submission(&c, validator_domain_key(addr, LOCAL), 500, 100).unwrap();
                write_submission(&c, validator_domain_key(addr, SEPOLIA), 5, 100).unwrap();
            }
            write_submission(&c, validator_domain_key(v(3), LOCAL), 7, 100).unwrap();
            write_submission(&c, validator_domain_key(v(3), SEPOLIA), 5, 100).unwrap();
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
            ICrosschainGov::LivenessJailed::SIGNATURE_HASH
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
