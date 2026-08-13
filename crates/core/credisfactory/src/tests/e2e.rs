//! End-to-end flow: mine → pledge → requestCredis → latch → settle → void.
//!
//! The harness (in-process enclave, sub-call stubs, view-key decryption, the
//! finalized daily series) lives in [`crate::tests::common`].

use alloy_primitives::{Address, B256, U256};

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
        assert_eq!(
            position.floor_price,
            U256::from(2_160_000_000_000_000_000u64)
        );
        assert_eq!(
            position.call_price,
            U256::from(2_640_000_000_000_000_000u64)
        );
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
        let paid = settle_principal(&storage, alice(), position_id, half);

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.outstanding, half);
        assert_eq!(position.collateral_locked, pledge_cost() / U256::from(2u64));
        assert!(
            paid > half,
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

        let paid = runtime::settle(
            storage.clone(),
            alice(),
            position_id,
            pledge_stables() * U256::from(1_000u64),
        )
        .unwrap();
        assert_eq!(
            paid,
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
fn request_credis_requires_the_card_account_to_hold_matching_funds() {
    let mut storage = env();
    // §7: the bundle must already hold the credit it is asking for. One wei short
    // of the requested $2.00 is a rejection.
    set_matched_funds(&mut storage, pledge_stables() - U256::from(1u64));
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let handle = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());

        let err =
            runtime::request_credis(storage.clone(), cca(), alice(), handle, spend).unwrap_err();
        assert!(err.to_string().contains("matching funds"), "got: {err}");
    });
    teardown();

    // Exactly the requested amount clears the rule.
    let mut storage = env();
    set_matched_funds(&mut storage, pledge_stables());
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let handle = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        runtime::request_credis(storage.clone(), cca(), alice(), handle, spend).unwrap();
    });
    teardown();
}

#[test]
fn a_stale_pledge_quote_cannot_be_exercised() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let stale = pledge(&storage, alice(), 1);

        // §7: the quote fixes P₀, so it expires if unused. One second past the
        // TTL is too late.
        advance_to(
            &storage,
            CREATED_AT + outbe_credis::constants::PLEDGE_QUOTE_TTL_SECS + 1,
        );
        let spend = credis_spend_auth(alice(), stale, alice());
        let err =
            runtime::request_credis(storage.clone(), cca(), alice(), stale, spend).unwrap_err();
        assert!(err.to_string().contains("quote has expired"), "got: {err}");

        // A quote struck at the new height is live, so age is what is being
        // judged rather than anything about the ticket itself.
        let fresh = pledge(&storage, alice(), 2);
        let spend = credis_spend_auth(alice(), fresh, alice());
        runtime::request_credis(storage.clone(), cca(), alice(), fresh, spend).unwrap();

        // Spending a ticket clears its quote, so the record cannot linger.
        assert_eq!(
            outbe_gratisfactory::api::pledge_quoted_at(storage.clone(), fresh).unwrap(),
            0
        );
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
fn void_sweep_burns_only_the_unpaid_share() {
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

        // One second inside the window the scan must find nothing. No daily
        // reference price is published, so the latch and call arms stay inert and
        // only the price-independent void arm can act.
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

        // Idempotent: a second run at the same height finds nothing to burn — the
        // voided position has left the active index entirely.
        assert_eq!(scan(&storage, deadline), 0);
        assert_eq!(
            CredisContract::new(storage.clone()).active_len().unwrap(),
            0
        );
    });
    teardown();
}

#[test]
fn a_position_settled_inside_the_window_is_never_voided() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
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

        // Nothing burned: the whole pledge is back with alice.
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost()
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
