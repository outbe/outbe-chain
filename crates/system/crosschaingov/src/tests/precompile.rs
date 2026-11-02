//! The selectors of the precompile and its owner calls.

use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolError, SolEvent};
use outbe_hyperlanecontroller::sol_ext::{IInterchainAccountRouter, IStorageMultisigIsm};
use outbe_hyperlanecontroller::test_support::*;
use outbe_hyperlanecontroller::{Owner, RemoteCall};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::storage::StorageHandle;

use super::{dispatch, remote_calls, sub_calls_from};
use crate::precompile::ICrosschainGov;
use crate::schema::CrosschainGov;

fn accept_ownership() -> Bytes {
    IStorageMultisigIsm::acceptOwnershipCall {}
        .abi_encode()
        .into()
}

#[test]
fn initialize_runs_the_ism_calls_from_the_precompile() {
    let mut p = provider();
    stub_pre_initialize(&mut p);
    let start = p.sub_call_inputs().len();
    StorageHandle::enter(&mut p, |storage| {
        dispatch(storage, &initialize_call(), deployer()).unwrap();
    });
    // The local ISM accepts on Outbe. Each remote ISM accepts through one
    // `callRemote` of the ICA router.
    let sent = sub_calls_from(&p, start);
    assert_eq!(sent[0], (local_ism(), accept_ownership()));
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[1].0, ica_router());
    assert_eq!(
        remote_calls(&sent[1].1),
        (SEPOLIA, vec![(sepolia_ism(), accept_ownership())])
    );
    assert_eq!(
        remote_calls(&sent[2].1),
        (BSC, vec![(bsc_ism(), accept_ownership())])
    );
    let dispatched: Vec<u32> = p
        .get_events(CROSSCHAIN_GOV_ADDRESS)
        .iter()
        .filter(|log| log.topics()[0] == ICrosschainGov::RemoteCallDispatched::SIGNATURE_HASH)
        .map(|log| {
            ICrosschainGov::RemoteCallDispatched::decode_log_data(log)
                .unwrap()
                .domain
        })
        .collect();
    assert_eq!(dispatched, vec![SEPOLIA, BSC]);
    assert_eq!(
        *topics(&p).last().unwrap(),
        ICrosschainGov::Initialized::SIGNATURE_HASH
    );
}

#[test]
fn fund_is_the_only_payable_selector() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let fund: Bytes = ICrosschainGov::fundCall {}.abi_encode().into();
        crate::precompile::dispatch(storage.clone(), &fund, stranger(), U256::from(5)).unwrap();
        assert!(dispatch(storage.clone(), &fund, stranger()).is_err());
        let view: Bytes = ICrosschainGov::icaRouterCall {}.abi_encode().into();
        assert!(crate::precompile::dispatch(storage, &view, stranger(), U256::from(1)).is_err());
    });
    assert_eq!(
        *topics(&p).last().unwrap(),
        ICrosschainGov::Funded::SIGNATURE_HASH
    );
}

#[test]
fn hyperlane_views_go_to_the_hyperlane_controller() {
    let mut p = initialized_provider();
    StorageHandle::enter(&mut p, |storage| {
        let call = ICrosschainGov::icaRouterCall {};
        let ret = dispatch(storage.clone(), &call.abi_encode(), stranger()).unwrap();
        assert_eq!(
            ICrosschainGov::icaRouterCall::abi_decode_returns(&ret).unwrap(),
            ica_router()
        );
        let call = ICrosschainGov::domainsCall {};
        let ret = dispatch(storage, &call.abi_encode(), stranger()).unwrap();
        assert_eq!(
            ICrosschainGov::domainsCall::abi_decode_returns(&ret).unwrap(),
            vec![LOCAL, SEPOLIA, BSC]
        );
    });
}

#[test]
fn sync_pays_each_remote_ism_message_from_the_precompile_balance() {
    let mut p = initialized_provider();
    let fee = U256::from(7);
    stub_router_and_ism(&mut p, fee, (&[v(1), v(2)], 1));
    p.set_balance(CROSSCHAIN_GOV_ADDRESS, fee * U256::from(2));
    p.clear_events(CROSSCHAIN_GOV_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let mut gov = CrosschainGov::new(storage.clone());
        let mut c = outbe_hyperlanecontroller::HyperlaneControllerContract::new(storage);
        c.set_validators_and_threshold(&[v(1), v(2), v(3)], 2, &mut gov)
            .unwrap();
    });
    assert_eq!(
        topics(&p),
        vec![
            ICrosschainGov::RemoteCallDispatched::SIGNATURE_HASH,
            ICrosschainGov::RemoteCallDispatched::SIGNATURE_HASH,
            ICrosschainGov::LocalCallExecuted::SIGNATURE_HASH,
            ICrosschainGov::ValidatorsAndThresholdApplied::SIGNATURE_HASH,
        ]
    );
    let dispatched = ICrosschainGov::RemoteCallDispatched::decode_log_data(
        &p.get_events(CROSSCHAIN_GOV_ADDRESS)[0],
    )
    .unwrap();
    assert_eq!(dispatched.domain, SEPOLIA);
    assert_eq!(dispatched.fee, fee);
}

#[test]
fn sync_without_fee_balance_changes_nothing() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::from(7), (&[v(1)], 1));
    p.clear_events(CROSSCHAIN_GOV_ADDRESS);
    StorageHandle::enter(&mut p, |storage| {
        let mut gov = CrosschainGov::new(storage.clone());
        let mut c = outbe_hyperlanecontroller::HyperlaneControllerContract::new(storage);
        let err = c
            .set_validators_and_threshold(&[v(1), v(2)], 2, &mut gov)
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::InsufficientBalance::SELECTOR
        );
    });
    assert!(topics(&p).is_empty());
}

#[test]
fn remote_and_local_owner_calls() {
    let mut p = initialized_provider();
    stub_router_and_ism(&mut p, U256::ZERO, (&[v(1)], 1));
    StorageHandle::enter(&mut p, |storage| {
        let mut gov = CrosschainGov::new(storage);
        let calls = [RemoteCall::new(sepolia_ism(), accept_ownership())];
        assert_eq!(
            gov.call_remote(SEPOLIA, &calls).unwrap(),
            B256::repeat_byte(0xaa)
        );
        assert!(gov.call_remote(LOCAL, &calls).is_err());
        assert!(gov.call_remote(0, &calls).is_err());
        assert_eq!(
            revert_selector(gov.call_remote(SEPOLIA, &[]).unwrap_err()),
            ICrosschainGov::EmptyCalls::SELECTOR
        );

        gov.call_local(local_ism(), U256::ZERO, Bytes::from_static(&[1, 2, 3, 4]))
            .unwrap();
        assert!(gov
            .call_local(local_ism(), U256::from(1), Bytes::new())
            .is_err());
        assert!(gov
            .call_local(alloy_primitives::Address::ZERO, U256::ZERO, Bytes::new())
            .is_err());
    });
}

#[test]
fn call_remote_requires_an_initialized_controller() {
    let mut p = provider();
    p.enable_sub_call_stub();
    StorageHandle::enter(&mut p, |storage| {
        let mut gov = CrosschainGov::new(storage);
        let err = gov
            .call_remote(SEPOLIA, &[RemoteCall::new(v(1), Bytes::new())])
            .unwrap_err();
        assert_eq!(
            revert_selector(err),
            ICrosschainGov::NotInitialized::SELECTOR
        );
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
        let call = ICrosschainGov::remoteAccountCall { domain: SEPOLIA };
        let ret = dispatch(storage.clone(), &call.abi_encode(), stranger()).unwrap();
        let account = ICrosschainGov::remoteAccountCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(account, v(0x1c));
        let call = ICrosschainGov::remoteAccountCall { domain: LOCAL };
        assert!(dispatch(storage, &call.abi_encode(), stranger()).is_err());
    });
}
