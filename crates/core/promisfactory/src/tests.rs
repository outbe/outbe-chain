use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_gratis::enclave_client::test_enclave;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promis::Promis;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use crate::precompile::{dispatch, IPromisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;
const ONE_YEAR_SECS: u64 = 365 * 86_400;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

/// Build the modify authorization a client holding `owner`'s gratis modify key
/// sends for `op` on `amount` at `op_nonce` (mirrors the gratisfactory test
/// helper). Requires the in-process enclave to be installed.
fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    let mk = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

/// Decrypt `account`'s confidential gratis balance the way a client would.
fn view_gratis_balance(storage: &StorageHandle<'_>, account: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), account).unwrap();
    let blob = outbe_gratis::api::balance_ct(storage.clone(), account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, account, &blob).unwrap()
}

fn dispatch_call_bytes(call: IPromisFactory::IPromisFactoryCalls) -> Bytes {
    Bytes::from(call.abi_encode())
}

#[test]
fn mine_mints_promis_and_records_fidelity_cohort() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let amount = U256::from(1_000u64);
        // No cohort yet: RCFI a year out is zero. Asserting this up front is what
        // makes the post-mine `> 0` check prove `mine` recorded the cohort
        // (rather than it having pre-existed).
        let later = CREATED_AT + ONE_YEAR_SECS;
        let rcfi_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .compute_fidelity_index(alice(), later)
            .unwrap();
        assert_eq!(rcfi_before, U256::ZERO);

        runtime::mint(storage.clone(), alice(), amount).unwrap();

        // Promis minted to the recipient and into total supply.
        let promis = Promis::new(storage.clone());
        assert_eq!(promis.balance_of(alice()).unwrap(), amount);
        assert_eq!(promis.total_supply().unwrap(), amount);

        // The acquisition cohort was recorded at the current block time, so the
        // aged RCFI a year later is now positive. If `mine` stopped calling
        // `cohort_in`, this would stay zero and fail.
        let rcfi_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .compute_fidelity_index(alice(), later)
            .unwrap();
        assert!(rcfi_after > U256::ZERO);
    });
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
fn mine_coen_burns_promis_mints_native_and_records_sale_cohort() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let amount = U256::from(1_000u64);

        // Seed promis to burn plus an active Fidelity cohort of the SAME size
        // acquired a year ago, so it has positive RCFI now and is fully
        // consumed by the sale.
        Promis::new(storage.clone()).mint(alice(), amount).unwrap();
        outbe_fidelity::api::cohort_in(
            storage.clone(),
            alice(),
            amount,
            CREATED_AT - ONE_YEAR_SECS,
        )
        .unwrap();
        let rcfi_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert!(rcfi_before > U256::ZERO);

        // mineCoen on the promisfactory precompile.
        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::mineCoen(
            IPromisFactory::mineCoenCall { amount },
        ));
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IPromisFactory::mineCoenCall::abi_decode_returns(&out).unwrap();
        assert_eq!(minted, amount);

        // Promis fully burned; native COEN minted 1:1 to the seller.
        let promis = Promis::new(storage.clone());
        assert_eq!(promis.balance_of(alice()).unwrap(), U256::ZERO);
        assert_eq!(promis.total_supply().unwrap(), U256::ZERO);
        assert_eq!(storage.balance(alice()).unwrap(), amount);

        // The active cohort was fully sold via cohort_out, so RCFI is now zero.
        // If the sale hook were dropped, this would stay positive and fail.
        let rcfi_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert_eq!(rcfi_after, U256::ZERO);
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
fn convert_to_gratis_burns_promis_mints_gratis_preserving_fidelity() {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let amount = U256::from(1_000u64);

        // Seed promis to convert plus an active Fidelity cohort of the SAME size
        // acquired a year ago, so it has positive RCFI now. Converting to gratis
        // must leave this cohort untouched (aging preserved).
        Promis::new(storage.clone()).mint(alice(), amount).unwrap();
        outbe_fidelity::api::cohort_in(
            storage.clone(),
            alice(),
            amount,
            CREATED_AT - ONE_YEAR_SECS,
        )
        .unwrap();
        let rcfi_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert!(rcfi_before > U256::ZERO);

        // convertToGratis on the promisfactory precompile. The confidential gratis
        // mint is authorized by alice's modify key at her current op-nonce (0).
        let a = auth(GratisOp::Mine, alice(), amount, 0);
        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::convertToGratis(
            IPromisFactory::convertToGratisCall {
                amount,
                mac: FixedBytes(a.mac),
                opNonce: a.op_nonce,
            },
        ));
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IPromisFactory::convertToGratisCall::abi_decode_returns(&out).unwrap();
        assert_eq!(minted, amount);

        // Promis fully burned; gratis minted 1:1 to the account (decrypt the
        // confidential balance to check; total supply is public).
        let promis = Promis::new(storage.clone());
        assert_eq!(promis.balance_of(alice()).unwrap(), U256::ZERO);
        assert_eq!(promis.total_supply().unwrap(), U256::ZERO);
        assert_eq!(view_gratis_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            amount
        );

        // Fidelity untouched: no cohort_out (promis burn) and no cohort_in (gratis
        // mint), so RCFI is unchanged. If either hook crept in, this would move.
        let rcfi_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .get_fidelity_index(alice())
            .unwrap();
        assert_eq!(rcfi_after, rcfi_before);
    });
    test_enclave::uninstall();
}

/// convert_to_gratis with insufficient balance must fail with no partial state:
/// no promis burned, no gratis minted (atomic revert).
#[test]
fn convert_to_gratis_rejects_insufficient_balance() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        // Alice holds 100 promis but tries to convert 200.
        Promis::new(storage.clone())
            .mint(alice(), U256::from(100u64))
            .unwrap();

        // The promis burn fails before the gratis mint is reached, so the auth is
        // never checked — a zero placeholder is fine here.
        let call = dispatch_call_bytes(IPromisFactory::IPromisFactoryCalls::convertToGratis(
            IPromisFactory::convertToGratisCall {
                amount: U256::from(200u64),
                mac: FixedBytes([0u8; 32]),
                opNonce: 0,
            },
        ));
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("insufficient balance"));

        // No gratis minted (no ciphertext ever written), promis untouched.
        assert_eq!(
            Promis::new(storage.clone()).balance_of(alice()).unwrap(),
            U256::from(100u64)
        );
        assert!(outbe_gratis::api::balance_ct(storage.clone(), alice())
            .unwrap()
            .is_empty());
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
