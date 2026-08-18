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

use outbe_credis::constants::PLEDGE_QUOTE_TTL_SECS;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_vaultrouter::api::IVaultRouter;

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;
use crate::schema::GratisFactoryContract;

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

/// ABI-encoded `uint256` return — the shares the vault router's reservation calls
/// report burning or minting. The value is not asserted anywhere; the real share
/// arithmetic is covered by `outbe_vaultrouter::tests`.
fn shares_word() -> Bytes {
    let mut b = vec![0u8; 32];
    b[31] = 1;
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
    outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
    outbe_oracle::api::set_exchange_rate(
        storage,
        Address::ZERO,
        outbe_oracle::api::DAY_TYPE_PAIR,
        rate_1e18,
        0,
        0,
    )
    .unwrap();
}

fn advance_to(storage: &StorageHandle<'_>, timestamp: u64) {
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
}

/// Mints exactly one pledge's worth of gratis to `account`, seeds its fidelity and
/// pledges once. `mint_nonce` is the account's current op-nonce; the pledge takes
/// the next one. Returns the pledge handle.
fn pledge_once(storage: &StorageHandle<'_>, account: Address, mint_nonce: u64) -> B256 {
    let seed = pledge_cost();
    outbe_gratis::api::mint(
        storage.clone(),
        account,
        seed,
        auth(GratisOp::Mint, account, seed, mint_nonce),
    )
    .unwrap();
    seed_fidelity(storage.clone(), account);
    runtime::pledge_gratis(
        storage.clone(),
        account,
        pledge_stables(),
        asset(),
        U256::MAX,
        auth(GratisOp::Pledge, account, pledge_stables(), mint_nonce + 1),
    )
    .unwrap()
    .0
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
    with_env_reservable(true, f)
}

/// [`with_env`] with the vault router's `reserve` either answering or not. A
/// pledge claims its credit out of the vault, so a router that cannot answer is
/// the "vault could not fund this" case.
///
/// `HashMapStorageProvider` cannot stub a reverting sub-call, so `reservable:
/// false` simply leaves `reserve` unstubbed: it returns empty returndata, the api
/// helper fails to decode, and `pledge_gratis` sees the same `Err` a revert would
/// produce. `returnReservation` stays stubbed either way — unpledge and the sweep
/// give assets back regardless of how the pledge went.
fn with_env_reservable<R>(reservable: bool, f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    outbe_promis::enclave_client::test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    // `pledge_gratis` staticcalls the asset for its ISO 4217 code before pricing.
    storage.enable_sub_call_stub();
    storage.stub_sub_call_at(asset(), iso_word(ASSET_ISO));
    storage.stub_sub_call_at_selector(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::returnReservationCall::SELECTOR,
        shares_word(),
    );
    if reservable {
        storage.stub_sub_call_at_selector(
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::reserveCall::SELECTOR,
            shares_word(),
        );
    }
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

/// A pledge that cannot claim its credit out of the vault must leave nothing
/// behind. The whole point of reserving at pledge time is that the pledger never
/// locks collateral against a `requestCredis` the vault could not fund, so a failed
/// reservation has to take the ticket down with it.
#[test]
fn a_pledge_whose_reservation_fails_locks_nothing() {
    with_env_reservable(false, |storage| {
        let seed = pledge_cost();
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            seed,
            auth(GratisOp::Mint, alice(), seed, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        assert!(runtime::pledge_gratis(
            storage.clone(),
            alice(),
            pledge_stables(),
            asset(),
            U256::MAX,
            auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
        )
        .is_err());

        // The pledger keeps their whole balance and nothing is queued for a sweep.
        assert_eq!(view_balance(&storage, alice()), seed);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            GratisFactoryContract::new(storage.clone())
                .pledge_queue
                .len()
                .unwrap(),
            0
        );
    });
}

/// The pledge queue is the sweep's only input, so a pledge must land on it and a
/// spend must not: `requestCredis` clears the quote, which turns the queue entry
/// into a tombstone rather than removing it.
#[test]
fn a_pledge_is_queued_for_expiry_and_a_spend_tombstones_it() {
    with_env(|storage| {
        let handle = pledge_once(&storage, alice(), 0);
        let contract = GratisFactoryContract::new(storage.clone());

        assert_eq!(contract.pledge_queue.len().unwrap(), 1);
        assert_eq!(contract.pledge_queue.front().unwrap(), Some(handle));
        assert_eq!(
            contract.pledge_quoted_at.read(&handle).unwrap(),
            CREATED_AT,
            "the quote timestamp doubles as the reservation deadline"
        );

        // What credisfactory does when the ticket is spent.
        crate::api::clear_pledge_quote(storage.clone(), handle).unwrap();

        // Still queued, but now a tombstone: the sweep must drop it without
        // touching the router, since the reservation is already released.
        assert_eq!(contract.pledge_queue.len().unwrap(), 1);
        assert_eq!(crate::lifecycle::sweep_expired(&storage, 8).unwrap(), 0);
        assert_eq!(contract.pledge_queue.len().unwrap(), 0);
    });
}

/// Before the TTL elapses the head is not due, and the sweep must stop at it
/// rather than walk the queue — that early break is what keeps a scheduled sweep
/// cheap when nothing has expired.
#[test]
fn the_sweep_leaves_a_live_quote_alone() {
    with_env(|storage| {
        let handle = pledge_once(&storage, alice(), 0);
        let contract = GratisFactoryContract::new(storage.clone());

        advance_to(&storage, CREATED_AT + PLEDGE_QUOTE_TTL_SECS);
        assert_eq!(
            crate::lifecycle::sweep_expired(&storage, 8).unwrap(),
            0,
            "the deadline is inclusive; equality is not yet expired"
        );
        assert_eq!(contract.pledge_queue.len().unwrap(), 1);
        assert_eq!(contract.pledge_quoted_at.read(&handle).unwrap(), CREATED_AT);
    });
}

/// The documented boundary of this feature: expiry returns the vault credit and
/// clears the quote, but the GRATIS stays in the ticket. If that ever changes
/// silently — an unauthenticated enclave unpledge slipping in — this test fails.
#[test]
fn expiry_returns_the_credit_but_leaves_the_collateral_pledged() {
    with_env(|storage| {
        let handle = pledge_once(&storage, alice(), 0);
        let contract = GratisFactoryContract::new(storage.clone());
        let after_pledge = view_balance(&storage, alice());

        advance_to(&storage, CREATED_AT + PLEDGE_QUOTE_TTL_SECS + 1);
        assert_eq!(crate::lifecycle::sweep_expired(&storage, 8).unwrap(), 1);

        // Quote gone, queue drained, and the sweep is idempotent.
        assert_eq!(contract.pledge_quoted_at.read(&handle).unwrap(), 0);
        assert_eq!(contract.pledge_queue.len().unwrap(), 0);
        assert_eq!(crate::lifecycle::sweep_expired(&storage, 8).unwrap(), 0);

        // The collateral did NOT come back. Recovering it is the pledger's own
        // `unpledgeGratis` call; see the todo in `crate::lifecycle`.
        assert_eq!(view_balance(&storage, alice()), after_pledge);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost(),
            "the ticket still holds the gratis"
        );

        // And that call still works after expiry — the ticket outlives the quote.
        runtime::unpledge_gratis(
            storage.clone(),
            alice(),
            pledge_stables(),
            handle,
            auth(GratisOp::Unpledge, alice(), pledge_stables(), 2),
        )
        .unwrap();
        assert_eq!(
            view_balance(&storage, alice()),
            after_pledge + pledge_cost()
        );
    });
}

/// The queue drains oldest-first and honours its budget, so a backlog cannot
/// starve the newest pledges of a later sweep.
#[test]
fn the_sweep_drains_in_order_within_its_budget() {
    with_env(|storage| {
        let seed = pledge_cost() * U256::from(3u64);
        outbe_gratis::api::mint(
            storage.clone(),
            alice(),
            seed,
            auth(GratisOp::Mint, alice(), seed, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());

        let mut handles = Vec::new();
        for nonce in 1..=3u64 {
            handles.push(
                runtime::pledge_gratis(
                    storage.clone(),
                    alice(),
                    pledge_stables(),
                    asset(),
                    U256::MAX,
                    auth(GratisOp::Pledge, alice(), pledge_stables(), nonce),
                )
                .unwrap()
                .0,
            );
        }
        let contract = GratisFactoryContract::new(storage.clone());
        assert_eq!(contract.pledge_queue.len().unwrap(), 3);

        advance_to(&storage, CREATED_AT + PLEDGE_QUOTE_TTL_SECS + 1);

        assert_eq!(crate::lifecycle::sweep_expired(&storage, 2).unwrap(), 2);
        assert_eq!(contract.pledge_queue.len().unwrap(), 1);
        // The two oldest went; the survivor is the last pledged.
        assert_eq!(contract.pledge_quoted_at.read(&handles[0]).unwrap(), 0);
        assert_eq!(contract.pledge_quoted_at.read(&handles[1]).unwrap(), 0);
        assert_eq!(contract.pledge_queue.front().unwrap(), Some(handles[2]));

        assert_eq!(crate::lifecycle::sweep_expired(&storage, 2).unwrap(), 1);
        assert_eq!(contract.pledge_queue.len().unwrap(), 0);
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
