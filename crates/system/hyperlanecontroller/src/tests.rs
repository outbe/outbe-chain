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

fn initialize_call() -> Bytes {
    IHyperlaneController::initializeCall {
        router: router(),
        domains: vec![LOCAL, SEPOLIA, BSC],
        isms: vec![local_ism(), sepolia_ism(), bsc_ism()],
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
        c.add_domain(1, v(5)).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, SEPOLIA, BSC, 1]);
        c.add_domain(1, v(6)).unwrap();
        assert_eq!(c.ism_by_domain.read(&1).unwrap(), v(6));
        assert_eq!(c.domains.read_all().unwrap().len(), 4);

        c.remove_domain(SEPOLIA).unwrap();
        assert_eq!(c.domains.read_all().unwrap(), vec![LOCAL, BSC, 1]);
        assert_eq!(c.ism_by_domain.read(&SEPOLIA).unwrap(), Address::ZERO);

        assert!(c.remove_domain(SEPOLIA).is_err());
        assert!(c.remove_domain(LOCAL).is_err());
        assert!(c.add_domain(LOCAL, v(7)).is_err());
        assert!(c.add_domain(0, v(7)).is_err());
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
        assert!(c.add_domain(SEPOLIA, v(1)).is_err());
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
