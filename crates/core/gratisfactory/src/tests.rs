//! Confidential gratisfactory tests driven by the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`). Balances/pledged amounts are
//! asserted by decrypting the ciphertext with the account's view key exactly as a
//! client would; writes carry a `ModifyAuth` bound to the account's op-nonce.

use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};

use outbe_gratis::enclave_client::test_enclave;
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{derive_modify_key, derive_view_key, modify_mac};

use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;
use outbe_tee::pledgenote::*;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
/// ISO 4217 code the pledged asset reports via `isoCode()`.
const ASSET_ISO: u16 = 840;

/// ABI-encoded `uint16` return for the asset's `isoCode()` static sub-call.
fn iso_word(iso: u16) -> Bytes {
    let mut b = vec![0u8; 32];
    b[30..32].copy_from_slice(&iso.to_be_bytes());
    Bytes::from(b)
}

/// The stablecoin a pledge is quoted in.
fn asset() -> Address {
    address!("0x0888088808880888088808880888088808880888")
}

fn one_six_decimal_unit() -> U256 {
    U256::from(1_000_000u64)
}

/// COEN/840 rate these tests seed: 2.0 at the pair's six-decimal scale.
fn oracle_rate() -> U256 {
    U256::from(2u64) * one_six_decimal_unit()
}

/// Credit a pledge asks for: $2.00 in 6-decimal minor units. At [`oracle_rate`] that
/// costs exactly [`pledge_cost`] gratis, so the collateral numbers stay round - and
/// stables and gratis stay visibly different, which is what catches a unit mix-up.
fn pledge_stables() -> U256 {
    U256::from(2_000_000u64)
}

/// Gratis [`pledge_stables`] costs at [`oracle_rate`]:
/// `floor(2e6 * 1e6 / 2e6) = 1e6`.
fn pledge_cost() -> U256 {
    one_six_decimal_unit()
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
    test_enclave::query(s, a).balance
}
fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    test_enclave::query(s, a).pledged
}

/// Register the COEN/840 pair plus the ISO 840 settlement mapping the pledge
/// conversion resolves through (the asset's `isoCode()` selects the pair).
fn seed_oracle(storage: StorageHandle<'_>, rate: U256) {
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::schema::OracleContract::new(storage.clone())
        .reference_currencies
        .push(ASSET_ISO)
        .unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        rate,
        1,
        CREATED_AT,
    )
    .unwrap();
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

/// Run `f` in a fresh storage scope with the Gratis in-process enclave installed,
/// the block time set (so Fidelity reads a non-zero `now`), and the COEN/840 pair
/// seeded (pledges are priced from it).
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    // `create_pledge_note` staticcalls the asset for its ISO 4217 code before pricing.
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(asset(), iso_word(ASSET_ISO));
    let out = StorageHandle::enter(&mut storage, |s| {
        seed_oracle(s.clone(), oracle_rate());
        f(s.clone())
    });
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
    out
}

fn create_call(storage: &StorageHandle<'_>, nonce: u64, principal: U256, cap: U256) -> Bytes {
    let quote = Quote {
        asset: asset(),
        principal_minor: principal,
        max_gratis_minor: cap,
        reference_currency: ASSET_ISO,
    };
    let envelope =
        test_enclave::owner_envelope(storage, alice(), nonce, OwnerAction::Create(quote.clone()));
    IGratisFactory::createPledgeNoteCall {
        request: encode(&CreateRequest { quote, envelope }).unwrap().into(),
    }
    .abi_encode()
    .into()
}

fn seed(storage: &StorageHandle<'_>) {
    let amount = pledge_cost() * U256::from(2);
    outbe_gratis::api::mint(
        storage.clone(),
        alice(),
        amount,
        auth(GratisOp::Mint, alice(), amount, 0),
    )
    .unwrap();
    seed_fidelity(storage.clone(), alice());
}

#[test]
fn private_create_and_cancel_restore_collateral_through_relayer() {
    with_env(|storage| {
        seed(&storage);
        let relayer = Address::repeat_byte(0x77);
        let out = dispatch(
            storage.clone(),
            &create_call(&storage, 1, pledge_stables(), U256::MAX),
            relayer,
            U256::ZERO,
        )
        .unwrap();
        let encrypted = IGratisFactory::createPledgeNoteCall::abi_decode_returns(&out).unwrap();
        let view = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
        let note = decrypt_receipt(&view, &encrypted).unwrap();
        assert_ne!(note.note_id, B256::ZERO);
        assert_eq!(
            note.terms.as_ref().unwrap().valid_until,
            CREATED_AT + QUOTE_TTL_SECONDS
        );
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        let encrypted_auth = test_enclave::owner_envelope(
            &storage,
            alice(),
            2,
            OwnerAction::Cancel {
                note_id: note.note_id,
            },
        );
        let call = IGratisFactory::cancelPledgeNoteCall {
            encryptedAuth: encrypted_auth.into(),
        }
        .abi_encode();
        dispatch(storage.clone(), &call, relayer, U256::ZERO).unwrap();
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() * U256::from(2)
        );
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert!(dispatch(storage.clone(), &call, relayer, U256::ZERO).is_err());
    });
}

#[test]
fn quote_guards_leave_private_state_unchanged() {
    with_env(|storage| {
        seed(&storage);
        let before = outbe_tee::pledge_ledger::head(&storage).unwrap();
        for (nonce, principal, cap) in [
            (1, pledge_stables(), pledge_cost() - U256::ONE),
            (5, pledge_stables(), U256::MAX),
            (1, U256::ONE, U256::MAX),
        ] {
            assert!(dispatch(
                storage.clone(),
                &create_call(&storage, nonce, principal, cap),
                Address::repeat_byte(0x77),
                U256::ZERO
            )
            .is_err());
            assert_eq!(outbe_tee::pledge_ledger::head(&storage).unwrap(), before);
            assert_eq!(
                view_balance(&storage, alice()),
                pledge_cost() * U256::from(2)
            );
        }
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            oracle_rate(),
            1,
            CREATED_AT - outbe_oracle::constants::FX_RATE_MAX_AGE_SECONDS - 1,
        )
        .unwrap();
        let err = dispatch(
            storage.clone(),
            &create_call(&storage, 1, pledge_stables(), U256::MAX),
            Address::repeat_byte(0x77),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("stale"), "{err}");
        assert_eq!(outbe_tee::pledge_ledger::head(&storage).unwrap(), before);
    });
}

#[test]
fn quote_rounds_down_and_binds_the_slippage_cap() {
    with_env(|storage| {
        seed(&storage);
        let out = dispatch(
            storage.clone(),
            &create_call(&storage, 1, U256::from(3), U256::ONE),
            Address::repeat_byte(0x77),
            U256::ZERO,
        )
        .unwrap();
        let encrypted = IGratisFactory::createPledgeNoteCall::abi_decode_returns(&out).unwrap();
        let view = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
        assert_eq!(
            decrypt_receipt(&view, &encrypted)
                .unwrap()
                .terms
                .unwrap()
                .gratis_minor,
            U256::ONE
        );
    });
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
            err.to_string().contains("invalid gratis command"),
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
                amount,
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
                amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string()
                .contains("insufficient confidential balance"),
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
        let call = IGratisFactory::createPledgeNoteCall {
            request: Bytes::new(),
        }
        .abi_encode();
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"), "got: {err}");
    });
}
