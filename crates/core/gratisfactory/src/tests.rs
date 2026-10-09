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
use outbe_tee_enclave::gratis::{decrypt_balance, decrypt_pledged, derive_view_key};

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
    test_enclave::modify_auth(outbe_tee_enclave::gratis::ModifyOperation {
        account: owner,
        op,
        amount,
        op_nonce,
        chain_id: chain_b256(),
    })
}

fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

/// Give `account` a positive Fidelity index so `pledge_gratis` clears the
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
            IGratisFactory::IGratisFactoryCalls::pledgeGratis(IGratisFactory::pledgeGratisCall {
                reservationId: U256::from(1u64),
                auth: IGratisFactory::ModifyAuth {
                    mac: FixedBytes([0u8; 32]),
                    opNonce: 0,
                },
            })
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
    runtime::pledge_gratis(
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
        assert!(runtime::pledge_gratis(storage.clone(), alice(), id, wrong_amount).is_err());
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
        let err = runtime::cancel_pledge(storage.clone(), bob(), id).unwrap_err();
        assert!(
            err.to_string().contains("not the reservation source"),
            "{err}"
        );
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));

        runtime::cancel_pledge(storage.clone(), alice(), id).unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::from(1_000u64));
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert!(runtime::pledge_of(&storage, id).unwrap().source.is_zero());
        let err = runtime::cancel_pledge(storage.clone(), alice(), id).unwrap_err();
        assert!(err.to_string().contains("pledge not found"), "{err}");
    });
}

#[test]
fn credis_takes_a_pledge_once_and_cancel_then_fails() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), id, 1).unwrap();
        let record = U256::from(7u64);
        let err =
            runtime::send_to_credis(&storage, id, record, bob(), U256::from(RESERVED)).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        let err = runtime::send_to_credis(&storage, id, record, alice(), U256::ONE).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");

        runtime::send_to_credis(&storage, id, record, alice(), U256::from(RESERVED)).unwrap();
        let collateral = runtime::collateral_of(&storage, record).unwrap();
        assert_eq!(
            (collateral.source, collateral.remaining_minor),
            (alice(), U256::from(RESERVED))
        );
        assert_eq!(view_pledged(&storage, alice()), U256::from(RESERVED));
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::from(RESERVED)
        );
        let err = runtime::send_to_credis(&storage, id, record, alice(), U256::from(RESERVED))
            .unwrap_err();
        assert!(err.to_string().contains("pledge not found"), "{err}");
        let err = runtime::cancel_pledge(storage.clone(), alice(), id).unwrap_err();
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
        runtime::cancel_pledge(storage.clone(), alice(), id).unwrap();
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

/// Adds another reservation of `RESERVED` Gratis for `source` and pledges it.
fn reserve_and_pledge(storage: &StorageHandle<'_>, source: Address, id: u64, nonce: u64) -> U256 {
    let mut reservation = outbe_vaultrouter::api::reservation_of(storage, U256::ONE).unwrap();
    reservation.id = U256::from(id);
    outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
        .reservations
        .create(&reservation)
        .unwrap();
    pledge(storage, source, reservation.id, nonce).unwrap();
    reservation.id
}

#[test]
fn collateral_is_drawn_per_credis_and_never_reopens() {
    with_env(|storage| {
        let first = fund_and_reserve(&storage, alice());
        pledge(&storage, alice(), first, 1).unwrap();
        let second = reserve_and_pledge(&storage, alice(), 2, 2);
        let (a, b) = (U256::from(7u64), U256::from(8u64));
        let reserved = U256::from(RESERVED);
        runtime::send_to_credis(&storage, first, a, alice(), reserved).unwrap();
        runtime::send_to_credis(&storage, second, b, alice(), reserved).unwrap();
        assert_eq!(view_pledged(&storage, alice()), reserved * U256::from(2u64));

        // The source's pledged total would cover it, but Credis A only holds RESERVED.
        let err = runtime::return_from_credis(&storage, a, reserved + U256::ONE).unwrap_err();
        assert!(
            err.to_string().contains("exceeds the Credis's collateral"),
            "{err}"
        );
        let err = runtime::burn_from_credis(&storage, a, reserved + U256::ONE).unwrap_err();
        assert!(
            err.to_string().contains("exceeds the Credis's collateral"),
            "{err}"
        );
        assert_eq!(view_pledged(&storage, alice()), reserved * U256::from(2u64));
        assert_eq!(
            runtime::collateral_of(&storage, a).unwrap().remaining_minor,
            reserved
        );

        let returned = U256::from(40u64);
        runtime::return_from_credis(&storage, a, returned).unwrap();
        assert_eq!(
            runtime::collateral_of(&storage, a).unwrap().remaining_minor,
            reserved - returned
        );
        assert_eq!(
            view_balance(&storage, alice()),
            U256::from(1_000 - 2 * RESERVED) + returned
        );
        runtime::burn_from_credis(&storage, a, reserved - returned).unwrap();
        assert!(runtime::collateral_of(&storage, a)
            .unwrap()
            .source
            .is_zero());
        assert_eq!(view_pledged(&storage, alice()), reserved);
        assert_eq!(
            view_balance(&storage, alice()) + view_pledged(&storage, alice()),
            U256::from(1_000u64) - (reserved - returned)
        );
        let err = runtime::return_from_credis(&storage, a, U256::ONE).unwrap_err();
        assert!(err.to_string().contains("collateral not found"), "{err}");
        assert_eq!(
            runtime::collateral_of(&storage, b).unwrap().remaining_minor,
            reserved
        );

        let third = reserve_and_pledge(&storage, alice(), 3, 3);
        let err = runtime::send_to_credis(&storage, third, b, alice(), reserved).unwrap_err();
        assert!(err.to_string().contains("already has collateral"), "{err}");
        assert_eq!(runtime::pledge_of(&storage, third).unwrap().source, alice());
    });
}

#[test]
fn encrypted_nod_mint_preserves_nonce_and_combined_fidelity_state() {
    with_env(|storage| {
        use outbe_primitives::{
            nod_encryption::NodTermsV2, time::WorldwideDay, wwd_entity_id::WwdEntityId,
        };
        let day = WorldwideDay::new(20250115);
        let amount = U256::from(123);
        let nod = outbe_tee_enclave::nod_encryption::encrypt_nod(
            &[0x5a; 32],
            &[9; 32],
            NodTermsV2 {
                chain_id: CHAIN_ID,
                nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(7)),
                owner: alice(),
                worldwide_day: day,
                league_id: 0,
                entry_price_minor: U256::ONE,
                issuance_currency: 840,
                reference_currency: 978,
            },
            amount,
        )
        .unwrap();
        let authorization = auth(GratisOp::Mint, alice(), amount, 0);
        crate::api::mint_encrypted_nod(storage.clone(), &nod, authorization.clone()).unwrap();
        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::op_nonce(storage.clone(), alice()).unwrap(),
            1
        );
        let fidelity = outbe_fidelity::FidelityContract::new(storage.clone());
        let cohorts = fidelity.cohorts_ct_of(alice()).unwrap();
        assert!(!cohorts.is_empty());
        let balance = crate::api::encrypted_balance(storage.clone(), alice()).unwrap();
        assert!(crate::api::mint_encrypted_nod(storage.clone(), &nod, authorization).is_err());
        assert_eq!(
            crate::api::encrypted_balance(storage.clone(), alice()).unwrap(),
            balance
        );
        assert_eq!(fidelity.cohorts_ct_of(alice()).unwrap(), cohorts);
        assert_eq!(outbe_gratis::api::op_nonce(storage, alice()).unwrap(), 1);
    });
}
