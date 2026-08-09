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

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

/// COEN/840 rate these tests seed: 2.0, 1e18-scaled.
fn oracle_rate() -> U256 {
    U256::from(2u64) * one_e18()
}

/// Credit a pledge asks for: $2.00 in 6-decimal minor units. At [`oracle_rate`] that
/// costs exactly [`pledge_cost`] gratis, so the collateral numbers stay round — and
/// stables and gratis stay visibly different, which is what catches a unit mix-up.
fn pledge_stables() -> U256 {
    U256::from(2_000_000u64)
}

/// Gratis [`pledge_stables`] costs at [`oracle_rate`]:
/// `2e6 * 1e12 * 1e18 / 2e18 = 1e18`.
fn pledge_cost() -> U256 {
    one_e18()
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

fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

/// Register the COEN/840 pair plus the ISO 840 settlement mapping the pledge
/// conversion resolves through (the asset's `isoCode()` selects the pair).
fn seed_oracle(storage: StorageHandle<'_>, rate_1e18: U256) {
    let mut oracle = outbe_oracle::schema::OracleContract::new(storage);
    oracle
        .register_pair(outbe_oracle::api::AddressPair::new_coen_to(840))
        .unwrap();
    oracle
        .set_exchange_rate(
            Address::ZERO,
            outbe_oracle::api::COEN_ASSET,
            outbe_oracle::api::currency_address(840),
            rate_1e18,
            0,
            0,
        )
        .unwrap();
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

/// Run `f` in a fresh storage scope with BOTH the Gratis and Promis in-process
/// enclaves installed (mineFromPromis burns confidential promis then mints
/// confidential gratis), the block time set (so Fidelity reads a non-zero `now`), and
/// the COEN/840 pair seeded (pledges are priced from it).
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    outbe_promis::enclave_client::test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    // `pledge_gratis` staticcalls the asset for its ISO 4217 code before pricing.
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(asset(), iso_word(ASSET_ISO));
    let out = StorageHandle::enter(&mut storage, |s| {
        seed_oracle(s.clone(), oracle_rate());
        f(s.clone())
    });
    fidelity_enclave::uninstall();
    outbe_promis::enclave_client::test_enclave::uninstall();
    test_enclave::uninstall();
    out
}

/// The Promis modify authorization for `owner` at `op_nonce` (the confidential
/// promis ledger is keyed independently from gratis).
fn promis_auth(
    op: outbe_tee::protocol::PromisOp,
    owner: Address,
    amount: U256,
    op_nonce: u64,
) -> ModifyAuth {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let mk = outbe_tee_enclave::promis::derive_modify_key(&sk, owner).unwrap();
    ModifyAuth {
        mac: outbe_tee_enclave::promis::modify_mac(&mk, owner, op, amount, op_nonce, chain_b256()),
        op_nonce,
    }
}

/// Decrypt `owner`'s confidential promis balance with its view key.
fn promis_view_balance(s: &StorageHandle<'_>, owner: Address) -> U256 {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let vk = outbe_tee_enclave::promis::derive_view_key(&sk, owner).unwrap();
    let blob = outbe_promis::api::balance_ct(s.clone(), owner).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    outbe_tee_enclave::promis::decrypt_balance(&vk, owner, &blob).unwrap()
}

/// Mint confidential promis to `owner` (op-nonce 0) so mineFromPromis has a balance
/// to burn.
fn seed_promis(storage: StorageHandle<'_>, owner: Address, amount: U256) {
    outbe_promis::api::mint(
        storage,
        owner,
        amount,
        promis_auth(outbe_tee::protocol::PromisOp::Mint, owner, amount, 0),
    )
    .unwrap();
}

/// `pledgeGratis(amountStables, asset, maxGratis, mac, opNonce)` calldata. `max_gratis`
/// is the caller's slippage cap; pass `U256::MAX` when the test does not exercise it.
fn pledge_call(a: ModifyAuth, amount_stables: U256, max_gratis: U256) -> Bytes {
    Bytes::from(
        IGratisFactory::IGratisFactoryCalls::pledgeGratis(IGratisFactory::pledgeGratisCall {
            amountStables: amount_stables,
            asset: asset(),
            maxGratis: max_gratis,
            mac: FixedBytes(a.mac),
            opNonce: a.op_nonce,
        })
        .abi_encode(),
    )
}

/// The pledger names the CREDIT they want; the gratis it costs is derived from the
/// oracle rate, and that — not the stables figure — is what leaves the balance.
#[test]
fn pledge_debits_the_oracle_derived_gratis_and_parks_it_in_the_ticket() {
    with_env(|storage| {
        let seed = pledge_cost() * U256::from(2u64);
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            seed,
            auth(GratisOp::Mint, alice(), seed, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        // Pledge at op-nonce 1 (mine advanced it from 0). The MAC binds the STABLES.
        let out = dispatch(
            storage.clone(),
            &pledge_call(
                auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
                pledge_stables(),
                U256::MAX,
            ),
            alice(),
            U256::ZERO,
        )
        .unwrap();
        let handle = IGratisFactory::pledgeGratisCall::abi_decode_returns(&out).unwrap();
        assert_ne!(handle, B256::ZERO, "a pledge handle is returned");

        // `pledge_cost()` gratis left the balance and is parked in the pending ticket
        // (NOT yet in the per-account pledged ledger); the aggregate — gratis, not
        // stables — counts it.
        assert_eq!(view_balance(&storage, alice()), seed - pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
    });
}

/// `maxGratis` is the pledger's slippage protection: the MAC only covers the stables
/// figure, so a rate move that makes the credit cost more gratis than they accepted
/// must revert rather than quietly draining the extra.
#[test]
fn pledge_rejects_when_derived_gratis_exceeds_max() {
    with_env(|storage| {
        let seed = pledge_cost() * U256::from(2u64);
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            seed,
            auth(GratisOp::Mint, alice(), seed, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        let err = dispatch(
            storage.clone(),
            &pledge_call(
                auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
                pledge_stables(),
                pledge_cost() - U256::from(1u64),
            ),
            alice(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("maxGratis"), "got: {err}");

        // Nothing moved.
        assert_eq!(view_balance(&storage, alice()), seed);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn pledge_rejects_wrong_op_nonce() {
    with_env(|storage| {
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_cost(),
            auth(GratisOp::Mint, alice(), pledge_cost(), 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        // op-nonce is 1 after the mine; a stale/forged 5 must be rejected.
        let err = dispatch(
            storage.clone(),
            &pledge_call(
                auth(GratisOp::Pledge, alice(), pledge_stables(), 5),
                pledge_stables(),
                U256::MAX,
            ),
            alice(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("op nonce"), "got: {err}");
    });
}

#[test]
fn pledge_rejects_zero_asset() {
    with_env(|storage| {
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_cost(),
            auth(GratisOp::Mint, alice(), pledge_cost(), 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        let err = runtime::pledge_gratis(
            storage.clone(),
            alice(),
            pledge_stables(),
            Address::ZERO,
            U256::MAX,
            auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
        )
        .unwrap_err();
        assert!(err.to_string().contains("asset"), "got: {err}");
    });
}

#[test]
fn unpledge_returns_collateral_to_pledger() {
    with_env(|storage| {
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            pledge_cost(),
            auth(GratisOp::Mint, alice(), pledge_cost(), 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        let (handle, gratis_cost) = runtime::pledge_gratis(
            storage.clone(),
            alice(),
            pledge_stables(),
            asset(),
            U256::MAX,
            auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
        )
        .unwrap();
        assert_eq!(gratis_cost, pledge_cost());
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);

        // Direct unpledge (credis rejected) at op-nonce 2, quoted in the same unit the
        // pledge was: stables in, the full gratis collateral back.
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::unpledgeGratis(
                IGratisFactory::unpledgeGratisCall {
                    amountStables: pledge_stables(),
                    pledgeHandle: handle,
                    mac: FixedBytes(auth(GratisOp::Unpledge, alice(), pledge_stables(), 2).mac),
                    opNonce: 2,
                },
            )
            .abi_encode(),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();

        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
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

        // The acquisition cohort was recorded: sole holder, no sales → top league.
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
            err.to_string().contains("amount must be positive"),
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
        assert_eq!(minted, amount);

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(storage.balance(alice()).unwrap(), amount);

        // Fully sold → efficiency 0 → league drops to the floor.
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
            err.to_string().contains("insufficient balance"),
            "got: {err}"
        );

        // Atomic revert: no COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), U256::from(100u64));
    });
}

#[test]
fn mine_from_promis_burns_promis_mints_gratis_creating_fidelity_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    with_env(|storage| {
        let amount = U256::from(1_000u64);

        // Seed only (confidential) promis to convert — no Fidelity cohort yet.
        // Promis is fidelity-neutral, so the aged RCFI a year out is zero up front;
        // the post-conversion `> 0` check then proves the conversion recorded a
        // fresh gratis cohort (rather than it having pre-existed).
        seed_promis(storage.clone(), alice(), amount);
        let later = CREATED_AT + ONE_YEAR_SECS;
        let league_before =
            outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_before, MIN_LEAGUE);

        // mineFromPromis on the gratisfactory precompile. Both the promis burn and
        // the gratis mint are enclave-confidential, so the call carries two modify
        // authorizations at each ledger's current op-nonce: gratis is fresh (0),
        // promis already advanced to 1 by the seed mint.
        let ga = auth(GratisOp::Mint, alice(), amount, 0);
        let pa = promis_auth(outbe_tee::protocol::PromisOp::Burn, alice(), amount, 1);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineFromPromis(
                IGratisFactory::mineFromPromisCall {
                    amount,
                    mac: FixedBytes(ga.mac),
                    opNonce: ga.op_nonce,
                    promisMac: FixedBytes(pa.mac),
                    promisOpNonce: pa.op_nonce,
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IGratisFactory::mineFromPromisCall::abi_decode_returns(&out).unwrap();
        assert_eq!(minted, amount);

        // Promis fully burned; gratis minted 1:1 to the account (decrypt both
        // confidential balances to check; promis total supply is public).
        assert_eq!(promis_view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_promis::api::total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            amount
        );

        // A fresh gratis acquisition cohort was recorded at conversion time
        // (CREATED_AT): sole holder, no sales → top league a year later. If
        // `mine_from_promis` stopped recording the cohort, this would stay at the
        // floor.
        let league_after = outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_after, MAX_LEAGUE);
    });
}

/// mine_from_promis with insufficient balance must fail with no partial state:
/// no promis burned, no gratis minted (atomic revert).
#[test]
fn mine_from_promis_rejects_insufficient_balance() {
    with_env(|storage| {
        // Alice holds 100 (confidential) promis but tries to convert 200.
        seed_promis(storage.clone(), alice(), U256::from(100u64));

        // The promis burn fails before the gratis mint is reached; the gratis auth
        // is never checked (zero placeholder), but the promis burn auth must be
        // valid to reach the balance check (op-nonce 1 after the seed mint).
        let pa = promis_auth(
            outbe_tee::protocol::PromisOp::Burn,
            alice(),
            U256::from(200u64),
            1,
        );
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineFromPromis(
                IGratisFactory::mineFromPromisCall {
                    amount: U256::from(200u64),
                    mac: FixedBytes([0u8; 32]),
                    opNonce: 0,
                    promisMac: FixedBytes(pa.mac),
                    promisOpNonce: pa.op_nonce,
                },
            )
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("insufficient balance"),
            "got: {err}"
        );

        // No gratis minted (no ciphertext ever written), promis untouched.
        assert_eq!(promis_view_balance(&storage, alice()), U256::from(100u64));
        assert!(outbe_gratis::api::balance_ct(storage.clone(), alice())
            .unwrap()
            .is_empty());
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
                amountStables: U256::from(1u64),
                asset: asset(),
                maxGratis: U256::MAX,
                mac: FixedBytes([0u8; 32]),
                opNonce: 0,
            })
            .abi_encode(),
        );
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"), "got: {err}");
    });
}
