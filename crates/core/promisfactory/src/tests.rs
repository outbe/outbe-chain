use alloy_primitives::{address, Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::Promis;

use crate::precompile::{dispatch, IPromisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

fn dispatch_call_bytes(call: IPromisFactory::IPromisFactoryCalls) -> Bytes {
    Bytes::from(call.abi_encode())
}

#[test]
fn mine_rejects_zero_amount() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err = runtime::mint(storage, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("amount must be positive"));
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        // Alice holds 100 promis but tries to convert 200.
        Promis::new(storage.clone())
            .mint(alice(), U256::from(100u64))
            .unwrap();

        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::mineCoen(
            IPromisFactory::mineCoenCall {
                amount: U256::from(200u64),
            },
        ));
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("insufficient balance"));

        // No native COEN minted, promis untouched (atomic revert).
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(
            Promis::new(storage.clone()).balance_of(alice()).unwrap(),
            U256::from(100u64)
        );
    });
}

/// mine_coen with insufficient balance must fail without partial burn.
#[test]
fn mine_coen_failure_no_partial_burn() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        Promis::new(storage.clone())
            .mint(alice(), U256::from(100u64))
            .unwrap();

        let balance_before = Promis::new(storage.clone()).balance_of(alice()).unwrap();
        let supply_before = Promis::new(storage.clone()).total_supply().unwrap();

        let result = runtime::mine_coen(storage.clone(), alice(), U256::from(200u64));
        assert!(
            result.is_err(),
            "mine_coen with insufficient balance must fail"
        );

        // No partial burn: balance and supply unchanged; no native minted.
        assert_eq!(
            Promis::new(storage.clone()).balance_of(alice()).unwrap(),
            balance_before
        );
        assert_eq!(
            Promis::new(storage.clone()).total_supply().unwrap(),
            supply_before
        );
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
    });
}

#[test]
fn supports_interface() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::supportsInterface(
            IPromisFactory::supportsInterfaceCall {
                interfaceId: alloy_primitives::FixedBytes(ERC165_INTERFACE_ID),
            },
        ));
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IPromisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::supportsInterface(
            IPromisFactory::supportsInterfaceCall {
                interfaceId: alloy_primitives::FixedBytes([0xde, 0xad, 0xbe, 0xef]),
            },
        ));
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IPromisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::mineCoen(
            IPromisFactory::mineCoenCall {
                amount: U256::from(1u64),
            },
        ));
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"));
    });
}
