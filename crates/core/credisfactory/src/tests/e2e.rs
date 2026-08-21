//! End-to-end flow: mine → pledge → requestCredis → latch → settle → void.
//!
//! The harness (in-process enclave, sub-call stubs, view-key decryption, the
//! finalized daily series) lives in [`crate::tests::common`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use crate::precompile::ICredisFactory;
use outbe_credis::{CredisContract, CredisState};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;

use crate::runtime;
use crate::tests::common::*;

#[test]
fn request_credis_seals_the_position_geometry_from_the_pledge_quote() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let handle = pledge(&storage, alice(), 1);
        // Pledge parks the amount in the ticket: balance drained, pledged ledger 0.
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);

        let spend = credis_spend_auth(alice(), handle, alice());
        let (position_id, amount_stables) =
            runtime::request_credis(storage.clone(), cca(), alice(), handle, spend).unwrap();

        // The collateral moved into alice's OWN pledged ledger.
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        // The loan is exactly what the pledger asked for — credis does not re-price it.
        assert_eq!(amount_stables, pledge_stables());

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.smart_account, alice());
        assert_eq!(position.cca, cca(), "the caller is the originating agent");
        // The pledger EOA is stored sealed (ciphertext), never as a plaintext address,
        // and the enclave opens it back to alice via RevealOwner.
        assert!(!position.eoa_ct.is_empty(), "eoa stored as ciphertext");
        assert_eq!(
            outbe_gratis::api::reveal_owner(storage.clone(), &position.eoa_ct).unwrap(),
            alice()
        );

        assert_eq!(position.principal, amount_stables);
        assert_eq!(position.outstanding, amount_stables);
        assert_eq!(position.collateral, pledge_cost());
        assert_eq!(position.collateral_locked, pledge_cost());
        // The entry price is the rate the PLEDGE was quoted at, not a later read;
        // floor and call derive from it, so the whole geometry follows the quote.
        assert_eq!(position.entry_price, oracle_rate());
        assert_eq!(position.floor_price, U256::from(2_160_000u64));
        assert_eq!(position.call_price, U256::from(2_640_000u64));
        assert_eq!(position.policy_rate, policy_rate());
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Open);
        assert_eq!(position.last_settled_at, position.originated_at);
    });
    teardown();
}

#[test]
fn settle_is_rejected_until_the_price_crosses_the_floor() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        // The seeded price (2.0) sits below the floor (2.16).
        let err = runtime::settle(
            storage.clone(),
            alice(),
            position_id,
            U256::from(1_000_000u64),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not settleable"), "got: {err}");

        // Cross the floor: the settlement latches the position on the way through.
        set_coen_rate(&storage, above_floor());
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Settleable
        );

        // The latch is one-way: the price falling back below the floor does not
        // re-lock the position.
        set_coen_rate(&storage, oracle_rate());
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .outstanding,
            U256::from(1_000_000u64)
        );
    });
    teardown();
}

#[test]
fn settlement_releases_collateral_proportionally_and_closes_without_dust() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // Half the principal, 30 days in → half the collateral back.
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let half = pledge_stables() / U256::from(2u64);
        let (principal_paid, interest_paid) =
            settle_principal(&storage, alice(), position_id, half);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.outstanding, half);
        assert_eq!(position.collateral_locked, pledge_cost() / U256::from(2u64));
        // The two components are reported separately: the principal is exactly
        // what was asked for, and the interest rode on top of it.
        assert_eq!(principal_paid, half);
        assert!(
            !interest_paid.is_zero(),
            "interest was collected on top of the principal"
        );

        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );

        // Settling the rest returns the whole pledge and closes the position with
        // nothing stranded in the pledged ledger.
        advance_to(&storage, CREATED_AT + 60 * DAY);
        settle_principal(&storage, alice(), position_id, half);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Settled);
        assert!(position.collateral_locked.is_zero());
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

#[test]
fn the_settle_abi_returns_the_principal_and_interest_split() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());
        advance_to(&storage, CREATED_AT + 30 * DAY);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&position, now_of(&storage)).unwrap();
        assert!(!interest.is_zero(), "30 days must have accrued something");
        let principal = pledge_stables() / U256::from(4u64);

        // Drive the real ABI path, so the two-field return is exercised through
        // encoding and decoding rather than only as a Rust tuple.
        let data = ICredisFactory::settleCall {
            positionId: position_id,
            amount: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
        let decoded = ICredisFactory::settleCall::abi_decode_returns(&out).unwrap();

        // Order matters: principal first, interest second.
        assert_eq!(decoded.principal, principal);
        assert_eq!(decoded.interest, interest);

        let after = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(
            after.outstanding,
            pledge_stables() - principal,
            "only the principal component reduces the balance"
        );
    });
    teardown();
}

#[test]
fn settle_takes_only_what_the_position_needs() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());
        advance_to(&storage, CREATED_AT + 30 * DAY);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&position, now_of(&storage)).unwrap();

        let (principal_paid, interest_paid) = runtime::settle(
            storage.clone(),
            alice(),
            position_id,
            pledge_stables() * U256::from(1_000u64),
        )
        .unwrap();
        // The split is reported separately, and only what the position needed
        // was pulled — the vast over-payment is not.
        assert_eq!(principal_paid, pledge_stables());
        assert_eq!(interest_paid, interest);
        assert_eq!(
            principal_paid + interest_paid,
            interest + pledge_stables(),
            "only interest + outstanding principal is pulled"
        );
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn settle_accepts_a_third_party_payer() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // bob is neither the pledger nor the smart account, but anyone may settle.
        let half = pledge_stables() / U256::from(2u64);
        settle_principal(&storage, bob(), position_id, half);

        // The freed collateral goes to the ORIGINAL pledger, never to the payer — this
        // is what makes an open payer safe without an access check.
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
    });
    teardown();
}

#[test]
fn request_credis_rejects_an_owner_with_an_unresolved_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        // Both pledges are quoted at the seeded rate, before the price moves.
        let first_handle = pledge(&storage, alice(), 1);
        let second_handle = pledge(&storage, alice(), 2);

        let first_spend = credis_spend_auth(alice(), first_handle, alice());
        let (first, _) =
            runtime::request_credis(storage.clone(), cca(), alice(), first_handle, first_spend)
                .unwrap();

        // Latch and call the first position.
        set_coen_rate(&storage, above_floor());
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_settleable(first).unwrap());
            assert!(credis.mark_called(first, now_of(&storage)).unwrap());
        }

        let spend = credis_spend_auth(alice(), second_handle, alice());
        let err = runtime::request_credis(storage.clone(), cca(), alice(), second_handle, spend)
            .unwrap_err();
        assert!(err.to_string().contains("called position"), "got: {err}");

        // Settling the call in full clears the block.
        settle_principal(&storage, alice(), first, pledge_stables());
        runtime::request_credis(storage.clone(), cca(), alice(), second_handle, spend).unwrap();
    });
    teardown();
}

#[test]
fn request_credis_rejects_zero_smart_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err =
            runtime::request_credis(storage.clone(), cca(), Address::ZERO, B256::ZERO, [0u8; 32])
                .unwrap_err();
        assert!(err.to_string().contains("smart account"), "got: {err}");
    });
}

#[test]
fn the_void_burns_only_the_unpaid_share() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        set_coen_rate(&storage, above_floor());

        // Settle half the principal, reclaiming half the collateral.
        advance_to(&storage, CREATED_AT + 30 * DAY);
        settle_principal(
            &storage,
            alice(),
            position_id,
            pledge_stables() / U256::from(2u64),
        );
        let unpaid_collateral = pledge_cost() / U256::from(2u64);
        assert_eq!(view_pledged(&storage, alice()), unpaid_collateral);

        // Call it, then let the settlement window lapse.
        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        // Encrypted cohort ledger before the burn — the burn records a sale
        // cohort, so the ciphertext must change afterwards.
        let cohorts_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert!(!cohorts_before.is_empty(), "alice has a seeded cohort");

        // One second inside the window the sweep must find nothing.
        let deadline = called_at + 14 * DAY;
        advance_to(&storage, deadline - 1);
        finalize_through(&storage, deadline - 1);
        assert_eq!(scan(&storage, deadline - 1), 0, "window still open");

        advance_to(&storage, deadline);
        finalize_through(&storage, deadline);
        assert_eq!(scan(&storage, deadline), 1);

        // Only the unpaid share was burned; the settled half stays with alice.
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), unpaid_collateral);
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost() - unpaid_collateral
        );
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );

        // The equivalent value was deposited 1:1 into the Promis Reserve.
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            unpaid_collateral
        );

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Void);
        assert!(position.outstanding.is_zero());
        assert!(position.collateral_locked.is_zero());

        // A sale cohort was recorded for the burned collateral, so alice's
        // encrypted cohort ledger changed (the RCFI-drop semantics itself is
        // covered by the fidelity + enclave tests).
        let cohorts_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert_ne!(
            cohorts_before, cohorts_after,
            "the void burn should record a sale cohort"
        );

        // Idempotent: a second sweep at the same height finds nothing to burn.
        assert_eq!(scan(&storage, deadline), 0);
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            0,
            "a voided position leaves the active index"
        );
    });
    teardown();
}

#[test]
fn a_position_settled_inside_the_window_is_never_voided() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let position_id = open(&storage, 1);
        // A second position keeps the active index non-empty after the first is
        // settled, so the scan really walks the book instead of returning at its
        // `len == 0` early exit and passing this test vacuously.
        let bystander = open(&storage, 2);
        set_coen_rate(&storage, above_floor());

        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_settleable(position_id).unwrap());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        // Settle in full inside the window.
        advance_to(&storage, called_at + 7 * DAY);
        settle_principal(&storage, alice(), position_id, pledge_stables());

        let deadline = called_at + 14 * DAY;
        advance_to(&storage, deadline + DAY);
        finalize_through(&storage, deadline + DAY);
        assert_eq!(scan(&storage, deadline + DAY), 0);
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            1,
            "the settled position left the index; the bystander keeps the scan walking"
        );
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(bystander)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Open
        );

        // Nothing burned: the settled pledge is back with alice, and the
        // bystander's collateral is still pledged.
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost() * U256::from(2u64),
            "nothing was burned"
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::ZERO
        );
    });
    teardown();
}
