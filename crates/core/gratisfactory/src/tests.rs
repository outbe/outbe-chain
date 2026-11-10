//! Confidential gratisfactory tests driven by the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`). The tests decrypt the ciphertext with
//! the account's view key, exactly as a client would, to assert balances. Writes carry a
//! `ModifyAuth` bound to the account's op-nonce.

use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolInterface};

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_gratis::enclave_client::test_enclave;
use outbe_primitives::addresses::{GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::SECONDS_PER_DAY;
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

/// Mint gratis and drop the fidelity outcome, so no cohort is persisted.
/// `outbe_gratis::api::mint` is visible only inside that crate's own tests.
fn mint_without_cohort(storage: StorageHandle<'_>, amount: U256) {
    let now = storage.now_secs().unwrap();
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        alice(),
        outbe_fidelity::api::FidelityCohortOp::In,
        now,
    )
    .unwrap();
    outbe_gratis::api::mint_with_fidelity(
        storage,
        alice(),
        amount,
        auth(GratisOp::Mint, alice(), amount, 0),
        section,
    )
    .unwrap();
}

/// Give `account` a positive Fidelity index so `pledge_gratis` clears the
/// eligibility gate.
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
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
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratis::errors::GratisError::Rejected(
                    "amount and account must be nonzero".into(),
                )
            )
            .to_string()
        );
    });
}

#[test]
fn mine_coen_burns_gratis_mints_native_and_records_sale_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        mint_without_cohort(storage.clone(), amount);
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
        mint_without_cohort(storage.clone(), U256::from(100u64));

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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratis::errors::GratisError::Rejected(
                    "insufficient balance or balance overflow".into()
                )
            )
            .to_string()
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
            amount_minor: U256::from(2 * RESERVED),
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
) -> Result<(), outbe_primitives::error::PrecompileError> {
    runtime::pledge_gratis(
        storage.clone(),
        caller,
        id,
        auth(GratisOp::Pledge, caller, U256::from(RESERVED), nonce),
    )
}

#[test]
fn only_the_source_pledges_exactly_the_reserved_gratis_once() {
    with_env(|storage| {
        let id = fund_and_reserve(&storage, alice());
        let fidelity = outbe_fidelity::schema::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        let err = pledge(&storage, bob(), id, 0).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::NotReservationSource
            )
            .to_string()
        );
        let err = pledge(&storage, alice(), U256::from(2u64), 1).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::ReservationNotFound
            )
            .to_string()
        );
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeExists
            )
            .to_string()
        );
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::ReservationExpired
            )
            .to_string()
        );
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::NotReservationSource
            )
            .to_string()
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeNotFound
            )
            .to_string()
        );
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeMismatch
            )
            .to_string()
        );
        let err = runtime::send_to_credis(&storage, id, record, alice(), U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeMismatch
            )
            .to_string()
        );

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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeNotFound
            )
            .to_string()
        );
        let err = runtime::cancel_pledge(storage.clone(), alice(), id).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::GratisFactoryError::PledgeNotFound
            )
            .to_string()
        );
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
        let err = runtime::release_from_credis(&storage, a, reserved + U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CollateralError::Exceeded
            )
            .to_string()
        );
        let err = runtime::burn_from_credis(&storage, a, reserved + U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CollateralError::Exceeded
            )
            .to_string()
        );
        assert_eq!(view_pledged(&storage, alice()), reserved * U256::from(2u64));
        assert_eq!(
            runtime::collateral_of(&storage, a).unwrap().remaining_minor,
            reserved
        );

        let returned = U256::from(40u64);
        runtime::release_from_credis(&storage, a, returned).unwrap();
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
        let err = runtime::release_from_credis(&storage, a, U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CollateralError::NotFound
            )
            .to_string()
        );
        assert_eq!(
            runtime::collateral_of(&storage, b).unwrap().remaining_minor,
            reserved
        );

        let third = reserve_and_pledge(&storage, alice(), 3, 3);
        let err = runtime::send_to_credis(&storage, third, b, alice(), reserved).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(crate::errors::CollateralError::Exists)
                .to_string()
        );
        assert_eq!(runtime::pledge_of(&storage, third).unwrap().source, alice());
    });
}

#[test]
fn encrypted_nod_mint_preserves_nonce_and_combined_fidelity_state() {
    with_env(|storage| {
        let amount = U256::from(123);
        let nod = encrypted_nod(amount);
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

/// One second past the largest block timestamp that fits in `u64`.
fn encrypted_nod(amount: U256) -> outbe_primitives::nod_encryption::EncryptedNodV1 {
    use outbe_primitives::{nod_encryption::NodTermsV1, time::WorldwideDay};
    let day = WorldwideDay::new(20250115);
    outbe_tee_enclave::nod_encryption::encrypt_nod(
        &[0x5a; 32],
        NodTermsV1 {
            chain_id: CHAIN_ID,
            nod_id: U256::from_be_bytes([7; 32]),
            owner: alice(),
            emission_day: day.value(),
            league_id: 0,
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
    )
    .unwrap()
}

fn timestamp_above_u64() -> U256 {
    U256::from(u64::MAX) + U256::from(1)
}

fn assert_timestamp_overflow(err: impl std::fmt::Display) {
    let err = err.to_string();
    assert!(err.contains("timestamp exceeds u64"), "got: {err}");
}

#[test]
fn mint_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();
        let amount = U256::from(1_000u64);
        let err = runtime::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap_err();
        assert_timestamp_overflow(err);
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
    });
}

#[test]
fn mine_coen_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        mint_without_cohort(storage.clone(), amount);
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert_timestamp_overflow(err);

        // No COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), amount);
    });
}

/// Gratis burn and native COEN mint are one sale: both logs are present together.
#[test]
fn mine_coen_emits_gratis_burned_and_coen_mined() {
    let mut provider = test_storage();
    let amount = U256::from(1_000u64);
    StorageHandle::enter(&mut provider, |storage| {
        mint_without_cohort(storage.clone(), amount);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        dispatch(storage, &call, alice(), U256::ZERO).unwrap();
    });
    let burned: Vec<_> = provider
        .get_events(GRATIS_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_gratis::precompile::IGratis::GratisBurned::decode_log_data(log).ok()
        })
        .collect();
    let mined: Vec<_> = provider
        .get_events(GRATIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGratisFactory::CoenMinedFromGratis::decode_log_data(log).ok())
        .collect();
    fidelity_enclave::uninstall();
    test_enclave::uninstall();

    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].account, alice());
    assert_eq!(burned[0].encryptedAmountMinor.len(), 92);
    assert_eq!(&burned[0].encryptedAmountMinor[8..12], b"GRA2");
    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].account, alice());
    assert_eq!(
        mined[0].amountMinor,
        checked_protocol_to_native(amount).unwrap()
    );
}

#[test]
fn encrypted_nod_mint_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        let amount = U256::from(123);
        let nod = encrypted_nod(amount);
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();

        let err = crate::api::mint_encrypted_nod(
            storage.clone(),
            &nod,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap_err();
        assert_timestamp_overflow(err);
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(outbe_gratis::api::op_nonce(storage, alice()).unwrap(), 0);
    });
}

fn open_account(storage: &StorageHandle<'_>, account: Address) -> (U256, U256) {
    let vk = derive_view_key(&test_enclave::state_key(), account).unwrap();
    let balance = outbe_gratis::api::balance_ct(storage.clone(), account).unwrap();
    let pledged = outbe_gratis::api::pledged_ct(storage.clone(), account).unwrap();
    let balance =
        outbe_tee::gratis_decrypt::decrypt_gratis_balance(&vk, account, &balance).unwrap();
    // The pledged blob is bound to the pledged field, so the balance opener cannot read it.
    let pledged = if pledged.is_empty() {
        U256::ZERO
    } else {
        decrypt_pledged(&vk, account, &pledged).unwrap()
    };
    (balance, pledged)
}

fn token_deltas(logs: &[alloy_primitives::LogData], account: Address) -> Vec<(bool, U256)> {
    use outbe_gratis::precompile::IGratis;
    let vk = derive_view_key(&test_enclave::state_key(), account).unwrap();
    let mut deltas = Vec::new();
    for log in logs {
        let opened = if let Ok(event) = IGratis::GratisMinedFromNod::decode_log_data(log) {
            Some((true, event.encryptedAmountMinor))
        } else if let Ok(event) = IGratis::GratisMinedFromPromis::decode_log_data(log) {
            Some((true, event.encryptedAmountMinor))
        } else if let Ok(event) = IGratis::GratisBurnedFromPledge::decode_log_data(log) {
            Some((false, event.encryptedAmountMinor))
        } else if let Ok(event) = IGratis::GratisBurned::decode_log_data(log) {
            Some((false, event.encryptedAmountMinor))
        } else {
            None
        };
        if let Some((mint, blob)) = opened {
            let amount =
                outbe_tee::gratis_decrypt::decrypt_gratis_balance(&vk, account, &blob).unwrap();
            deltas.push((mint, amount));
        }
    }
    deltas
}

fn prefix_net(deltas: &[(bool, U256)], count: usize) -> U256 {
    deltas
        .iter()
        .take(count)
        .fold(U256::ZERO, |net, (mint, amount)| {
            if *mint {
                net + *amount
            } else {
                net - *amount
            }
        })
}

fn pledge_names(logs: &[alloy_primitives::LogData]) -> Vec<&'static str> {
    logs.iter()
        .filter_map(|log| {
            if IGratisFactory::GratisPledgeCreated::decode_log_data(log).is_ok() {
                Some("created")
            } else if IGratisFactory::GratisPledgeCanceled::decode_log_data(log).is_ok() {
                Some("canceled")
            } else if IGratisFactory::GratisPledgeSentToCredis::decode_log_data(log).is_ok() {
                Some("sent")
            } else if IGratisFactory::GratisPledgeReleased::decode_log_data(log).is_ok() {
                Some("released")
            } else if IGratisFactory::GratisPledgeBurned::decode_log_data(log).is_ok() {
                Some("burned")
            } else {
                None
            }
        })
        .collect()
}

/// One account walks nod mint, promis mint, pledge, cancel, pledge, credis,
/// release, pledge burn, and coen burn. The owner's view key opens every token
/// event, and mints minus burns match liquid plus pledged after each step.
#[test]
fn view_key_reconstructs_gratis_through_pledge_and_burn() {
    let mut provider = test_storage();
    let nod_amount = U256::from(400u64);
    let promis_amount = U256::from(250u64);
    let pledged = U256::from(RESERVED);
    let released = U256::from(40u64);
    let burned_pledge = pledged - released;
    let (liquid, pledged_bal) = StorageHandle::enter(&mut provider, |storage| {
        let mut liquid = Vec::new();
        let mut pledged_bal = Vec::new();
        let snap =
            |storage: &StorageHandle<'_>, liquid: &mut Vec<U256>, pledged_bal: &mut Vec<U256>| {
                let (balance, pledged) = open_account(storage, alice());
                liquid.push(balance);
                pledged_bal.push(pledged);
            };
        crate::api::mint_encrypted_nod(
            storage.clone(),
            &encrypted_nod(nod_amount),
            auth(GratisOp::Mint, alice(), nod_amount, 0),
        )
        .unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        runtime::mint(
            storage.clone(),
            alice(),
            promis_amount,
            auth(GratisOp::Mint, alice(), promis_amount, 1),
        )
        .unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        seed_fidelity(storage.clone(), alice());
        let id = U256::ONE;
        outbe_vaultrouter::schema::VaultRouterContract::new(storage.clone())
            .reservations
            .create(&outbe_vaultrouter::schema::LiquidityReservation {
                id,
                asset: address!("0x0000000000000000000000000000000000000888"),
                amount_minor: pledged * U256::from(2u64),
                gratis_minor: pledged,
                expires_at: CREATED_AT + 900,
                source: alice(),
                ..Default::default()
            })
            .unwrap();
        pledge(&storage, alice(), id, 2).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        runtime::cancel_pledge(storage.clone(), alice(), id).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        pledge(&storage, alice(), id, 3).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        let credis = U256::from(7u64);
        runtime::send_to_credis(&storage, id, credis, alice(), pledged).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        runtime::release_from_credis(&storage, credis, released).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        runtime::burn_from_credis(&storage, credis, burned_pledge).unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        let remaining = nod_amount + promis_amount - burned_pledge;
        runtime::mine_coen(
            storage.clone(),
            alice(),
            remaining,
            auth(GratisOp::Burn, alice(), remaining, 4),
        )
        .unwrap();
        snap(&storage, &mut liquid, &mut pledged_bal);
        (liquid, pledged_bal)
    });
    let deltas = token_deltas(provider.get_events(GRATIS_ADDRESS), alice());
    fidelity_enclave::uninstall();
    test_enclave::uninstall();

    assert_eq!(
        deltas,
        vec![
            (true, nod_amount),
            (true, promis_amount),
            (false, burned_pledge),
            (false, nod_amount + promis_amount - burned_pledge),
        ]
    );
    let token_counts = [1usize, 2, 2, 2, 2, 2, 2, 3, 4];
    assert_eq!(liquid.len(), token_counts.len());
    for (index, count) in token_counts.iter().enumerate() {
        assert_eq!(
            prefix_net(&deltas, *count),
            liquid[index] + pledged_bal[index],
            "step {index}"
        );
    }
    assert_eq!(pledged_bal[2], pledged);
    assert_eq!(pledged_bal[3], U256::ZERO);
    assert_eq!(pledged_bal[6], pledged - released);
    assert_eq!(liquid[8] + pledged_bal[8], U256::ZERO);
    assert_eq!(
        pledge_names(provider.get_events(GRATIS_FACTORY_ADDRESS)),
        ["created", "canceled", "created", "sent", "released", "burned"]
    );
}
