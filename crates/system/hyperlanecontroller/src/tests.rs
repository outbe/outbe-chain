//! Unit tests for the Hyperlane controller precompile.
//!
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs. `stub_sub_call_at_selector` pins a target's reply per
//! selector. `enable_sub_call_stub()` makes every other sub-call succeed
//! with empty returndata.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolValue};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::errors::HyperlaneControllerError;
use crate::precompile::{dispatch, IHyperlaneController};
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use crate::RemoteCall;
use outbe_primitives::error::PrecompileError;
use outbe_validatorset::contract::ValidatorSet;

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
fn ica_router() -> Address {
    address!("0x0000000000000000000000000000000000000626")
}
fn validator_announce() -> Address {
    address!("0x0000000000000000000000000000000000000627")
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
/// already owns the ICA router: the state right after `mise run owner:transfer`.
/// The router quotes a zero fee so `initialize` can dispatch the remote
/// `acceptOwnership()` calls without a balance.
fn stub_pre_initialize(p: &mut HashMapStorageProvider) {
    StorageHandle::enter(p, |storage| {
        HyperlaneControllerContract::new(storage)
            .bootstrap_admin
            .write(deployer())
            .unwrap();
    });
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(U256::ZERO.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
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
        ica_router(),
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
        icaRouter: ica_router(),
        validatorAnnounce: validator_announce(),
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
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(fee.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
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

fn revert_selector(err: outbe_primitives::error::PrecompileError) -> [u8; 4] {
    match err {
        outbe_primitives::error::PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}

#[test]
fn initialize_accepts_ism_ownership_and_stores_table() {
    let p = initialized_provider();
    let mut p = p;
    StorageHandle::enter(&mut p, |storage| {
        let c = HyperlaneControllerContract::new(storage);
        assert_eq!(c.ica_router.read().unwrap(), ica_router());
        assert_eq!(c.validator_announce.read().unwrap(), validator_announce());
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
    // acceptOwnership() dispatched to the two remote ISMs, not the local one.
    let accepts: Vec<_> = p
        .get_events(HYPERLANE_CONTROLLER_ADDRESS)
        .iter()
        .filter(|log| log.topics()[0] == IHyperlaneController::RemoteCallDispatched::SIGNATURE_HASH)
        .map(|log| {
            IHyperlaneController::RemoteCallDispatched::decode_log_data(log)
                .unwrap()
                .domain
        })
        .collect();
    assert_eq!(accepts, vec![SEPOLIA, BSC]);
    assert_eq!(
        *t.last().unwrap(),
        IHyperlaneController::Initialized::SIGNATURE_HASH
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
        let err = dispatch(storage, &initialize_call(), stranger(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::NotBootstrapAdmin::SELECTOR
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
        let err = dispatch(storage, &initialize_call(), Address::ZERO, U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::NotBootstrapAdmin::SELECTOR
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
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::NotIsmOwner::SELECTOR
        );
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
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::IsmNotPendingToController::SELECTOR
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
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::NotOwnedByController::SELECTOR
        );
    });

    // Table without the local domain.
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        let call: Bytes = IHyperlaneController::initializeCall {
            icaRouter: ica_router(),
            validatorAnnounce: validator_announce(),
            domains: vec![SEPOLIA],
            isms: vec![sepolia_ism()],
            hooks: vec![hook(2)],
        }
        .abi_encode()
        .into();
        let err = dispatch(storage, &call, deployer(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::LocalDomainMissing::SELECTOR
        );
    });

    // Second call.
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::AlreadyInitialized::SELECTOR
        );
    });
}

#[test]
fn fund_is_the_only_payable_selector() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = IHyperlaneController::fundCall {}.abi_encode().into();
        dispatch(storage.clone(), &fund, stranger(), U256::from(5)).unwrap();
        assert!(dispatch(storage.clone(), &fund, stranger(), U256::ZERO).is_err());
        let view: Bytes = IHyperlaneController::icaRouterCall {}.abi_encode().into();
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
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::InsufficientBalance::SELECTOR
        );
    });
    assert!(topics(&p).is_empty());
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
fn remote_account_is_the_router_derived_account() {
    let mut p = initialized_provider();
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::getRemoteInterchainAccountCall::SELECTOR,
        word_addr(v(0x1c)),
    );
    StorageHandle::enter(&mut p, |storage| {
        let call = IHyperlaneController::remoteAccountCall { domain: SEPOLIA };
        let ret = dispatch(storage, &call.abi_encode(), stranger(), U256::ZERO).unwrap();
        let account = IHyperlaneController::remoteAccountCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(account, v(0x1c));
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
            Err(outbe_primitives::error::PrecompileError::RevertBytes(_))
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
        stub_router_and_ism(&mut p, U256::ZERO, &[v(1)], 1);
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
        stub_router_and_ism(&mut p, U256::ZERO, &[v(9), v(3), v(2), v(1)], 3);
        p.clear_events(HYPERLANE_CONTROLLER_ADDRESS);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(!c.sync().unwrap());
        });
        assert!(topics(&p).is_empty());

        // The validator address of v4 is not its signer: that set is stale.
        stub_router_and_ism(&mut p, U256::ZERO, &[v(4), v(3), v(2), v(1)], 3);
        StorageHandle::enter(&mut p, |storage| {
            let mut c = HyperlaneControllerContract::new(storage);
            assert!(c.sync().unwrap());
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

    /// Initialized controller with the fixture hook on the local domain and
    /// the fixture validator active.
    fn liveness_provider() -> HashMapStorageProvider {
        let mut p = provider();
        stub_pre_initialize(&mut p);
        StorageHandle::enter(&mut p, |storage| {
            let call: Bytes = IHyperlaneController::initializeCall {
                icaRouter: ica_router(),
                validatorAnnounce: validator_announce(),
                domains: vec![LOCAL, SEPOLIA],
                isms: vec![local_ism(), sepolia_ism()],
                hooks: vec![FIXTURE_HOOK, hook(2)],
            }
            .abi_encode()
            .into();
            dispatch(storage.clone(), &call, deployer(), U256::ZERO).unwrap();
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
            assert_eq!(
                revert_selector(not_validator),
                IHyperlaneController::NotActiveValidator::SELECTOR
            );

            let no_hook = dispatch(
                storage.clone(),
                &submit_call(BSC, &two, 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(no_hook),
                IHyperlaneController::UnknownHook::SELECTOR
            );

            let tampered = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &two, 3),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(tampered),
                IHyperlaneController::SignerMismatch::SELECTOR
            );

            let older = dispatch(
                storage.clone(),
                &submit_call(LOCAL, &checkpoint_0(), 0),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(older),
                IHyperlaneController::StaleIndex::SELECTOR
            );

            let repeat = dispatch(
                storage,
                &submit_call(LOCAL, &two, 2),
                FIXTURE_VALIDATOR,
                U256::ZERO,
            )
            .unwrap_err();
            assert_eq!(
                revert_selector(repeat),
                IHyperlaneController::StaleIndex::SELECTOR
            );
        });
        let submitted = topics(&p)
            .iter()
            .filter(|h| **h == IHyperlaneController::CheckpointSubmitted::SIGNATURE_HASH)
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
            assert_eq!(
                revert_selector(err),
                IHyperlaneController::SignerTaken::SELECTOR
            );
            let err = c.set_hyperlane_signer(FIXTURE_VALIDATOR, v(2)).unwrap_err();
            assert_eq!(
                revert_selector(err),
                IHyperlaneController::SignerTaken::SELECTOR
            );
            c.set_hyperlane_signer(v(2), Address::ZERO).unwrap();
            c.set_hyperlane_signer(FIXTURE_VALIDATOR, v(9)).unwrap();
        });
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
            assert_eq!(
                revert_selector(err),
                IHyperlaneController::SignerMismatch::SELECTOR
            );
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

mod vote_target {
    use super::*;
    use crate::sol_ext::{IERC7786Bridge, IHyperlaneGatewayAdapter};
    use crate::CrosschainVoteTarget;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_vote::handlers::{TargetExecutionOutcome, VoteTarget, VoteTargetContext};

    const APPROVED_AT: u64 = 30;
    const ARBITRUM: u32 = 42_161;

    fn bridge() -> Address {
        v(0xb1)
    }

    fn context() -> VoteTargetContext {
        VoteTargetContext {
            proposer: v(1),
            attached_value: U256::ZERO,
            block_number: APPROVED_AT,
            chain_id: CHAIN_ID,
        }
    }

    fn pause_on(chain_id: u32) -> String {
        format!(
            r#"{{"operation":"setBridgePaused","chainId":{chain_id},"bridge":"{:#x}","paused":true}}"#,
            bridge()
        )
    }

    fn link_arbitrum() -> String {
        format!(
            r#"{{"operation":"linkBridges","remoteChainId":{ARBITRUM},"bridge":"{:#x}","adapter":"{:#x}","remoteBridge":"{:#x}","remoteAdapter":"{:#x}"}}"#,
            bridge(),
            v(0xa1),
            v(0xb2),
            v(0xa2)
        )
    }

    fn approve(p: &mut HashMapStorageProvider, payload: &str) {
        StorageHandle::enter(p, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(APPROVED_AT, 1_700_000_000, CHAIN_ID),
                storage,
            );
            assert_eq!(
                CrosschainVoteTarget
                    .handle_approved(&ctx, U256::ONE, payload.as_bytes(), context())
                    .unwrap(),
                TargetExecutionOutcome::Applied
            );
        });
        p.set_block_number(APPROVED_AT);
    }

    fn apply(p: &mut HashMapStorageProvider, payload: &str) -> Result<Bytes, PrecompileError> {
        let call = IHyperlaneController::applyApprovedCall {
            proposalId: U256::ONE,
            payload: payload.as_bytes().to_vec().into(),
        };
        StorageHandle::enter(p, |storage| {
            dispatch(storage, &call.abi_encode(), stranger(), U256::ZERO)
        })
    }

    fn sub_calls_from(p: &HashMapStorageProvider, start: usize) -> Vec<(Address, Bytes)> {
        p.sub_call_inputs()[start..]
            .iter()
            .filter(|input| !input.is_static)
            .map(|input| (input.target, input.calldata.clone()))
            .collect()
    }

    fn remote_calls(calldata: &Bytes) -> (u32, Vec<(Address, Bytes)>) {
        let call = IInterchainAccountRouter::callRemoteCall::abi_decode(calldata).unwrap();
        let calls = call
            ._calls
            .into_iter()
            .map(|c| (Address::from_word(c.to), c.data))
            .collect();
        (call._destination, calls)
    }

    #[test]
    fn validate_accepts_every_action_and_rejects_bad_fields() {
        let valid = [
            pause_on(LOCAL),
            pause_on(SEPOLIA).replace("true", "false"),
            format!(
                r#"{{"operation":"setBridgeGateway","chainId":{LOCAL},"bridge":"{:#x}","destinationChainId":{ARBITRUM},"adapter":"{:#x}"}}"#,
                bridge(),
                Address::ZERO
            ),
            link_arbitrum(),
            format!(
                r#"{{"operation":"linkTokenBridges","remoteChainId":{ARBITRUM},"tokenBridge":"{:#x}","remoteTokenBridge":"{:#x}"}}"#,
                v(0xc1),
                v(0xc2)
            ),
            format!(
                r#"{{"operation":"addHyperlaneChain","chainId":{ARBITRUM},"icaRouter":"{:#x}","ism":"{:#x}","hook":"{:#x}"}}"#,
                v(0xe1),
                v(0xd1),
                hook(9)
            ),
            format!(r#"{{"operation":"removeHyperlaneChain","chainId":{ARBITRUM}}}"#),
        ];
        for payload in &valid {
            CrosschainVoteTarget
                .validate(payload.as_bytes(), context())
                .unwrap_or_else(|e| panic!("{payload}: {e:?}"));
        }
        let invalid = [
            "{}".to_string(),
            pause_on(LOCAL).replace("setBridgePaused", "callLocal"),
            pause_on(LOCAL).replace(
                &format!("{:#x}", bridge()),
                &format!("{:#x}", Address::ZERO),
            ),
            pause_on(LOCAL).replace(r#""paused":true"#, r#""paused":true,"paused":false"#),
            pause_on(LOCAL).replace('}', r#","value":1}"#),
            link_arbitrum().replace(&ARBITRUM.to_string(), &LOCAL.to_string()),
            link_arbitrum().replace('}', r#","transport":"layerZero"}"#),
            format!(r#"{{"operation":"removeHyperlaneChain","chainId":{LOCAL}}}"#),
        ];
        for payload in &invalid {
            assert!(
                CrosschainVoteTarget
                    .validate(payload.as_bytes(), context())
                    .is_err(),
                "{payload}"
            );
        }
    }

    #[test]
    fn link_bridges_wires_outbe_then_sends_the_mirror_wiring_once() {
        let mut p = initialized_provider();
        let payload = link_arbitrum();
        approve(&mut p, &payload);
        let start = p.sub_call_inputs().len();
        apply(&mut p, &payload).unwrap();

        // ERC-7930: version 1, chain type 0, chain id 42161 (0xa4b1) in two bytes, the address.
        let mut arbitrum_bridge = alloy_primitives::hex!("0001000002a4b114").to_vec();
        arbitrum_bridge.extend_from_slice(v(0xb2).as_slice());
        let gateway = |chain_id: u32, adapter: Address| {
            Bytes::from(
                IERC7786Bridge::setGatewayCall {
                    chainId: U256::from(chain_id),
                    gateway: adapter,
                }
                .abi_encode(),
            )
        };
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent.len(), 4);
        assert_eq!(
            sent[..3],
            [
                (
                    v(0xa1),
                    Bytes::from(
                        IHyperlaneGatewayAdapter::setRouterWithChainCall {
                            domain: ARBITRUM,
                            router: v(0xa2).into_word(),
                            chainId: U256::from(ARBITRUM),
                        }
                        .abi_encode(),
                    ),
                ),
                (
                    bridge(),
                    Bytes::from(
                        IERC7786Bridge::registerRemoteBridgeCall {
                            bridge: arbitrum_bridge.into(),
                        }
                        .abi_encode(),
                    ),
                ),
                (bridge(), gateway(ARBITRUM, v(0xa1))),
            ]
        );
        assert_eq!(sent[3].0, ica_router());
        let (destination, mirror) = remote_calls(&sent[3].1);
        assert_eq!(destination, ARBITRUM);
        let targets: Vec<Address> = mirror.iter().map(|(to, _)| *to).collect();
        assert_eq!(targets, [v(0xa2), v(0xb2), v(0xb2)]);
        assert_eq!(mirror[2].1, gateway(LOCAL, v(0xa2)));

        let err = apply(&mut p, &payload).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::UnknownApprovedAction::SELECTOR
        );
    }

    #[test]
    fn single_chain_action_runs_locally_or_as_one_remote_message() {
        let mut p = initialized_provider();
        approve(&mut p, &pause_on(LOCAL));
        let start = p.sub_call_inputs().len();
        apply(&mut p, &pause_on(LOCAL)).unwrap();
        let pause = Bytes::from(IERC7786Bridge::pauseCall {}.abi_encode());
        assert_eq!(sub_calls_from(&p, start), [(bridge(), pause.clone())]);

        let mut p = initialized_provider();
        approve(&mut p, &pause_on(SEPOLIA));
        let start = p.sub_call_inputs().len();
        apply(&mut p, &pause_on(SEPOLIA)).unwrap();
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent.len(), 1);
        assert_eq!(remote_calls(&sent[0].1), (SEPOLIA, vec![(bridge(), pause)]));
    }

    #[test]
    fn add_hyperlane_chain_enrolls_the_ica_router_once_then_adds_the_domain() {
        let add = format!(
            r#"{{"operation":"addHyperlaneChain","chainId":{ARBITRUM},"icaRouter":"{:#x}","ism":"{:#x}","hook":"{:#x}"}}"#,
            v(0xe1),
            v(0xd1),
            hook(9)
        );
        let enrolled = |p: &mut HashMapStorageProvider, router: B256| {
            p.stub_sub_call_at_selector(
                ica_router(),
                crate::sol_ext::IHyperlaneRouter::routersCall::SELECTOR,
                Bytes::from(router.abi_encode()),
            );
        };
        let enroll = Bytes::from(
            crate::sol_ext::IHyperlaneRouter::enrollRemoteRouterCall {
                _domain: ARBITRUM,
                _router: v(0xe1).into_word(),
            }
            .abi_encode(),
        );

        // A new chain: enroll, then accept the remote ISM through the ICA.
        let mut p = initialized_provider();
        enrolled(&mut p, B256::ZERO);
        approve(&mut p, &add);
        let start = p.sub_call_inputs().len();
        apply(&mut p, &add).unwrap();
        let sent = sub_calls_from(&p, start);
        assert_eq!(sent[0], (ica_router(), enroll));
        assert_eq!(remote_calls(&sent[1].1).0, ARBITRUM);
        StorageHandle::enter(&mut p, |storage| {
            let c = HyperlaneControllerContract::new(storage);
            assert_eq!(c.ism_by_domain.read(&ARBITRUM).unwrap(), v(0xd1));
        });

        // The deployer enrolled the same router: no second enrollment.
        let mut p = initialized_provider();
        enrolled(&mut p, v(0xe1).into_word());
        approve(&mut p, &add);
        let start = p.sub_call_inputs().len();
        apply(&mut p, &add).unwrap();
        assert_eq!(sub_calls_from(&p, start).len(), 1);

        // Another router is enrolled: nothing applies.
        let mut p = initialized_provider();
        enrolled(&mut p, v(0xe2).into_word());
        approve(&mut p, &add);
        let err = apply(&mut p, &add).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::RemoteRouterMismatch::SELECTOR
        );
    }

    #[test]
    fn apply_rejects_another_payload_and_drops_an_expired_action() {
        let mut p = initialized_provider();
        let payload = pause_on(LOCAL);
        approve(&mut p, &payload);

        let err = apply(&mut p, &pause_on(SEPOLIA)).unwrap_err();
        assert_eq!(
            revert_selector(err),
            IHyperlaneController::ApprovedPayloadMismatch::SELECTOR
        );

        p.set_block_number(
            APPROVED_AT + outbe_chain_constants::get_governance_voting_window_blocks() + 1,
        );
        let start = p.sub_call_inputs().len();
        apply(&mut p, &payload).unwrap();
        assert!(sub_calls_from(&p, start).is_empty());
        assert!(apply(&mut p, &payload).is_err());
    }
}
