//! Repayment, forfeiture and stake behaviour of Credis backed by a direct pledge.
use crate::rules;
use crate::{precompile::ICredisFactory, runtime, tests::*};
use alloy_primitives::{b256, keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_credis::precompile::ICredis;
use outbe_credis::{CredisContract, CredisState};
use outbe_gratisfactory::runtime::cancel_pledge;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
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

/// What a reader of `id` sees now.
fn shown(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Outcome {
    let now = storage.timestamp().unwrap().to::<u64>();
    outbe_credis::outcome(&record(storage, id), now).unwrap()
}

/// The collateral left on `id`, checked against the Credis's own accounting.
fn collateral(storage: &StorageHandle<'_>, id: U256) -> U256 {
    let remaining = outbe_gratisfactory::api::collateral_of(storage, id)
        .unwrap()
        .remaining_minor;
    assert_eq!(remaining, shown(storage, id).outstanding_gratis_minor);
    remaining
}

#[test]
fn settle_runs_immediately_after_issuance() {
    let mut provider = env();
    let (id, sealed) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        (id, record(&storage, id))
    });
    settle_principal(&mut provider, alice(), id, U256::from(500_000u64));
    settle_principal(&mut provider, alice(), id, U256::from(500_000u64));
    StorageHandle::enter(&mut provider, |storage| {
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
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        id
    });
    let half = pledge_stables() / U256::from(2u64);
    let (principal_paid, interest_paid) = settle_principal(&mut provider, alice(), id, half);
    assert_eq!(principal_paid, half);
    assert!(!interest_paid.is_zero());
    StorageHandle::enter(&mut provider, |storage| {
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
    });
    settle_principal(&mut provider, alice(), id, half);
    StorageHandle::enter(&mut provider, |storage| {
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
        let id = StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, collateral);
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
            id
        });
        for (payment, remaining) in [(1u64, 5u64), (1, 4), (1, 3), (1, 2), (5, 0), (1, 0)] {
            let payment = U256::from(payment);
            assert_eq!(
                paying(&mut provider, id, payment, |storage| {
                    runtime::settle_credis(storage, bob(), id, payment).unwrap()
                }),
                (payment, U256::ZERO)
            );
            StorageHandle::enter(&mut provider, |storage| {
                assert_eq!(view_pledged(&storage, alice()), U256::from(remaining));
                assert_eq!(
                    view_balance(&storage, alice()),
                    collateral - U256::from(remaining)
                );
                assert_eq!(
                    outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                    U256::from(remaining)
                );
            });
        }
        StorageHandle::enter(&mut provider, |storage| {
            let p = record(&storage, id);
            assert_eq!(p.outstanding_principal_minor, U256::from(3u64));
            assert!(p.outstanding_gratis_minor.is_zero());
            assert_eq!(p.lifecycle_state().unwrap(), CredisState::Issued);
        });

        let expected = if forfeit {
            StorageHandle::enter(&mut provider, |storage| {
                let called_at = now_of(&storage);
                rules::mark_called(&mut CredisContract::new(storage.clone()), id, called_at)
                    .unwrap();
                let expired_at = called_at + NOTICE + HOUR;
                advance_to(&storage, expired_at);
                finalize_through(&storage, expired_at);
                assert_eq!(expire(&storage, expired_at), 1);
                assert_eq!(expire(&storage, expired_at), 0);
            });
            CredisState::Forfeited
        } else {
            let rest = U256::from(3u64);
            paying(&mut provider, id, rest, |storage| {
                runtime::settle_credis(storage, bob(), id, rest).unwrap()
            });
            CredisState::Settled
        };
        StorageHandle::enter(&mut provider, |storage| {
            let p = record(&storage, id);
            assert_eq!(p.lifecycle_state().unwrap(), expected);
            assert!(shown(&storage, id).outstanding_principal_minor.is_zero());
            if forfeit {
                // The rounding already returned every Gratis: nothing is burned.
                let outcome = shown(&storage, id);
                assert_eq!(outcome.principal_written_off_minor, U256::from(3u64));
                assert!(outcome.gratis_burned_minor.is_zero());
                assert_eq!(
                    outcome.gratis_returned_minor,
                    record(&storage, id).gratis_minor
                );
            }
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
    let principal = pledge_stables() / U256::from(4u64);
    let (id, interest) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        assert!(!interest.is_zero());
        (id, interest)
    });
    let decoded = paying(&mut provider, id, interest + principal, |storage| {
        let data = ICredisFactory::settleCredisCall {
            credisId: id,
            amountMinor: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage, &data, alice(), U256::ZERO).unwrap();
        ICredisFactory::settleCredisCall::abi_decode_returns(&out).unwrap()
    });
    assert_eq!(decoded.principalPaidMinor, principal);
    assert_eq!(decoded.interestPaidMinor, interest);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            record(&storage, id).outstanding_principal_minor,
            pledge_stables() - principal
        );
    });
    teardown();
}

#[test]
fn quote_settlement_is_the_payoff_that_closes_the_credis() {
    let mut provider = env();
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        id
    });
    let quote = |storage: &StorageHandle<'_>| {
        let data = ICredisFactory::quoteSettlementCall { credisId: id }.abi_encode();
        let out = crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
        ICredisFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap()
    };
    let payoff = StorageHandle::enter(&mut provider, |storage| {
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        let payoff = quote(&storage);
        assert_eq!(payoff, pledge_stables() + interest);
        payoff
    });

    paying(&mut provider, id, payoff, |storage| {
        runtime::settle_credis(storage, alice(), id, payoff).unwrap()
    });
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            record(&storage, id).lifecycle_state().unwrap(),
            CredisState::Settled
        );
        assert!(quote(&storage).is_zero());
    });
    teardown();
}

#[test]
fn a_called_credis_quotes_its_payoff_until_the_deadline_and_nothing_after() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        assert!(
            rules::mark_called(&mut CredisContract::new(storage.clone()), id, CREATED_AT).unwrap()
        );
        let quote = |storage: &StorageHandle<'_>| {
            let data = ICredisFactory::quoteSettlementCall { credisId: id }.abi_encode();
            let out =
                crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
            ICredisFactory::quoteSettlementCall::abi_decode_returns(&out).unwrap()
        };
        let deadline = CREATED_AT + NOTICE;
        advance_to(&storage, deadline);
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        assert_eq!(quote(&storage), pledge_stables() + interest);

        advance_to(&storage, deadline + 1);
        assert!(quote(&storage).is_zero());
    });
    teardown();
}

#[test]
fn settle_takes_only_what_the_credis_needs() {
    let mut provider = env();
    let (id, interest) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_interest(&record(&storage, id), now_of(&storage)).unwrap();
        (id, interest)
    });
    let offered = pledge_stables() * U256::from(1_000u64);
    let (principal_paid, interest_paid) = paying(&mut provider, id, offered, |storage| {
        runtime::settle_credis(storage, alice(), id, offered).unwrap()
    });
    assert_eq!(principal_paid, pledge_stables());
    assert_eq!(interest_paid, interest);
    StorageHandle::enter(&mut provider, |storage| {
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
        assert!(rules::mark_called(
            &mut CredisContract::new(storage.clone()),
            first,
            now_of(&storage)
        )
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
fn one_source_backs_several_credis_and_unused_pledges() {
    let mut provider = env();
    let first = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));
        open(&storage, 1)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    let (second, unused) = StorageHandle::enter(&mut provider, |storage| {
        let second = open(&storage, 2);
        let unused = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), unused, 3);
        (second, unused)
    });
    let backing = |storage: &StorageHandle<'_>| {
        shown(storage, first).outstanding_gratis_minor
            + shown(storage, second).outstanding_gratis_minor
    };
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            view_pledged(&storage, alice()),
            backing(&storage) + pledge_cost()
        );
    });

    settle_principal(
        &mut provider,
        bob(),
        first,
        pledge_stables() / U256::from(2u64),
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            collateral(&storage, first),
            pledge_cost() / U256::from(2u64)
        );
        assert_eq!(collateral(&storage, second), pledge_cost());
        assert_eq!(
            view_pledged(&storage, alice()),
            backing(&storage) + pledge_cost()
        );
        rules::mark_called(
            &mut CredisContract::new(storage.clone()),
            second,
            CREATED_AT,
        )
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
    let (bystander, called_at) = StorageHandle::enter(&mut provider, |storage| {
        let bystander = open(&storage, 2);
        let called_at = now_of(&storage);
        assert!(
            rules::mark_called(&mut CredisContract::new(storage.clone()), id, called_at).unwrap()
        );
        advance_to(&storage, called_at + NOTICE);
        (bystander, called_at)
    });
    settle_principal(&mut provider, alice(), id, pledge_stables());
    StorageHandle::enter(&mut provider, |storage| {
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
    let (id, deadline) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let mut credis = CredisContract::new(storage.clone());
        rules::mark_called(&mut credis, id, CREATED_AT).unwrap();
        (
            id,
            outbe_credis::settlement_deadline(&credis.get_credis(id).unwrap()),
        )
    });
    let principal = pledge_stables() / U256::from(4);
    for now in [deadline - 1, deadline] {
        let amount = StorageHandle::enter(&mut provider, |storage| {
            advance_to(&storage, now);
            interest_plus(&storage, id, principal)
        });
        let out = paying(&mut provider, id, amount, |storage| {
            let data = ICredisFactory::settleCredisCall {
                credisId: id,
                amountMinor: amount,
            }
            .abi_encode();
            crate::precompile::dispatch(storage, &data, bob(), U256::ZERO).unwrap()
        });
        assert_eq!(
            ICredisFactory::settleCredisCall::abi_decode_returns(&out)
                .unwrap()
                .principalPaidMinor,
            principal
        );
    }
    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
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
fn the_stake_stays_with_the_owner_through_settlement_and_forfeit() {
    for forfeit in [false, true] {
        let mut provider = env();
        let id = StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let id = open(&storage, 1);
            assert_eq!(storage.balance(alice()).unwrap(), pledge_stake());
            assert_eq!(storage.balance(CREDIS_FACTORY_ADDRESS).unwrap(), U256::ZERO);
            id
        });
        if forfeit {
            StorageHandle::enter(&mut provider, |storage| {
                let called_at = now_of(&storage);
                rules::mark_called(&mut CredisContract::new(storage.clone()), id, called_at)
                    .unwrap();
                let deadline = called_at + NOTICE + HOUR;
                advance_to(&storage, deadline);
                finalize_through(&storage, deadline);
                assert_eq!(expire(&storage, deadline), 1);
            });
        } else {
            settle_principal(&mut provider, alice(), id, pledge_stables());
        }
        StorageHandle::enter(&mut provider, |storage| {
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
fn a_half_paid_call_forfeits_only_the_unpaid_backing_of_another_accounts_source() {
    let mut provider = env();
    let (credis_id, unused) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let id = seed_reservation(&storage, bob(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        let unused = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), unused, 2);
        fund_stake(&storage, pledge_stake());
        let credis_id = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake())
            .unwrap()
            .0;
        (credis_id, unused)
    });
    let half = pledge_cost() / U256::from(2u64);
    settle_principal(
        &mut provider,
        bob(),
        credis_id,
        pledge_stables() / U256::from(2u64),
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost() + half);
        assert_eq!(collateral(&storage, credis_id), half);

        let called_at = now_of(&storage);
        assert!(rules::mark_called(
            &mut CredisContract::new(storage.clone()),
            credis_id,
            called_at
        )
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

fn forfeit_through_abi(
    storage: &StorageHandle<'_>,
    caller: Address,
    id: U256,
    value: U256,
) -> outbe_primitives::error::Result<ICredisFactory::forfeitCredisReturn> {
    let data = ICredisFactory::forfeitCredisCall { credisId: id }.abi_encode();
    let out = crate::precompile::dispatch(storage.clone(), &data, caller, value)?;
    Ok(ICredisFactory::forfeitCredisCall::abi_decode_returns(&out).unwrap())
}

#[test]
fn the_owner_forfeits_an_issued_credis_burning_its_sources_pledge() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let id = seed_reservation(&storage, bob(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        fund_stake(&storage, pledge_stake());
        let credis_id = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake())
            .unwrap()
            .0;
        let day = outbe_primitives::time::timestamp_to_date_key(CREATED_AT);
        let liquid = view_balance(&storage, alice());

        for stranger in [alice(), cca()] {
            let err = forfeit_through_abi(&storage, stranger, credis_id, U256::ZERO)
                .err()
                .unwrap();
            assert!(err.to_string().contains("only the Credis owner"), "{err}");
        }
        assert!(forfeit_through_abi(&storage, bob(), credis_id, U256::from(1u64)).is_err());

        let out = forfeit_through_abi(&storage, bob(), credis_id, U256::ZERO).unwrap();
        assert_eq!(out.gratisBurnedMinor, pledge_cost());
        assert_eq!(out.principalWrittenOffMinor, pledge_stables());
        assert_eq!(
            record(&storage, credis_id).lifecycle_state().unwrap(),
            CredisState::Forfeited
        );
        assert_eq!(collateral(&storage, credis_id), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), liquid);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(unallocated(&storage), pledge_cost());
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(&storage, cca(), day).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            CredisContract::new(storage.clone())
                .call_bin_tree_root
                .read(&REFERENCE_ISO)
                .unwrap(),
            U256::ZERO
        );

        assert!(forfeit_through_abi(&storage, bob(), credis_id, U256::ZERO).is_err());
        assert!(
            runtime::settle_credis(storage.clone(), bob(), credis_id, U256::from(1u64)).is_err()
        );
        assert_eq!(unallocated(&storage), pledge_cost());
    });
    teardown();
}

#[test]
fn the_owners_forfeit_of_a_lapsed_call_leaves_the_sweep_nothing() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let called_at = now_of(&storage);
        assert!(
            rules::mark_called(&mut CredisContract::new(storage.clone()), id, called_at).unwrap()
        );
        let lapsed = outbe_credis::settlement_deadline(&record(&storage, id)) + HOUR;
        advance_to(&storage, lapsed);
        finalize_through(&storage, lapsed);

        forfeit_through_abi(&storage, alice(), id, U256::ZERO).unwrap();
        assert_eq!(expire(&storage, lapsed), 0);
        assert_eq!(unallocated(&storage), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
    teardown();
}

#[derive(Debug, PartialEq, Eq)]
struct Footprint {
    gas: u64,
    reads: u64,
    writes: u64,
    logs: B256,
    state: B256,
}

/// Runs one step under production metering and digests its ordered logs and the storage it leaves.
fn measure<T>(
    provider: &mut HashMapStorageProvider,
    step: impl FnOnce(&StorageHandle<'_>) -> T,
) -> (T, Footprint) {
    let logged = provider.get_ordered_events().len();
    provider.set_gas_limit(1_000_000_000_000);
    provider.enable_production_storage_gas_metering();
    let (out, gas) = StorageHandle::enter(provider, |storage| {
        let out = step(&storage);
        (out, storage.gas_used().unwrap())
    });
    let (reads, writes) = provider.metered_storage_operations();
    let mut digest = Vec::new();
    for log in &provider.get_ordered_events()[logged..] {
        digest.extend_from_slice(log.address.as_slice());
        for topic in log.data.topics() {
            digest.extend_from_slice(topic.as_slice());
        }
        digest.extend_from_slice(&log.data.data);
    }
    let mut slots: Vec<_> = provider.storage.iter().collect();
    slots.sort();
    let mut state = Vec::new();
    for ((address, slot), value) in slots {
        state.extend_from_slice(address.as_slice());
        state.extend_from_slice(&slot.to_be_bytes::<32>());
        state.extend_from_slice(&value.to_be_bytes::<32>());
    }
    let footprint = Footprint {
        gas,
        reads,
        writes,
        logs: keccak256(digest),
        state: keccak256(state),
    };
    (out, footprint)
}

fn credis_views(storage: &StorageHandle<'_>, id: U256) -> B256 {
    let calls = [
        ICredis::tokenURICall { credisId: id }.abi_encode(),
        ICredis::getCredisCall { credisId: id }.abi_encode(),
        ICredis::credisExistsCall { credisId: id }.abi_encode(),
        ICredis::tokenByIndexCall { index: U256::ZERO }.abi_encode(),
        ICredis::balanceOfCall { owner: alice() }.abi_encode(),
        ICredis::tokenOfOwnerByIndexCall {
            owner: alice(),
            index: U256::ZERO,
        }
        .abi_encode(),
        ICredis::interestAccruedMinorCall { credisId: id }.abi_encode(),
        ICredis::interestPaidMinorCall { credisId: id }.abi_encode(),
    ];
    let mut out = Vec::new();
    for call in calls {
        let ret = outbe_credis::precompile::dispatch(storage.clone(), &call, alice(), U256::ZERO)
            .unwrap();
        out.extend_from_slice(&ret);
    }
    keccak256(out)
}

#[test]
fn lifecycle_steps_keep_their_gas_storage_and_log_footprint() {
    let mut provider = env();
    let reservation = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        fund_stake(&storage, pledge_stake());
        id
    });
    let (id, issue) = measure(&mut provider, |storage| {
        runtime::issue_credis(storage.clone(), cca(), reservation, pledge_stake())
            .unwrap()
            .0
    });
    let half = pledge_stables() / U256::from(2u64);
    let amount = StorageHandle::enter(&mut provider, |storage| {
        advance_to(&storage, CREATED_AT + 30 * DAY);
        interest_plus(&storage, id, half)
    });
    stub_payment(&mut provider, id, amount);
    let (_, partial) = measure(&mut provider, |storage| {
        let amount = interest_plus(storage, id, half);
        runtime::settle_credis(storage.clone(), alice(), id, amount).unwrap()
    });
    let (views, read) = measure(&mut provider, |storage| credis_views(storage, id));
    let amount = StorageHandle::enter(&mut provider, |storage| {
        advance_to(&storage, CREATED_AT + 60 * DAY);
        interest_plus(&storage, id, half)
    });
    stub_payment(&mut provider, id, amount);
    let (_, close) = measure(&mut provider, |storage| {
        let amount = interest_plus(storage, id, half);
        runtime::settle_credis(storage.clone(), alice(), id, amount).unwrap()
    });
    teardown();

    let mut provider = env();
    let (called_id, deadline) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        rules::mark_called(&mut CredisContract::new(storage.clone()), id, CREATED_AT).unwrap();
        let deadline = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, deadline);
        finalize_through(&storage, deadline);
        (id, deadline)
    });
    let (called_views, called_read) =
        measure(&mut provider, |storage| credis_views(storage, called_id));
    let (voided, void) = measure(&mut provider, |storage| expire(storage, deadline));
    teardown();

    assert_eq!(voided, 1);
    assert_eq!(
        [views, called_views],
        [
            b256!("0x7e6b406253d988cd27b5cbfb246b1bd7322f9f4911148d6dc6c7ba5fae5f4bc3"),
            b256!("0x6c25580c8311832a45e33c6d99fd5a9d5e36e5edda93ee221756a1f921ba8b13"),
        ]
    );
    assert_eq!(
        [issue, partial, read, close, called_read, void],
        [
            Footprint {
                gas: 214400,
                reads: 94,
                writes: 41,
                logs: b256!("0x59b3ca3f3148e9e0003f491a3ca360375164f443e75533f2e54a2c3aded618c2"),
                state: b256!("0xe5105721424247d0820f2440eab319e643783b59312dcc5df1db053c4b3f0254"),
            },
            Footprint {
                gas: 183700,
                reads: 137,
                writes: 34,
                logs: b256!("0xfa9255ee438839b0520014084f89e0ce1198398f2c6d5ac90a2deaef5d6e3cfe"),
                state: b256!("0x1f3c262a9a38ecc3a66c5d4eaf2e0d4a67dea131a5ceceea5d720dcd1fff570d"),
            },
            Footprint {
                gas: 10200,
                reads: 102,
                writes: 0,
                logs: b256!("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
                state: b256!("0x1f3c262a9a38ecc3a66c5d4eaf2e0d4a67dea131a5ceceea5d720dcd1fff570d"),
            },
            Footprint {
                gas: 214200,
                reads: 142,
                writes: 40,
                logs: b256!("0xd29e96c90ec5d2d18b6ba358502c03c9857a2354e6c5c6dd748b1ef1746fd260"),
                state: b256!("0xaa4ae700d85559cbfbeca19253b4a531cb781643df0d27b9d7128a66026edd8c"),
            },
            Footprint {
                gas: 10200,
                reads: 102,
                writes: 0,
                logs: b256!("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
                state: b256!("0x34cc3ee0b3cb010a09159333196d88564c06ebc4187eb179d5c62aaaa9bde5f9"),
            },
            Footprint {
                gas: 228800,
                reads: 138,
                writes: 43,
                logs: b256!("0x81f01b28f8f9039cd7d463c5f70139bf2384e0e24c333878342348d88f5e8c7c"),
                state: b256!("0xba8332f7579746565583b6efe326ec04932490da3ab0d73b76ddf41132b5c8c1"),
            },
        ]
    );
}
