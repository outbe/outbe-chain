//! Repayment, forfeiture and stake behaviour of Credis backed by a direct pledge.
use crate::{precompile::ICredisFactory, runtime, tests::common::*};
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_credis::{CredisContract, CredisState};
use outbe_gratisfactory::runtime::cancel_pledge;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_tee::protocol::GratisOp;
use outbe_vaultrouter::VaultRouterContract;

fn unallocated(storage: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(storage.clone())
        .get_total_unallocated()
        .unwrap()
}

fn record(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Credis {
    CredisContract::new(storage.clone()).get_credis(id).unwrap()
}

/// The collateral left on `id`, checked against the Credis's own accounting.
fn collateral(storage: &StorageHandle<'_>, id: U256) -> U256 {
    let remaining = outbe_gratisfactory::api::collateral_of(storage, id)
        .unwrap()
        .remaining_minor;
    assert_eq!(remaining, record(storage, id).outstanding_gratis_minor);
    remaining
}

#[test]
fn settle_runs_immediately_after_opening() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let sealed = record(&storage, id);
        settle_principal(&storage, alice(), id, U256::from(500_000u64));
        settle_principal(&storage, alice(), id, U256::from(500_000u64));
        let p = record(&storage, id);
        assert_eq!(p.outstanding_principal_minor, U256::from(1_000_000u64));
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Issued);
        assert_eq!(p.entry_price_minor, sealed.entry_price_minor);
        assert_eq!(p.call_price_minor, sealed.call_price_minor);
        assert_eq!(p.issued_at, sealed.issued_at);
    });
    teardown();
}

#[test]
fn settlement_releases_collateral_proportionally_and_closes_without_dust() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let half = pledge_stables() / U256::from(2u64);
        let (principal_paid, interest_paid) = settle_principal(&storage, alice(), id, half);
        assert_eq!(principal_paid, half);
        assert!(!interest_paid.is_zero());
        let p = record(&storage, id);
        assert_eq!(p.outstanding_principal_minor, half);
        assert_eq!(p.outstanding_gratis_minor, pledge_cost() / U256::from(2u64));
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() / U256::from(2u64)
        );

        advance_to(&storage, CREATED_AT + 60 * DAY);
        settle_principal(&storage, alice(), id, half);
        let p = record(&storage, id);
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Settled);
        assert!(p.outstanding_gratis_minor.is_zero());
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

#[test]
fn rounded_returns_can_exhaust_collateral_before_repayment_or_forfeiture() {
    for forfeit in [false, true] {
        let collateral = U256::from(6u64);
        let principal = U256::from(13u64);
        let mut provider = env();
        provider.stub_sub_call_at_selector(
            VAULT_ROUTER_ADDRESS,
            outbe_vaultrouter::api::IVaultRouter::releaseReservationCall::SELECTOR,
            Bytes::from(principal.to_be_bytes::<32>().to_vec()),
        );
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, collateral);
            deploy_smart_account(&storage, bob());
            let reservation_id = seed_reservation(&storage, bob(), alice(), principal);
            let mut reservation =
                outbe_vaultrouter::api::reservation_of(&storage, reservation_id).unwrap();
            reservation.gratis_minor = collateral;
            VaultRouterContract::new(storage.clone())
                .reservations
                .update(&reservation)
                .unwrap();
            outbe_gratisfactory::runtime::pledge_gratis(
                storage.clone(),
                alice(),
                reservation_id,
                auth(GratisOp::Pledge, alice(), collateral, 1),
            )
            .unwrap();
            let stake = outbe_primitives::units::checked_protocol_to_native(collateral).unwrap();
            fund_stake(&storage, stake);
            let (id, disbursed) =
                runtime::issue_credis(storage.clone(), cca(), reservation_id, stake).unwrap();
            assert_eq!(disbursed, principal);
            assert_eq!(view_pledged(&storage, alice()), collateral);

            // Subunit interest floors to zero; returns ceil and then cap at the remainder.
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
            let p = record(&storage, id);
            assert_eq!(p.outstanding_principal_minor, U256::from(3u64));
            assert!(p.outstanding_gratis_minor.is_zero());
            assert_eq!(p.lifecycle_state().unwrap(), CredisState::Issued);

            let expected = if forfeit {
                let called_at = now_of(&storage);
                CredisContract::new(storage.clone())
                    .mark_called(id, called_at)
                    .unwrap();
                let expired_at = called_at + NOTICE + HOUR;
                advance_to(&storage, expired_at);
                finalize_through(&storage, expired_at);
                assert_eq!(expire(&storage, expired_at), 1);
                assert_eq!(expire(&storage, expired_at), 0);
                CredisState::Forfeited
            } else {
                runtime::settle(storage.clone(), bob(), id, U256::from(3u64)).unwrap();
                CredisState::Settled
            };
            let p = record(&storage, id);
            assert_eq!(p.lifecycle_state().unwrap(), expected);
            assert!(p.outstanding_principal_minor.is_zero());
            assert_eq!(view_balance(&storage, alice()), collateral);
            assert_eq!(view_balance(&storage, bob()), U256::ZERO);
            assert_eq!(
                view_balance(&storage, alice()) + view_pledged(&storage, alice()),
                collateral
            );
            assert_eq!(
                outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                U256::ZERO
            );
            assert_eq!(unallocated(&storage), U256::ZERO);
            assert_eq!(
                CredisContract::new(storage)
                    .call_bin_tree_root
                    .read(&REFERENCE_ISO)
                    .unwrap(),
                U256::ZERO
            );
        });
        teardown();
    }
}

#[test]
fn the_settle_abi_returns_the_principal_and_interest_split() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        assert!(!interest.is_zero());
        let principal = pledge_stables() / U256::from(4u64);
        let data = ICredisFactory::settleCredisCall {
            credisId: id,
            amountMinor: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
        let decoded = ICredisFactory::settleCredisCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded.principalPaidMinor, principal);
        assert_eq!(decoded.interestPaidMinor, interest);
        assert_eq!(
            record(&storage, id).outstanding_principal_minor,
            pledge_stables() - principal
        );
    });
    teardown();
}

#[test]
fn settle_takes_only_what_the_credis_needs() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        let (principal_paid, interest_paid) = runtime::settle(
            storage.clone(),
            alice(),
            id,
            pledge_stables() * U256::from(1_000u64),
        )
        .unwrap();
        assert_eq!(principal_paid, pledge_stables());
        assert_eq!(interest_paid, interest);
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn issue_credis_allows_an_owner_with_an_unresolved_call() {
    let mut provider = env();
    let first = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let first = open(&storage, 1);
        assert!(CredisContract::new(storage.clone())
            .mark_called(first, now_of(&storage))
            .unwrap());
        first
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let second = open(&storage, 2);
        assert_ne!(second, first);
        assert_eq!(
            record(&storage, first).lifecycle_state().unwrap(),
            CredisState::Called
        );
        assert_eq!(
            record(&storage, second).lifecycle_state().unwrap(),
            CredisState::Issued
        );
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() * U256::from(2u64)
        );
    });
    teardown();
}

#[test]
fn one_source_backs_several_positions_and_unused_pledges() {
    let mut provider = env();
    let first = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));
        open(&storage, 1)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let second = open(&storage, 2);
        let unused = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), unused, 3);
        let backing = |storage: &StorageHandle<'_>| {
            record(storage, first).outstanding_gratis_minor
                + record(storage, second).outstanding_gratis_minor
        };
        assert_eq!(
            view_pledged(&storage, alice()),
            backing(&storage) + pledge_cost()
        );

        settle_principal(&storage, bob(), first, pledge_stables() / U256::from(2u64));
        assert_eq!(
            collateral(&storage, first),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(collateral(&storage, second), pledge_cost());
        assert_eq!(
            view_pledged(&storage, alice()),
            backing(&storage) + pledge_cost()
        );
        CredisContract::new(storage.clone())
            .mark_called(second, CREATED_AT)
            .unwrap();
        advance_to(&storage, CREATED_AT + NOTICE + 1);
        runtime::forfeit_credis(storage.clone(), second).unwrap();
        assert_eq!(
            view_pledged(&storage, alice()),
            backing(&storage) + pledge_cost()
        );
        cancel_pledge(storage.clone(), alice(), unused).unwrap();
        assert_eq!(view_pledged(&storage, alice()), backing(&storage));
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            backing(&storage)
        );
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() + pledge_cost() / U256::from(2u64)
        );
    });
    teardown();
}

#[test]
fn a_credis_settled_at_the_deadline_is_never_forfeited() {
    let mut provider = env();
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        open(&storage, 1)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let bystander = open(&storage, 2);
        let called_at = now_of(&storage);
        assert!(CredisContract::new(storage.clone())
            .mark_called(id, called_at)
            .unwrap());
        advance_to(&storage, called_at + NOTICE);
        settle_principal(&storage, alice(), id, pledge_stables());
        let after = called_at + NOTICE + DAY;
        advance_to(&storage, after);
        finalize_through(&storage, after);
        assert_eq!(expire(&storage, after), 0);
        let credis = CredisContract::new(storage.clone());
        let open = record(&storage, bystander);
        let bins = outbe_credis::CallBins(&credis, open.reference_currency);
        let bin = outbe_primitives::call_bins::price_to_bin(open.call_price_minor).unwrap();
        assert_eq!(outbe_primitives::call_bins::len(&bins, bin).unwrap(), 1);
        assert_eq!(
            outbe_primitives::call_bins::entry_at(&bins, bin, 0).unwrap(),
            bystander
        );
        assert_eq!(
            record(&storage, bystander).lifecycle_state().unwrap(),
            CredisState::Issued
        );
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            view_balance(&storage, alice()) + view_pledged(&storage, alice()),
            pledge_cost() * U256::from(2u64)
        );
        assert_eq!(unallocated(&storage), U256::ZERO);
    });
    teardown();
}

#[test]
fn repayment_deadline_is_enforced_before_cleanup_through_the_abi() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let mut credis = CredisContract::new(storage.clone());
        credis.mark_called(id, CREATED_AT).unwrap();
        let deadline = outbe_credis::settlement_deadline(&credis.get_credis(id).unwrap());
        let principal = pledge_stables() / U256::from(4);
        for now in [deadline - 1, deadline] {
            advance_to(&storage, now);
            let interest =
                CredisContract::accrued_interest(&credis.get_credis(id).unwrap(), now).unwrap();
            let data = ICredisFactory::settleCredisCall {
                credisId: id,
                amountMinor: principal + interest,
            }
            .abi_encode();
            let out =
                crate::precompile::dispatch(storage.clone(), &data, bob(), U256::ZERO).unwrap();
            assert_eq!(
                ICredisFactory::settleCredisCall::abi_decode_returns(&out)
                    .unwrap()
                    .principalPaidMinor,
                principal
            );
        }
        let before = credis.get_credis(id).unwrap();
        let balance = view_balance(&storage, alice());
        let pledged = view_pledged(&storage, alice());
        assert_eq!(pledged, pledge_cost() / U256::from(2));
        advance_to(&storage, deadline + HOUR);
        let data = ICredisFactory::settleCredisCall {
            credisId: id,
            amountMinor: U256::MAX,
        }
        .abi_encode();
        let err =
            crate::precompile::dispatch(storage.clone(), &data, bob(), U256::ZERO).unwrap_err();
        assert!(
            err.to_string().contains("settlement deadline has passed"),
            "{err}"
        );
        assert_eq!(credis.get_credis(id).unwrap(), before);
        assert_eq!(view_balance(&storage, alice()), balance);
        assert_eq!(view_pledged(&storage, alice()), pledged);

        runtime::forfeit_credis(storage.clone(), id).unwrap();
        assert_eq!(view_balance(&storage, alice()), balance);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(unallocated(&storage), pledged);
    });
    teardown();
}

#[test]
fn issue_credis_requires_the_stake_to_equal_the_collateral() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        fund_stake(&storage, pledge_stake() + U256::ONE);
        for wrong in [
            U256::ZERO,
            pledge_stake() - U256::ONE,
            pledge_stake() + U256::ONE,
        ] {
            let err = runtime::issue_credis(storage.clone(), cca(), id, wrong).unwrap_err();
            assert!(err.to_string().contains("attached COEN"), "{wrong}: {err}");
        }
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn the_stake_stays_with_the_smart_account_through_settlement_and_forfeit() {
    for forfeit in [false, true] {
        let mut provider = env();
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let id = open(&storage, 1);
            assert_eq!(storage.balance(alice()).unwrap(), pledge_stake());
            assert_eq!(storage.balance(CREDIS_FACTORY_ADDRESS).unwrap(), U256::ZERO);
            if forfeit {
                let called_at = now_of(&storage);
                CredisContract::new(storage.clone())
                    .mark_called(id, called_at)
                    .unwrap();
                let deadline = called_at + NOTICE + HOUR;
                advance_to(&storage, deadline);
                finalize_through(&storage, deadline);
                assert_eq!(expire(&storage, deadline), 1);
            } else {
                settle_principal(&storage, alice(), id, pledge_stables());
            }
            assert_eq!(storage.balance(alice()).unwrap(), pledge_stake());
            assert_eq!(storage.balance(cca()).unwrap(), U256::ZERO);
            assert_eq!(storage.balance(CREDIS_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        });
        teardown();
    }
}

#[test]
fn failed_origination_keeps_the_pledge_and_cca_weight_and_exit_freezes_new_credis() {
    let mut provider = env();
    let (credis_id, next) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        let day = outbe_primitives::time::timestamp_to_date_key(CREATED_AT);
        // The unfunded stake transfer fails after the pledge was handed to Credis.
        assert!(runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).is_err());
        assert_eq!(
            outbe_gratisfactory::runtime::pledge_of(&storage, id)
                .unwrap()
                .source,
            alice()
        );
        assert_eq!(
            CredisContract::new(storage.clone())
                .call_bin_tree_root
                .read(&REFERENCE_ISO)
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(&storage, cca(), day).unwrap(),
            U256::ZERO
        );
        fund_stake(&storage, pledge_stake());
        let (credis_id, _) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(&storage, cca(), day).unwrap(),
            pledge_cost()
        );
        outbe_ccaregistry::runtime::unbond(storage.clone(), cca()).unwrap();
        let next = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), next, 2);
        fund_stake(&storage, pledge_stake());
        (credis_id, next)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let err = runtime::issue_credis(storage.clone(), cca(), next, pledge_stake()).unwrap_err();
        assert!(err.to_string().contains("CCA is not active"), "{err}");
        assert_eq!(
            record(&storage, credis_id).outstanding_principal_minor,
            pledge_stables()
        );
        assert_eq!(
            outbe_gratisfactory::runtime::pledge_of(&storage, next)
                .unwrap()
                .source,
            alice()
        );
    });
    teardown();
}

#[test]
fn a_half_repaid_call_voids_only_the_unpaid_backing_of_another_accounts_source() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        deploy_smart_account(&storage, bob());
        let id = seed_reservation(&storage, bob(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        let unused = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), unused, 2);
        fund_stake(&storage, pledge_stake());
        let credis_id = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake())
            .unwrap()
            .0;
        let half = pledge_cost() / U256::from(2u64);
        settle_principal(
            &storage,
            bob(),
            credis_id,
            pledge_stables() / U256::from(2u64),
        );
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost() + half);
        assert_eq!(collateral(&storage, credis_id), half);

        let called_at = now_of(&storage);
        assert!(CredisContract::new(storage.clone())
            .mark_called(credis_id, called_at)
            .unwrap());
        let deadline = outbe_credis::settlement_deadline(&record(&storage, credis_id));
        for at in [deadline - 1, deadline] {
            advance_to(&storage, at);
            finalize_through(&storage, at);
            assert_eq!(expire(&storage, at), 0);
            assert_eq!(
                record(&storage, credis_id).lifecycle_state().unwrap(),
                CredisState::Called
            );
        }
        let supply = view_balance(&storage, alice()) + view_pledged(&storage, alice());
        advance_to(&storage, deadline + HOUR);
        finalize_through(&storage, deadline + HOUR);
        assert_eq!(expire(&storage, deadline + HOUR), 1);
        assert_eq!(expire(&storage, deadline + HOUR), 0);
        assert_eq!(
            record(&storage, credis_id).lifecycle_state().unwrap(),
            CredisState::Forfeited
        );
        assert_eq!(collateral(&storage, credis_id), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(
            view_balance(&storage, alice()) + view_pledged(&storage, alice()),
            supply - half
        );
        assert_eq!(unallocated(&storage), half);

        cancel_pledge(storage.clone(), alice(), unused).unwrap();
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), pledge_cost() + half);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
    teardown();
}
