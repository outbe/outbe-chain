//! Confidential gratisfactory tests driven by the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`). The tests decrypt the ciphertext with
//! the account's view key, exactly as a client would, to assert balances. Writes carry a
//! `ModifyAuth` bound to the account's op-nonce.

use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_gratis::enclave_client::test_enclave;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
};

use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

/// Build the modify authorization a client holding `owner`'s modify key sends for
/// `op` on `amount` at `op_nonce`.
fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    let mk = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

/// Give `account` a positive Fidelity index so `create_pledge_note` clears the
/// eligibility gate.
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

fn test_storage() -> HashMapStorageProvider {
    test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage
}

fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut storage = test_storage();
    let out = StorageHandle::enter(&mut storage, f);
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
    out
}

#[test]
fn mine_mints_gratis_and_records_fidelity_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        let later = CREATED_AT + ONE_YEAR_SECS;
        // No cohort yet: no account has qualified, so the league is the floor.
        let league_before =
            outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_before, MIN_LEAGUE);

        runtime::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            amount
        );

        // The acquisition cohort was recorded: sole holder, no sales -> top league.
        let league_after = outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_after, MAX_LEAGUE);
    });
}

#[test]
fn mine_rejects_zero_amount() {
    with_env(|storage| {
        let err = runtime::mint(
            storage.clone(),
            alice(),
            U256::ZERO,
            auth(GratisOp::Mint, alice(), U256::ZERO, 0),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("amount and account must be nonzero"),
            "got: {err}"
        );
    });
}

#[test]
fn mine_coen_burns_gratis_mints_native_and_records_sale_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        outbe_fidelity::api::cohort_in(
            storage.clone(),
            alice(),
            amount,
            CREATED_AT - ONE_YEAR_SECS,
        )
        .unwrap();
        let league_before = outbe_fidelity::api::league(storage.clone(), alice()).unwrap();
        assert_eq!(league_before, MAX_LEAGUE);

        // mineCoen burns gratis (op = Burn) at op-nonce 1.
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IGratisFactory::mineCoenCall::abi_decode_returns(&out).unwrap();
        let native_amount = checked_protocol_to_native(amount).unwrap();
        assert_eq!(minted, native_amount);

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(storage.balance(alice()).unwrap(), native_amount);

        // Fully sold -> efficiency 0 -> league drops to the floor.
        let league_after = outbe_fidelity::api::league(storage.clone(), alice()).unwrap();
        assert_eq!(league_after, MIN_LEAGUE);
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    with_env(|storage| {
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(GratisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();

        let amount = U256::from(200u64);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("insufficient balance"),
            "got: {err}"
        );

        // Atomic revert: no COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), U256::from(100u64));
    });
}

#[test]
fn supports_interface() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes(ERC165_INTERFACE_ID),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes([0xde, 0xad, 0xbe, 0xef]),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::createPledgeNote(
                IGratisFactory::createPledgeNoteCall {
                    reservationId: U256::from(1u64),
                    auth: IGratisFactory::ModifyAuth {
                        mac: FixedBytes([0u8; 32]),
                        opNonce: 0,
                    },
                },
            )
            .abi_encode(),
        );
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"), "got: {err}");
    });
}

fn bob() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}

const RESERVED: u64 = 100;

fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

/// Mint 1000 Gratis to `source` and reserve `RESERVED` of it for reservation 1.
fn fund_and_reserve(storage: &StorageHandle<'_>, source: Address) -> U256 {
    let minted = U256::from(1_000u64);
    runtime::mint(
        storage.clone(),
        source,
        minted,
        auth(GratisOp::Mint, source, minted, 0),
    )
    .unwrap();
    seed_fidelity(storage.clone(), source);
    let id = U256::ONE;
    outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
        .reservations
        .create(&outbe_vaultrouter::schema::LiquidityReservation {
            id,
            asset: address!("0x0000000000000000000000000000000000000888"),
            amount: U256::from(2 * RESERVED),
            gratis_minor: U256::from(RESERVED),
            expires_at: CREATED_AT + 900,
            source,
            ..Default::default()
        })
        .unwrap();
    id
}

fn pledge(
    storage: &StorageHandle<'_>,
    caller: Address,
    id: U256,
    nonce: u64,
) -> Result<(), String> {
    runtime::create_pledge_note(
        storage.clone(),
        caller,
        id,
        auth(GratisOp::Pledge, caller, U256::from(RESERVED), nonce),
    )
    .map_err(|e| e.to_string())
}

#[test]
fn only_the_source_pledges_exactly_the_reserved_gratis_once() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        let fidelity = outbe_fidelity::schema::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        let err = pledge(&storage, bob(), id, 0).unwrap_err();
        assert!(err.contains("not the reservation source"), "{err}");
        let err = pledge(&storage, alice(), U256::from(2u64), 1).unwrap_err();
        assert!(err.contains("reservation not found"), "{err}");
        let wrong_amount = auth(GratisOp::Pledge, alice(), U256::from(RESERVED - 1), 1);
        assert!(runtime::create_pledge_note(storage.clone(), alice(), id, wrong_amount).is_err());
        assert_eq!(view_balance(&storage, alice()), U256::from(1_000u64));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::op_nonce(storage.clone(), alice()).unwrap(),
            1
        );
        assert!(runtime::pledge_of(&storage, id).unwrap().source.is_zero());

        pledge(&storage, alice(), id, 1).unwrap();
        assert_eq!(
            view_balance(&storage, alice()),
            U256::from(1_000 - RESERVED)
        );
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::from(RESERVED)
        );
        assert_eq!(
            outbe_fidelity::schema::FidelityContract::new(storage.clone())
                .cohorts_ct_of(alice())
                .unwrap(),
            fidelity
        );
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::pledgeOf(IGratisFactory::pledgeOfCall {
                reservationId: id,
            })
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, bob(), U256::ZERO).unwrap();
        let record = IGratisFactory::pledgeOfCall::abi_decode_returns(&out).unwrap();
        assert_eq!(
            (record.source, record.gratisMinor),
            (alice(), U256::from(RESERVED))
        );

        let err = pledge(&storage, alice(), id, 2).unwrap_err();
        assert!(err.contains("already pledged"), "{err}");
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
    });
}

#[test]
fn an_expired_reservation_cannot_be_pledged() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        storage
            .set_block_timestamp(U256::from(CREATED_AT + 901))
            .unwrap();
        let err = pledge(&storage, alice(), id, 1).unwrap_err();
        assert!(err.contains("reservation expired"), "{err}");
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
}

#[test]
fn only_the_source_cancels_an_unused_pledge_even_after_expiry() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), id, 1).unwrap();
        storage
            .set_block_timestamp(U256::from(CREATED_AT + 901))
            .unwrap();
        let err = runtime::cancel_pledge_note(storage.clone(), bob(), id).unwrap_err();
        assert!(
            err.to_string().contains("not the reservation source"),
            "{err}"
        );
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));

        runtime::cancel_pledge_note(storage.clone(), alice(), id).unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::from(1_000u64));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert!(runtime::pledge_of(&storage, id).unwrap().source.is_zero());
        let err = runtime::cancel_pledge_note(storage.clone(), alice(), id).unwrap_err();
        assert!(err.to_string().contains("pledge not found"), "{err}");
    });
}

#[test]
fn credis_takes_a_pledge_once_and_cancel_then_fails() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), id, 1).unwrap();
        let position = U256::from(7u64);
        let err = runtime::send_to_credis(&storage, id, position, bob(), U256::from(RESERVED))
            .unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        let err = runtime::send_to_credis(&storage, id, position, alice(), U256::ONE).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");

        runtime::send_to_credis(&storage, id, position, alice(), U256::from(RESERVED)).unwrap();
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::from(RESERVED)
        );
        assert!(
            runtime::send_to_credis(&storage, id, position, alice(), U256::from(RESERVED)).is_err()
        );
        let err = runtime::cancel_pledge_note(storage.clone(), alice(), id).unwrap_err();
        assert!(err.to_string().contains("pledge not found"), "{err}");
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
    });
}

#[test]
fn a_pledge_is_accepted_up_to_the_reservation_expiry() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        storage
            .set_block_timestamp(U256::from(CREATED_AT + 900))
            .unwrap();
        pledge(&storage, alice(), id, 1).unwrap();
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
    });
}

#[test]
fn a_pledge_outlives_its_returned_reservation_and_can_be_cancelled() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), id, 1).unwrap();
        outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
            .reservations
            .delete(id)
            .unwrap();
        runtime::cancel_pledge_note(storage.clone(), alice(), id).unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::from(1_000u64));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
}

#[test]
fn mining_coen_cannot_spend_pledged_gratis() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), id, 1).unwrap();
        let liquid = U256::from(1_000 - RESERVED);
        let too_much = liquid + U256::ONE;
        assert!(runtime::mine_coen(
            storage.clone(),
            alice(),
            too_much,
            auth(GratisOp::Burn, alice(), too_much, 2),
        )
        .is_err());
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
        runtime::mine_coen(
            storage.clone(),
            alice(),
            liquid,
            auth(GratisOp::Burn, alice(), liquid, 2),
        )
        .unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
    });
}
