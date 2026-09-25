//! End-to-end flow: mine -> pledge -> issueCredis -> latch -> settle -> void.
//!
//! The harness (in-process enclave, sub-call stubs, view-key decryption, the
//! finalized daily series) lives in [`crate::tests::common`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use crate::precompile::ICredisFactory;
use outbe_credis::{CredisContract, CredisState};
use outbe_oracle::{api::AddressPair, lifecycle::OracleLifecycle, schema::OracleContract};
use outbe_primitives::block::{BlockContext, BlockLifecycle, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_promislimit::PromisLimitContract;
use outbe_tee::protocol::GratisOp;

use crate::runtime;
use crate::tests::common::*;

#[test]
fn issuance_checks_note_and_liquidity_expiry_independently() {
    for (delay, reservation_lifetime, error) in [
        (899, 1800, None),
        (900, 1800, None),
        (901, 1800, Some("pledge note expired")),
        (899, 898, Some("reservation expired")),
    ] {
        let mut provider = env();
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let (note, _) = pledge_fixture(
                storage.clone(),
                alice(),
                pledge_stables(),
                asset(),
                U256::MAX,
                auth(GratisOp::Pledge, alice(), pledge_stables(), 1),
            )
            .unwrap();
            let reservation = seed_reservation_at(
                &storage,
                cca(),
                alice(),
                asset(),
                pledge_stables(),
                CREATED_AT + reservation_lifetime,
            );
            let spend = credis_spend_auth(alice(), note, alice());
            fund_stake(&storage, pledge_stake());
            advance_to(&storage, CREATED_AT + delay);
            let result = storage.with_checkpoint(|| {
                runtime::issue_credis(
                    storage.clone(),
                    cca(),
                    alice(),
                    note,
                    spend,
                    REFERENCE_ISO,
                    reservation,
                    pledge_stake(),
                )
            });
            if let Some(reason) = error {
                let error = result.unwrap_err();
                assert!(error.to_string().contains(reason), "{error}");
                assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
                assert_eq!(
                    outbe_gratis::api::op_nonce(storage.clone(), alice()).unwrap(),
                    2
                );
                assert_eq!(
                    outbe_vaultrouter::api::reservation_of(&storage, reservation)
                        .unwrap()
                        .amount,
                    pledge_stables()
                );
                assert_eq!(
                    outbe_gratisfactory::runtime::unpledge_gratis(
                        storage.clone(),
                        alice(),
                        pledge_stables(),
                        note,
                        auth(GratisOp::Unpledge, alice(), pledge_stables(), 2)
                    )
                    .unwrap(),
                    pledge_cost()
                );
                assert_eq!(view_balance(&storage, alice()), pledge_cost());
                assert_eq!(
                    outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                    U256::ZERO
                );
            } else {
                let (id, principal) = result.unwrap();
                assert_eq!(principal, pledge_stables());
                let position = CredisContract::new(storage.clone())
                    .get_position(id)
                    .unwrap();
                assert_eq!(position.collateral, pledge_cost());
                assert_eq!(position.entry_price, oracle_rate());
                assert_eq!(position.issued_at, CREATED_AT + delay);
                assert!(
                    outbe_gratis::api::consume_pledge(storage.clone(), note, alice(), spend)
                        .is_err()
                );
            }
        });
        teardown();
    }
}

#[test]
fn issue_credis_seals_the_position_geometry_from_the_pledge_quote() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        // Pledge parks the amount in the ticket: balance drained, pledged ledger 0.
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);

        let spend = credis_spend_auth(alice(), handle, alice());
        fund_stake(&storage, pledge_stake());
        let (position_id, amount_stables) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap();

        // The collateral moved into alice's OWN pledged ledger.
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        // The loan is exactly what the pledger asked for - credis does not re-price it.
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
        // Entry price is principal / gratis, sealed on the pledge (2.00 here).
        // The call anchor is max(previous-day VWAP, current price); both are
        // seeded at 2.00, so the call price is 3.28. See
        // `entry_price_stays_on_the_pledge_when_the_reference_price_moves`.
        assert_eq!(position.entry_price, oracle_rate());
        assert_eq!(position.call_anchor_price, oracle_rate());
        assert_eq!(position.call_price, U256::from(3_280_000u64));
        assert_eq!(position.policy_rate, policy_rate());
        // Both codes are sealed, and the policy rate follows the ISSUANCE one.
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        assert_eq!(position.reference_currency, REFERENCE_ISO);
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Open);
        assert_eq!(position.last_settled_at, position.issued_at);
    });
    teardown();
}

#[test]
fn settle_runs_immediately_after_opening() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        // No price condition gates settlement: a position is settleable the
        // moment it exists, and stays Open until a sustained breach calls it.
        let sealed = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));
        settle_principal(&storage, alice(), position_id, U256::from(500_000u64));

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.outstanding, U256::from(1_000_000u64));
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Open);
        // Settlement does not reprice a live position.
        assert_eq!(position.entry_price, sealed.entry_price);
        assert_eq!(position.call_anchor_price, sealed.call_anchor_price);
        assert_eq!(position.call_price, sealed.call_price);
        assert_eq!(position.issued_at, sealed.issued_at);
    });
    teardown();
}

#[test]
fn settlement_releases_collateral_proportionally_and_closes_without_dust() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        // Half the principal, 30 days in -> half the collateral back.
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
fn rounded_returns_can_exhaust_collateral_before_repayment_or_forfeiture() {
    for forfeit in [false, true] {
        let mut storage = env();
        StorageHandle::enter(&mut storage, |storage| {
            let collateral = U256::from(6u64);
            let principal = U256::from(13u64);
            bootstrap(&storage, collateral);
            deploy_smart_account(&storage, bob());
            // The quote floors 13 / 2 to 6, and the note carries that exact amount.
            let reservation_id = seed_reservation(&storage, bob(), principal);
            let (handle, quoted) = pledge_fixture(
                storage.clone(),
                alice(),
                principal,
                asset(),
                collateral,
                auth(outbe_tee::protocol::GratisOp::Pledge, alice(), principal, 1),
            )
            .unwrap();
            assert_eq!(quoted, collateral);
            let stake = outbe_primitives::units::checked_protocol_to_native(collateral).unwrap();
            fund_stake(&storage, stake);
            let (id, disbursed) = runtime::issue_credis(
                storage.clone(),
                cca(),
                bob(),
                handle,
                credis_spend_auth(alice(), handle, bob()),
                REFERENCE_ISO,
                reservation_id,
                stake,
            )
            .unwrap();
            assert_eq!(disbursed, principal);
            let accepted = CredisContract::new(storage.clone())
                .get_position(id)
                .unwrap();
            assert_eq!(accepted.entry_price, U256::from(2_166_666));
            assert_eq!(accepted.issuance_currency, ISSUANCE_ISO);
            assert_eq!(view_pledged(&storage, alice()), collateral);

            // Positive subunit interest floors to zero; returns ceiling and then cap.
            advance_to(&storage, CREATED_AT + DAY);
            for (payment, remaining) in [(1u64, 5u64), (1, 4), (1, 3), (1, 2), (5, 0), (1, 0)] {
                assert_eq!(
                    runtime::settle(storage.clone(), bob(), id, U256::from(payment)).unwrap(),
                    (U256::from(payment), U256::ZERO)
                );
                assert_eq!(view_pledged(&storage, alice()), U256::from(remaining));
                assert_eq!(
                    view_balance(&storage, alice()),
                    collateral - U256::from(remaining)
                );
                assert_eq!(
                    outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                    U256::from(remaining)
                );
            }
            let position = CredisContract::new(storage.clone())
                .get_position(id)
                .unwrap();
            assert_eq!(position.outstanding, U256::from(3u64));
            assert_eq!(position.collateral_locked, U256::ZERO);
            assert_eq!(position.lifecycle_state().unwrap(), CredisState::Open);
            let fidelity_before = outbe_fidelity::FidelityContract::new(storage.clone())
                .cohorts_ct_of(alice())
                .unwrap();

            let expected_state = if forfeit {
                let called_at = now_of(&storage);
                CredisContract::new(storage.clone())
                    .mark_called(id, called_at)
                    .unwrap();
                let expired_at = called_at + NOTICE + 1;
                advance_to(&storage, expired_at);
                finalize_through(&storage, expired_at);
                assert_eq!(scan(&storage, expired_at), 1);
                assert_eq!(scan(&storage, expired_at), 0);
                CredisState::Void
            } else {
                runtime::settle(storage.clone(), bob(), id, U256::from(3u64)).unwrap();
                CredisState::Settled
            };
            let position = CredisContract::new(storage.clone())
                .get_position(id)
                .unwrap();
            assert_eq!(position.lifecycle_state().unwrap(), expected_state);
            assert_eq!(position.outstanding, U256::ZERO);
            assert_eq!(view_balance(&storage, alice()), collateral);
            assert_eq!(view_balance(&storage, bob()), U256::ZERO);
            assert_eq!(
                outbe_gratis::api::total_supply(storage.clone()).unwrap(),
                collateral
            );
            assert_eq!(
                outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                U256::ZERO
            );
            assert_eq!(
                PromisLimitContract::new(storage.clone())
                    .get_total_unallocated()
                    .unwrap(),
                U256::ZERO
            );
            assert_eq!(
                outbe_fidelity::FidelityContract::new(storage.clone())
                    .cohorts_ct_of(alice())
                    .unwrap(),
                fidelity_before
            );
            assert_eq!(CredisContract::new(storage).active_len().unwrap(), 0);
        });
        teardown();
    }
}

#[test]
fn the_settle_abi_returns_the_principal_and_interest_split() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
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
        // was pulled - the vast over-payment is not.
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

        // bob is neither the pledger nor the smart account, but anyone may settle.
        let half = pledge_stables() / U256::from(2u64);
        settle_principal(&storage, bob(), position_id, half);

        // The freed collateral goes to the ORIGINAL pledger, never to the payer - this
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
fn issue_credis_allows_an_owner_with_an_unresolved_call() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        // Both pledges are quoted at the seeded rate, before the price moves.
        let (first_handle, first_reservation) = pledge(&storage, alice(), 1);
        let (second_handle, second_reservation) = pledge(&storage, alice(), 2);

        let first_spend = credis_spend_auth(alice(), first_handle, alice());
        fund_stake(&storage, pledge_stake());
        let (first, _) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            first_handle,
            first_spend,
            REFERENCE_ISO,
            first_reservation,
            pledge_stake(),
        )
        .unwrap();

        // Call the first position.
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_called(first, now_of(&storage)).unwrap());
        }

        // The called position does not gate origination: the second one opens
        // while the first is still unresolved, and both stand on their own.
        let spend = credis_spend_auth(alice(), second_handle, alice());
        fund_stake(&storage, pledge_stake());
        let (second, _) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            second_handle,
            spend,
            REFERENCE_ISO,
            second_reservation,
            pledge_stake(),
        )
        .unwrap();

        assert_ne!(second, first);
        let credis = CredisContract::new(storage.clone());
        assert_eq!(
            credis
                .get_position(first)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Called
        );
        assert_eq!(
            credis
                .get_position(second)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Open
        );
    });
    teardown();
}

#[test]
fn issue_credis_rejects_zero_smart_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            Address::ZERO,
            B256::ZERO,
            [0u8; 32],
            REFERENCE_ISO,
            U256::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        assert!(err.to_string().contains("smart account"), "got: {err}");
    });
}

#[test]
fn oracle_call_survives_half_repayment_then_voids_the_unpaid_share() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);
        let credis = CredisContract::new(storage.clone());
        let issued = credis.get_position(position_id).unwrap();
        assert_eq!(issued.lifecycle_state().unwrap(), CredisState::Open);
        assert_eq!(view_pledged(&storage, alice()), issued.collateral);

        let mut oracle = OracleContract::new(storage.clone());
        oracle.config_is_initialized.write(true).unwrap();
        // Snapshots are supplied directly; no validator vote tally is needed.
        oracle.config_vote_period.write(0).unwrap();
        let pair = AddressPair::new_coen_to(issued.reference_currency);
        let price = issued.call_price + U256::ONE;
        let threshold_days = u64::from(issued.call_threshold) / DAY;
        assert!(threshold_days > 1);
        assert!(issued.call_threshold <= issued.call_window);
        let first_midnight = issued.issued_at - issued.issued_at % DAY;
        let tick = |timestamp| {
            advance_to(&storage, timestamp);
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(BLOCK_NUMBER, timestamp, CHAIN_ID),
                storage.clone(),
            );
            OracleLifecycle::begin_block(&ctx).unwrap();
            crate::called::run_daily(&ctx).unwrap();
        };

        // Only closed reference-currency VWAP days count. The current day's
        // price cannot call the position before the final qualifying rollover.
        for day in 0..threshold_days {
            let now = now_of(&storage);
            set_coen_rate_for(&storage, issued.reference_currency, price);
            oracle
                .write_snapshot(now, &[(pair, price, U256::ONE)])
                .unwrap();
            tick(now);
            assert_eq!(
                credis
                    .get_position(position_id)
                    .unwrap()
                    .lifecycle_state()
                    .unwrap(),
                CredisState::Open
            );
            let next_day = first_midnight + (day + 1) * DAY;
            tick(next_day);
            assert_eq!(
                oracle
                    .get_utc_day_vwap_for_pair(
                        last_closed_day(next_day),
                        oracle.pair_index_of(pair).unwrap()
                    )
                    .unwrap(),
                Some(price)
            );
            assert_eq!(
                credis
                    .get_position(position_id)
                    .unwrap()
                    .lifecycle_state()
                    .unwrap(),
                if day + 1 == threshold_days {
                    CredisState::Called
                } else {
                    CredisState::Open
                }
            );
        }

        let called = credis.get_position(position_id).unwrap();
        assert_eq!(called.called_at, now_of(&storage));
        assert!(credis.has_called_position(alice()).unwrap());
        let deadline = outbe_credis::settlement_deadline(&called);
        let half = issued.principal / U256::from(2u64);
        let interest = CredisContract::accrued_interest(&called, now_of(&storage)).unwrap();
        assert!(!interest.is_zero());
        assert_eq!(
            settle_principal(&storage, alice(), position_id, half),
            (half, interest)
        );
        let repaid = credis.get_position(position_id).unwrap();
        let unpaid_collateral = issued.collateral / U256::from(2u64);
        let released = issued.collateral - unpaid_collateral;
        assert_eq!(repaid.lifecycle_state().unwrap(), CredisState::Called);
        assert_eq!(repaid.outstanding, issued.principal - half);
        assert_eq!(repaid.collateral_locked, unpaid_collateral);
        assert_eq!(repaid.called_at, called.called_at);
        assert_eq!(outbe_credis::settlement_deadline(&repaid), deadline);
        assert!(credis.has_called_position(alice()).unwrap());
        assert_eq!(view_pledged(&storage, alice()), unpaid_collateral);
        assert_eq!(view_balance(&storage, alice()), released);

        let ledger = || {
            (
                view_balance(&storage, alice()),
                view_pledged(&storage, alice()),
                outbe_gratis::api::total_supply(storage.clone()).unwrap(),
                outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                PromisLimitContract::new(storage.clone())
                    .get_total_unallocated()
                    .unwrap(),
            )
        };
        let before = ledger();
        assert_eq!(
            before.2, issued.collateral,
            "repayment releases without burning"
        );
        assert_eq!(before.3, unpaid_collateral);
        let cohorts_before = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert!(!cohorts_before.is_empty(), "alice has a seeded cohort");

        tick(deadline - 1);
        assert_eq!(
            credis
                .get_position(position_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Called
        );
        assert_eq!(ledger(), before, "no burn before the settlement deadline");

        tick(deadline);
        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.lifecycle_state().unwrap(), CredisState::Void);
        assert!(position.outstanding.is_zero());
        assert!(position.collateral_locked.is_zero());
        assert_eq!(credis.active_len().unwrap(), 0);
        assert!(!credis.has_called_position(alice()).unwrap());
        // Burn only the pledged remainder; preserve the returned liquid half
        // and credit the same amount to the Promis reserve.
        let expected = (
            released,
            U256::ZERO,
            before.2 - unpaid_collateral,
            before.3 - unpaid_collateral,
            before.4 + unpaid_collateral,
        );
        assert_eq!(ledger(), expected);
        let cohorts_after = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        assert_ne!(
            cohorts_before, cohorts_after,
            "the burn records a sale cohort"
        );

        tick(deadline);
        assert_eq!(
            ledger(),
            expected,
            "a repeated sweep must not burn or credit twice"
        );
        assert_eq!(
            credis
                .get_position(position_id)
                .unwrap()
                .lifecycle_state()
                .unwrap(),
            CredisState::Void
        );
        assert_eq!(
            outbe_fidelity::FidelityContract::new(storage.clone())
                .cohorts_ct_of(alice())
                .unwrap(),
            cohorts_after
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

        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        // Settle in full inside the window.
        advance_to(&storage, called_at + NOTICE - DAY);
        settle_principal(&storage, alice(), position_id, pledge_stables());

        let deadline = called_at + NOTICE;
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

// ---------------------------------------------------------------------------
// The originating CCA's matching COEN stake
// ---------------------------------------------------------------------------

fn factory_balance(storage: &StorageHandle<'_>) -> U256 {
    storage
        .balance(outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS)
        .unwrap()
}

/// The stake must equal the pledged collateral exactly. Both directions are
/// rejected: under-staking would let a CCA originate cheaply, over-staking would
/// hand the borrower more than the collateral it is supposed to match.
#[test]
fn issue_credis_requires_the_stake_to_equal_the_collateral() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));

        let expected = pledge_stake();
        for (i, wrong) in [
            U256::ZERO,
            expected - U256::from(1u64),
            expected + U256::from(1u64),
        ]
        .into_iter()
        .enumerate()
        {
            // Each pledge consumes the next op nonce.
            let (handle, reservation_id) = pledge(&storage, alice(), i as u64 + 1);
            let spend = credis_spend_auth(alice(), handle, alice());
            let err = runtime::issue_credis(
                storage.clone(),
                cca(),
                alice(),
                handle,
                spend,
                REFERENCE_ISO,
                reservation_id,
                wrong,
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("attached COEN"),
                "stake {wrong} should be rejected, got: {err}"
            );
        }
    });
    teardown();
}

/// The stake is handed to the borrower's smart account at origination - the factory
/// keeps nothing and the CCA gets nothing back.
#[test]
fn issue_credis_pays_the_stake_to_the_smart_account() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        open(&storage, 1);

        assert_eq!(storage.balance(alice()).unwrap(), pledge_stake());
        assert_eq!(factory_balance(&storage), U256::ZERO, "nothing escrowed");
        assert_eq!(storage.balance(cca()).unwrap(), U256::ZERO);
    });
    teardown();
}

/// Settling the position in full does not claw the stake back: it stays with the
/// borrower, who may already have spent it.
#[test]
fn the_closing_settlement_does_not_return_the_stake() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        settle_principal(
            &storage,
            alice(),
            position_id,
            pledge_stables() / U256::from(2u64),
        );
        settle_principal(&storage, alice(), position_id, pledge_stables());

        assert_eq!(
            storage.balance(alice()).unwrap(),
            pledge_stake(),
            "the borrower keeps it"
        );
        assert_eq!(
            storage.balance(cca()).unwrap(),
            U256::ZERO,
            "never returned"
        );
        assert_eq!(factory_balance(&storage), U256::ZERO);
    });
    teardown();
}

/// A void burns the unpaid collateral but not the stake - that COEN left the protocol's
/// hands at origination.
#[test]
fn the_void_leaves_the_stake_with_the_smart_account() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let position_id = open(&storage, 1);

        let called_at = now_of(&storage);
        {
            let mut credis = CredisContract::new(storage.clone());
            assert!(credis.mark_called(position_id, called_at).unwrap());
        }

        let deadline = called_at + NOTICE;
        advance_to(&storage, deadline);
        finalize_through(&storage, deadline);
        assert_eq!(scan(&storage, deadline), 1);

        assert_eq!(
            storage.balance(alice()).unwrap(),
            pledge_stake(),
            "not burned with the collateral"
        );
        assert_eq!(
            storage.balance(cca()).unwrap(),
            U256::ZERO,
            "never returned"
        );
        assert_eq!(factory_balance(&storage), U256::ZERO);
    });
    teardown();
}

/// The loan is delivered by a call into the smart account, which would silently
/// succeed against a codeless address.
#[test]
fn issue_credis_rejects_an_undeployed_smart_account() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, _) = pledge(&storage, alice(), 1);
        // A valid auth bound to bob, so the rejection can only come from the
        // deployment guard and not from a bad authorization.
        let spend = credis_spend_auth(alice(), handle, bob());

        // bob was never bootstrapped, so it has no code.
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            bob(),
            handle,
            spend,
            REFERENCE_ISO,
            U256::ZERO,
            pledge_stake(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("not deployed"), "got: {err}");
    });
    teardown();
}

/// Moving the reference current price after the pledge does not reprice the entry.
/// The call anchor does follow max(previous-day VWAP, that current price).
#[test]
fn entry_price_stays_on_the_pledge_when_the_reference_price_moves() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        // Pledged at COEN/840 = 2.0, so entry = principal / gratis = 2.0.
        let (handle, reservation_id) = pledge(&storage, alice(), 1);

        // Both current prices move after acceptance. Neither can change the
        // principal, asset, collateral, entry or issuance currency on the ticket.
        set_coen_rate(&storage, U256::from(4_000_000u64));
        // The reference current price moves to 3.0. Yesterday's VWAP stays 2.0.
        set_coen_rate_for(&storage, REFERENCE_ISO, U256::from(3_000_000u64));

        let spend = credis_spend_auth(alice(), handle, alice());
        fund_stake(&storage, pledge_stake());
        let (position_id, amount_stables) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap();

        assert_eq!(amount_stables, pledge_stables());

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.collateral, pledge_cost());
        assert_eq!(position.principal, pledge_stables());
        assert_eq!(position.asset, asset());
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        assert_eq!(position.entry_price, oracle_rate());
        // max(2.0, 3.0) = 3.0; 3.0 * 1.64 = 4.92.
        assert_eq!(position.call_anchor_price, U256::from(3_000_000u64));
        assert_eq!(position.call_price, U256::from(4_920_000u64));
    });
    teardown();
}

#[test]
fn issue_credis_rejects_an_unregistered_reference_currency() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        fund_stake(&storage, pledge_stake());

        // 392 (JPY) has no COEN pair and is not in the reference registry: electing it
        // would seal a threshold the daily scan can never evaluate.
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            392,
            reservation_id,
            pledge_stake(),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("not a registered reference currency"),
            "got: {err}"
        );
    });
    teardown();
}

/// The previous day's VWAP outranks a lower current price. Entry price still
/// comes from the pledge, in the issuance currency.
#[test]
fn call_anchor_uses_the_previous_day_vwap_when_it_is_higher() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        set_vwap(
            &storage,
            last_closed_day(now_of(&storage)),
            U256::from(2_500_000u64),
        );

        let spend = credis_spend_auth(alice(), handle, alice());
        fund_stake(&storage, pledge_stake());
        let (position_id, _) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap();

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.entry_price, oracle_rate());
        // max(2.50, 2.00) = 2.50; 2.50 * 1.64 = 4.10.
        assert_eq!(position.call_anchor_price, U256::from(2_500_000u64));
        assert_eq!(position.call_price, U256::from(4_100_000u64));
    });
    teardown();
}

/// principal 1,000 USD, gratis 500, previous-day COEN/EUR VWAP 1.80, current
/// price 1.90. Entry stays 2.00 USD; the call is 3.116 EUR.
#[test]
fn worked_example_keeps_entry_and_call_in_different_currencies() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        let principal = U256::from(1_000_000_000u64);
        let gratis = U256::from(500_000_000u64);
        bootstrap(&storage, gratis);
        let reservation_id = seed_reservation(&storage, alice(), principal);
        let (handle, gratis_cost) = pledge_fixture(
            storage.clone(),
            alice(),
            principal,
            asset(),
            U256::MAX,
            auth(GratisOp::Pledge, alice(), principal, 1),
        )
        .unwrap();
        assert_eq!(gratis_cost, gratis);

        set_vwap(
            &storage,
            last_closed_day(now_of(&storage)),
            U256::from(1_800_000u64),
        );
        set_coen_rate_for(&storage, REFERENCE_ISO, U256::from(1_900_000u64));

        let stake = checked_protocol_to_native(gratis).unwrap();
        fund_stake(&storage, stake);
        let (position_id, _) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            credis_spend_auth(alice(), handle, alice()),
            REFERENCE_ISO,
            reservation_id,
            stake,
        )
        .unwrap();

        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.entry_price, U256::from(2_000_000u64));
        assert_eq!(position.call_anchor_price, U256::from(1_900_000u64));
        assert_eq!(position.call_price, U256::from(3_116_000u64));
        assert_eq!(position.issuance_currency, ISSUANCE_ISO);
        assert_eq!(position.reference_currency, REFERENCE_ISO);
    });
    teardown();
}

#[test]
fn issue_credis_rejects_a_missing_previous_day_vwap() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        set_vwap(&storage, last_closed_day(now_of(&storage)), U256::ZERO);
        fund_stake(&storage, pledge_stake());
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            credis_spend_auth(alice(), handle, alice()),
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("previous closed UTC-day VWAP"),
            "got: {err}"
        );
    });
    teardown();
}

#[test]
fn issue_credis_rejects_a_stale_current_price() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        let now = now_of(&storage);
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::AddressPair::new_coen_to(REFERENCE_ISO),
            oracle_rate(),
            1,
            now - outbe_oracle::constants::FX_RATE_MAX_AGE_SECONDS - 1,
        )
        .unwrap();
        fund_stake(&storage, pledge_stake());
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            credis_spend_auth(alice(), handle, alice()),
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("stale"), "got: {err}");
    });
    teardown();
}

#[test]
fn failed_origination_preserves_the_pledge_and_cca_weight_and_exit_freezes_new_positions() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let (handle, reservation_id) = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        // Model the EVM call frame: stake validation fails after pledge consumption,
        // so the enclosing transaction must restore the ticket.
        assert!(storage
            .with_checkpoint(|| runtime::issue_credis(
                storage.clone(),
                cca(),
                alice(),
                handle,
                spend,
                REFERENCE_ISO,
                reservation_id,
                U256::ZERO
            ))
            .is_err());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(
                &storage,
                cca(),
                outbe_primitives::time::timestamp_to_date_key(CREATED_AT)
            )
            .unwrap(),
            U256::ZERO
        );
        fund_stake(&storage, pledge_stake());
        let (id, _) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap();
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(
                &storage,
                cca(),
                outbe_primitives::time::timestamp_to_date_key(CREATED_AT)
            )
            .unwrap(),
            pledge_cost()
        );
        outbe_ccaregistry::runtime::unbond(storage.clone(), cca()).unwrap();
        assert!(runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake()
        )
        .is_err());
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(id)
                .unwrap()
                .outstanding,
            pledge_stables()
        );
    });
}

#[test]
fn issue_credis_rejects_a_missing_or_mismatched_reservation() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));
        fund_stake(&storage, pledge_stake());

        let (handle, _) = pledge(&storage, alice(), 1);
        let spend = credis_spend_auth(alice(), handle, alice());
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            U256::ZERO,
            pledge_stake(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("reservation not found"), "{err}");

        let (handle, _) = pledge(&storage, alice(), 2);
        let spend = credis_spend_auth(alice(), handle, alice());
        let err = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            seed_reservation_at(
                &storage,
                cca(),
                bob(),
                asset(),
                pledge_stables(),
                CREATED_AT + 15 * 60,
            ),
            pledge_stake(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("reservation account mismatch"),
            "{err}"
        );

        let (handle, _) = pledge(&storage, alice(), 3);
        let spend = credis_spend_auth(alice(), handle, alice());
        let too_small = seed_reservation_at(
            &storage,
            cca(),
            alice(),
            asset(),
            pledge_stables() - U256::from(1u64),
            CREATED_AT + 15 * 60,
        );
        let err = storage
            .with_checkpoint(|| {
                runtime::issue_credis(
                    storage.clone(),
                    cca(),
                    alice(),
                    handle,
                    spend,
                    REFERENCE_ISO,
                    too_small,
                    pledge_stake(),
                )
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("reservation amount is below"),
            "{err}"
        );
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

#[test]
fn issue_credis_accepts_a_larger_reservation() {
    let mut storage = env();
    StorageHandle::enter(&mut storage, |storage| {
        bootstrap(&storage, pledge_cost());
        let reservation_id =
            seed_reservation(&storage, alice(), pledge_stables() * U256::from(2u64));
        let (handle, gratis_cost) = pledge_fixture(
            storage.clone(),
            alice(),
            pledge_stables(),
            asset(),
            U256::MAX,
            auth(
                outbe_tee::protocol::GratisOp::Pledge,
                alice(),
                pledge_stables(),
                1,
            ),
        )
        .unwrap();
        assert_eq!(gratis_cost, pledge_cost());
        let spend = credis_spend_auth(alice(), handle, alice());
        fund_stake(&storage, pledge_stake());
        let (position_id, amount) = runtime::issue_credis(
            storage.clone(),
            cca(),
            alice(),
            handle,
            spend,
            REFERENCE_ISO,
            reservation_id,
            pledge_stake(),
        )
        .unwrap();
        assert_eq!(amount, pledge_stables());
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap()
                .principal,
            pledge_stables()
        );
    });
    teardown();
}
