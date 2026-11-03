//! Unit tests for the CrosschainBridge precompile.
//!
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs. `stub_sub_call_at_selector` pins a target's reply per
//! selector. `enable_sub_call_stub()` makes every other sub-call succeed
//! with empty returndata. The Hyperlane registry comes from the
//! HyperlaneController precompile (`hub_dispatch`).

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent, SolValue};
use outbe_hyperlanecontroller::precompile::{dispatch as hub_dispatch, IHyperlaneController};
use outbe_hyperlanecontroller::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use outbe_hyperlanecontroller::HyperlaneControllerContract;

use crate::precompile::ICrosschainBridge::Call;
use outbe_primitives::addresses::{CROSSCHAIN_BRIDGE_ADDRESS, HYPERLANE_CONTROLLER_ADDRESS};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::{dispatch, ICrosschainBridge};
use crate::schema::CrosschainBridgeContract;

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
        word_addr(CROSSCHAIN_BRIDGE_ADDRESS),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(CROSSCHAIN_BRIDGE_ADDRESS),
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
        hub_dispatch(storage, &initialize_call(), deployer(), U256::ZERO).unwrap();
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
    p.get_events(CROSSCHAIN_BRIDGE_ADDRESS)
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
fn fund_is_the_only_payable_selector() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = ICrosschainBridge::fundCall {}.abi_encode().into();
        dispatch(storage.clone(), &fund, stranger(), U256::from(5)).unwrap();
        assert!(dispatch(storage.clone(), &fund, stranger(), U256::ZERO).is_err());
        let view: Bytes = ICrosschainBridge::remoteAccountCall { domain: SEPOLIA }
            .abi_encode()
            .into();
        assert!(dispatch(storage, &view, stranger(), U256::from(1)).is_err());
    });
    assert_eq!(
        *topics(&p).last().unwrap(),
        ICrosschainBridge::Funded::SIGNATURE_HASH
    );
}

#[test]
fn a_remote_call_without_fee_balance_changes_nothing() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::from(7), &[v(1)], 1);
    p.clear_events(CROSSCHAIN_BRIDGE_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let err = CrosschainBridgeContract::new(storage)
            .call_remote(
                SEPOLIA,
                &[Call {
                    to: sepolia_ism(),
                    value: U256::ZERO,
                    data: Bytes::new(),
                }],
            )
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainBridge::InsufficientBalance::SELECTOR
        );
    });
    assert!(topics(&p).is_empty());
}

#[test]
fn remote_and_local_generic_calls() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::ZERO, &[v(1)], 1);
    StorageHandle::enter(&mut p, |storage| {
        let mut gov = CrosschainBridgeContract::new(storage);
        let calls = [Call {
            to: sepolia_ism(),
            value: U256::ZERO,
            data: IStorageMultisigIsm::acceptOwnershipCall {}
                .abi_encode()
                .into(),
        }];
        assert_eq!(
            gov.call_remote(SEPOLIA, &calls).unwrap(),
            B256::repeat_byte(0xaa)
        );
        assert!(gov.call_remote(LOCAL, &calls).is_err());
        assert!(gov.call_remote(SEPOLIA, &[]).is_err());

        gov.call_local(local_ism(), U256::ZERO, Bytes::from_static(&[1, 2, 3, 4]))
            .unwrap();
        assert!(gov
            .call_local(local_ism(), U256::from(1), Bytes::new())
            .is_err());
    });
}

#[test]
fn only_an_allowed_caller_may_call_local_or_remote() {
    let mut p = initialized_provider();
    let start = p.sub_call_inputs().len();
    let call: Bytes = ICrosschainBridge::callLocalCall {
        call: ICrosschainBridge::Call {
            to: local_ism(),
            value: U256::ZERO,
            data: Bytes::from_static(&[1, 2, 3, 4]),
        },
    }
    .abi_encode()
    .into();
    StorageHandle::enter(&mut p, |storage| {
        let err = dispatch(storage.clone(), &call, stranger(), U256::ZERO).unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainBridge::CallerNotAllowed::SELECTOR
        );
        dispatch(storage, &call, HYPERLANE_CONTROLLER_ADDRESS, U256::ZERO).unwrap();
    });
    let sent: Vec<_> = p.sub_call_inputs()[start..]
        .iter()
        .filter(|input| !input.is_static)
        .map(|input| (input.target, input.calldata.clone()))
        .collect();
    assert_eq!(sent, [(local_ism(), Bytes::from_static(&[1, 2, 3, 4]))]);
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
        let call = ICrosschainBridge::remoteAccountCall { domain: SEPOLIA };
        let ret = dispatch(storage, &call.abi_encode(), stranger(), U256::ZERO).unwrap();
        let account = ICrosschainBridge::remoteAccountCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(account, v(0x1c));
    });
}
