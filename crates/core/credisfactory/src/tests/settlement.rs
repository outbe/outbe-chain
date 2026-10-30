//! Repayment, forfeiture and stake behaviour of Credis backed by a direct pledge.
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
        .promis_limit()
        .unwrap()
}

fn record(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Credis {
    CredisContract::new(storage.clone()).get_credis(id).unwrap()
}

/// What a reader of `id` sees now.
fn shown(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Outcome {
    let now = storage.now_secs().unwrap();
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

        if forfeit {
            StorageHandle::enter(&mut provider, |storage| {
                let called_at = now_of(&storage);
                call_credis(&storage, id, called_at);
                let expired_at = called_at + NOTICE + CLOSED_DAY_DELAY;
                advance_to(&storage, expired_at);
                finalize_through(&storage, expired_at);
                // The rounding already returned every Gratis: nothing is burned.
                let outcome = shown(&storage, id);
                assert!(outcome.outstanding_principal_minor.is_zero());
                assert_eq!(outcome.principal_written_off_minor, U256::from(3u64));
                assert!(outcome.gratis_burned_minor.is_zero());
                assert_eq!(
                    outcome.gratis_returned_minor,
                    record(&storage, id).gratis_minor
                );
                assert_eq!(expire(&storage, expired_at), 1);
                assert_eq!(expire(&storage, expired_at), 0);
                assert!(!CredisContract::new(storage.clone())
                    .credis_exists(id)
                    .unwrap());
            });
        } else {
            let rest = U256::from(3u64);
            paying(&mut provider, id, rest, |storage| {
                runtime::settle_credis(storage, bob(), id, rest).unwrap()
            });
            StorageHandle::enter(&mut provider, |storage| {
                let p = record(&storage, id);
                assert_eq!(p.lifecycle_state().unwrap(), CredisState::Settled);
                assert!(shown(&storage, id).outstanding_principal_minor.is_zero());
            });
        }
        StorageHandle::enter(&mut provider, |storage| {
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
        assert!(call_credis(&storage, id, CREATED_AT));
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
        assert!(call_credis(&storage, first, now_of(&storage)));
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
        call_credis(&storage, second, CREATED_AT);
        assert_eq!(lapse(&storage, second), 1);
        let backing = shown(&storage, first).outstanding_gratis_minor;
        assert_eq!(view_pledged(&storage, alice()), backing + pledge_cost());
        cancel_pledge(storage.clone(), alice(), unused).unwrap();
        assert_eq!(view_pledged(&storage, alice()), backing);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            backing
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
        assert!(call_credis(&storage, id, called_at));
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
            open.group_key().expect("date step")
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
        let credis = CredisContract::new(storage.clone());
        call_credis(&storage, id, CREATED_AT);
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
        advance_to(&storage, deadline + CLOSED_DAY_DELAY);
        let data = ICredisFactory::settleCredisCall {
            credisId: id,
            amountMinor: U256::MAX,
        }
        .abi_encode();
        let err =
            crate::precompile::dispatch(storage.clone(), &data, bob(), U256::ZERO).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::SettlementDeadlinePassed
            )
            .to_string()
        );
        assert_eq!(credis.get_credis(id).unwrap(), before);
        assert_eq!(view_balance(&storage, alice()), balance);
        assert_eq!(view_pledged(&storage, alice()), pledged);

        assert_eq!(expire(&storage, deadline + CLOSED_DAY_DELAY), 1);
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
            assert_eq!(
                err.to_string(),
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::CredisFactoryError::CcaStakeMismatch
                )
                .to_string()
            );
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
                call_credis(&storage, id, called_at);
                let deadline = called_at + NOTICE + CLOSED_DAY_DELAY;
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
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_ccaregistry::errors::CcaError::CcaNotActive {
                    cca: cca(),
                    state: outbe_ccaregistry::precompile::ICcaRegistry::State::Deregistering,
                }
            )
            .to_string()
        );
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
        assert!(call_credis(&storage, credis_id, called_at));
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
        advance_to(&storage, deadline + CLOSED_DAY_DELAY);
        finalize_through(&storage, deadline + CLOSED_DAY_DELAY);
        assert_eq!(expire(&storage, deadline + CLOSED_DAY_DELAY), 1);
        assert_eq!(expire(&storage, deadline + CLOSED_DAY_DELAY), 0);
        assert!(!CredisContract::new(storage.clone())
            .credis_exists(credis_id)
            .unwrap());
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
            assert_eq!(
                err.to_string(),
                outbe_primitives::error::PrecompileError::from(
                    crate::errors::CredisFactoryError::NotCredisOwner
                )
                .to_string()
            );
        }
        assert!(forfeit_through_abi(&storage, bob(), credis_id, U256::from(1u64)).is_err());

        let out = forfeit_through_abi(&storage, bob(), credis_id, U256::ZERO).unwrap();
        assert_eq!(out.gratisBurnedMinor, pledge_cost());
        assert_eq!(out.principalWrittenOffMinor, pledge_stables());
        assert!(!CredisContract::new(storage.clone())
            .credis_exists(credis_id)
            .unwrap());
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

/// Past the deadline the Credis is its group's to forfeit: the owner is refused and the
/// sweep credits it once.
#[test]
fn the_owner_cannot_forfeit_a_lapsed_call_that_the_sweep_forfeits() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let called_at = now_of(&storage);
        assert!(call_credis(&storage, id, called_at));
        let lapsed = outbe_credis::settlement_deadline(&record(&storage, id)) + CLOSED_DAY_DELAY;
        advance_to(&storage, lapsed);
        finalize_through(&storage, lapsed);

        let err = forfeit_through_abi(&storage, alice(), id, U256::ZERO)
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::CredisClosed
            )
            .to_string()
        );
        assert_eq!(expire(&storage, lapsed), 1);
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
        ICredis::balanceOfCall { owner: alice() }.abi_encode(),
        ICredis::tokensByOwnerCall { owner: alice() }.abi_encode(),
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
        call_credis(&storage, id, CREATED_AT);
        let deadline = CREATED_AT + NOTICE + CLOSED_DAY_DELAY;
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
            b256!("0xca96785236a17305aaf93d64814dcb5971a0b697144df0928e38c080dd8b8e70"),
            b256!("0xe1ee272483f1a74fb4d9e8a7fcda9d556eab6d702ec3e076553cc343df1581b9"),
        ]
    );
    assert_eq!(
        [issue, partial, read, close, called_read, void],
        [
            Footprint {
                gas: 213900,
                reads: 89,
                writes: 41,
                logs: b256!("0x59b3ca3f3148e9e0003f491a3ca360375164f443e75533f2e54a2c3aded618c2"),
                state: b256!("0x029a07fcf8f95755682002a1eb4e4fcbd0dc0d4e5e62a72abfe607c7c64b1183"),
            },
            Footprint {
                gas: 135800,
                reads: 108,
                writes: 25,
                logs: b256!("0xfa9255ee438839b0520014084f89e0ce1198398f2c6d5ac90a2deaef5d6e3cfe"),
                state: b256!("0xc060109d93aa93ec5b2924c3d5d30be00c6210ed9d4581bdb99d90b0837aa0b7"),
            },
            Footprint {
                gas: 7600,
                reads: 76,
                writes: 0,
                logs: b256!("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
                state: b256!("0xc060109d93aa93ec5b2924c3d5d30be00c6210ed9d4581bdb99d90b0837aa0b7"),
            },
            Footprint {
                gas: 236800,
                reads: 118,
                writes: 45,
                logs: b256!("0xd29e96c90ec5d2d18b6ba358502c03c9857a2354e6c5c6dd748b1ef1746fd260"),
                state: b256!("0x94a8cb3e82ed9b0421e340ec4c0002c599bc4a2654b776a46922a45bdebd45f2"),
            },
            Footprint {
                gas: 7600,
                reads: 76,
                writes: 0,
                logs: b256!("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
                // Daily scheduling keys and nonzero queue locations change stored state.
                state: b256!("0xd27d66fab41ea7f353d16469f9a481a8d210923a7c7180a1ce57129e57e7306a"),
            },
            Footprint {
                gas: 354200,
                reads: 142,
                writes: 68,
                logs: b256!("0x42350e0ceadf5f8160c7fd05f29d7c6fe163d7aa3f377817fb7a04adbf541b5c"),
                state: b256!("0xfe019f46baba0f01dc01cb0e9964e90ec16c04a284358bc7ec8db5be8e993bc8"),
            },
        ]
    );
}
