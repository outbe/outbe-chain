//! Repayment and forfeiture of Credis backed by the pledge of a main account.
use crate::{precompile::ICredisFactory, runtime, tests::*};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_credis::{CredisContract, CredisState};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;
use outbe_vaultrouter::VaultRouterContract;

fn unallocated(storage: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

fn record(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::CredisView {
    CredisContract::new(storage.clone()).get_credis(id).unwrap()
}

fn deleted(storage: &StorageHandle<'_>, id: U256) -> bool {
    !CredisContract::new(storage.clone())
        .credis_exists(id)
        .unwrap()
}

/// What a reader of `id` sees now.
fn shown(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Outcome {
    let now = storage.now_secs().unwrap();
    outbe_credis::outcome(&record(storage, id), now).unwrap()
}

/// The collateral left on `id`, checked against the Credis's own accounting.
fn collateral(storage: &StorageHandle<'_>, id: U256) -> U256 {
    shown(storage, id).outstanding_gratis_minor
}

#[test]
fn settle_runs_immediately_after_issuance() {
    let mut provider = env();
    let (id, sealed) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        (id, record(&storage, id))
    });
    settle_principal(&mut provider, alice(), id, U256::from(500_000u64));
    settle_principal(&mut provider, alice(), id, U256::from(500_000u64));
    StorageHandle::enter(&mut provider, |storage| {
        let p = record(&storage, id);
        assert_eq!(p.outstanding_amount_minor, U256::from(1_000_000u64));
        assert_eq!(
            outbe_credis::runtime::effective_state(&p, now_of(&storage)).unwrap(),
            CredisState::Issued
        );
        assert_eq!(p.entry_price_minor, sealed.entry_price_minor);
        assert_eq!(p.call_price_minor, sealed.call_price_minor);
        assert_eq!(p.issued_at, sealed.issued_at);
    });
    teardown();
}

#[test]
fn part_settlement_frees_its_share_in_the_main_account() {
    let mut provider = env();
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        id
    });
    let half = pledge_stables() / U256::from(2u64);
    let (amount_paid, fee_paid) = settle_principal(&mut provider, alice(), id, half);
    assert_eq!(amount_paid, half);
    assert!(!fee_paid.is_zero());
    StorageHandle::enter(&mut provider, |storage| {
        let p = record(&storage, id);
        assert_eq!(p.outstanding_amount_minor, half);
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
        assert!(deleted(&storage, id));
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
            outbe_vaultrouter::precompile::IVaultRouter::redeemCredisReservationCall::SELECTOR,
            Bytes::from(principal.to_be_bytes::<32>().to_vec()),
        );
        let id = StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, collateral);
            let reservation_id = seed_reservation(&storage, bob(), principal);
            let mut reservation =
                outbe_vaultrouter::api::credis_reservation_of(&storage, reservation_id).unwrap();
            reservation.gratis_minor = collateral;
            VaultRouterContract::new(storage.clone())
                .reservations
                .update(&reservation)
                .unwrap();
            let (id, disbursed) = issue(&storage, reservation_id, alice()).unwrap();
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
                    runtime::settle_credis(storage, bob(), id, Address::ZERO, payment).unwrap()
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
            assert_eq!(p.outstanding_amount_minor, U256::from(3u64));
            assert!(p.outstanding_gratis_minor.is_zero());
            assert_eq!(
                outbe_credis::runtime::effective_state(&p, now_of(&storage)).unwrap(),
                CredisState::Issued
            );
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
                assert!(outcome.outstanding_amount_minor.is_zero());
                assert_eq!(outcome.forfeited_amount_minor, U256::from(3u64));
                assert!(outcome.forfeited_gratis_minor.is_zero());
                assert_eq!(
                    outcome.unpledged_gratis_minor,
                    record(&storage, id).gratis_minor
                );
                assert_eq!(expire(&storage, expired_at), 1);
                assert_eq!(expire(&storage, expired_at + DAY), 0);
                assert!(!CredisContract::new(storage.clone())
                    .credis_exists(id)
                    .unwrap());
            });
        } else {
            let rest = U256::from(3u64);
            paying(&mut provider, id, rest, |storage| {
                runtime::settle_credis(storage, bob(), id, Address::ZERO, rest).unwrap()
            });
            StorageHandle::enter(&mut provider, |storage| assert!(deleted(&storage, id)));
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
        let id = open(&storage);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_fee(&record(&storage, id), now_of(&storage)).unwrap();
        assert!(!interest.is_zero());
        (id, interest)
    });
    let decoded = paying(&mut provider, id, interest + principal, |storage| {
        let data = ICredisFactory::settleCredisCall {
            credisId: id,
            asset: Address::ZERO,
            amountCapMinor: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage, &data, alice(), U256::ZERO).unwrap();
        ICredisFactory::settleCredisCall::abi_decode_returns(&out).unwrap()
    });
    assert_eq!(decoded.settledAmountMinor, principal);
    assert_eq!(decoded.settledFeeMinor, interest);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            record(&storage, id).outstanding_amount_minor,
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
        let id = open(&storage);
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
            CredisContract::accrued_fee(&record(&storage, id), now_of(&storage)).unwrap();
        let payoff = quote(&storage);
        assert_eq!(payoff, pledge_stables() + interest);
        payoff
    });

    paying(&mut provider, id, payoff, |storage| {
        runtime::settle_credis(storage, alice(), id, Address::ZERO, payoff).unwrap()
    });
    StorageHandle::enter(&mut provider, |storage| assert!(deleted(&storage, id)));
    teardown();
}

#[test]
fn a_called_credis_quotes_its_payoff_until_the_deadline_and_nothing_after() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
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
            CredisContract::accrued_fee(&record(&storage, id), now_of(&storage)).unwrap();
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
        let id = open(&storage);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_fee(&record(&storage, id), now_of(&storage)).unwrap();
        (id, interest)
    });
    let offered = pledge_stables() * U256::from(1_000u64);
    let (amount_paid, fee_paid) = paying(&mut provider, id, offered, |storage| {
        runtime::settle_credis(storage, alice(), id, Address::ZERO, offered).unwrap()
    });
    assert_eq!(amount_paid, pledge_stables());
    assert_eq!(fee_paid, interest);
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
        let first = open(&storage);
        assert!(call_credis(&storage, first, now_of(&storage)));
        first
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let second = open(&storage);
        assert_ne!(second, first);
        assert_eq!(
            outbe_credis::runtime::effective_state(&record(&storage, first), now_of(&storage))
                .unwrap(),
            CredisState::Called
        );
        assert_eq!(
            outbe_credis::runtime::effective_state(&record(&storage, second), now_of(&storage))
                .unwrap(),
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
fn one_main_account_backs_several_credis() {
    let mut provider = env();
    let first = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));
        open(&storage)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    let second = StorageHandle::enter(&mut provider, |storage| open(&storage));
    let backing = |storage: &StorageHandle<'_>| {
        shown(storage, first).outstanding_gratis_minor
            + shown(storage, second).outstanding_gratis_minor
    };
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_pledged(&storage, alice()), backing(&storage));
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
        assert_eq!(view_pledged(&storage, alice()), backing(&storage));
        call_credis(&storage, second, CREATED_AT);
        assert_eq!(lapse(&storage, second), 1);
        let backing = shown(&storage, first).outstanding_gratis_minor;
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
        open(&storage)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    let (bystander, called_at) = StorageHandle::enter(&mut provider, |storage| {
        let bystander = open(&storage);
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
        let bin = outbe_system_common::call_bins::price_to_bin(open.call_price_minor).unwrap();
        assert_eq!(outbe_system_common::call_bins::len(&bins, bin).unwrap(), 1);
        assert_eq!(
            outbe_system_common::call_bins::entry_at(&bins, bin, 0).unwrap(),
            credis.group_of(&open).unwrap()
        );
        assert_eq!(
            outbe_credis::runtime::effective_state(&record(&storage, bystander), now_of(&storage))
                .unwrap(),
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
        let id = open(&storage);
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
            fee_plus(&storage, id, principal)
        });
        let out = paying(&mut provider, id, amount, |storage| {
            let data = ICredisFactory::settleCredisCall {
                credisId: id,
                asset: Address::ZERO,
                amountCapMinor: amount,
            }
            .abi_encode();
            crate::precompile::dispatch(storage, &data, bob(), U256::ZERO).unwrap()
        });
        assert_eq!(
            ICredisFactory::settleCredisCall::abi_decode_returns(&out)
                .unwrap()
                .settledAmountMinor,
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
            asset: Address::ZERO,
            amountCapMinor: U256::MAX,
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
fn a_failed_issue_keeps_the_gratis_and_cca_weight_and_exit_freezes_new_credis() {
    let mut provider = env();
    let (credis_id, next) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let id = seed_reservation(&storage, alice(), pledge_stables());
        let day = outbe_system_common::time::timestamp_to_date_key(CREATED_AT);
        // A pledge sealed for another reservation fails before anything moves.
        let other = seed_reservation(&storage, alice(), pledge_stables());
        let wrong = sealed_pledge(&storage, alice(), other);
        assert!(runtime::issue_credis(storage.clone(), alice(), id, &wrong).is_err());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
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
        let (credis_id, _) = issue(&storage, id, alice()).unwrap();
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(&storage, cca(), day).unwrap(),
            pledge_cost()
        );
        outbe_ccaregistry::runtime::unbond(storage.clone(), cca()).unwrap();
        (credis_id, other)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let err = issue(&storage, next, alice()).unwrap_err();
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
            record(&storage, credis_id).outstanding_amount_minor,
            pledge_stables()
        );
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn a_half_paid_call_forfeits_only_the_unpaid_backing_of_another_accounts_main_account() {
    let mut provider = env();
    let credis_id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, bob(), pledge_stables());
        issue(&storage, id, alice()).unwrap().0
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
        assert_eq!(view_pledged(&storage, alice()), half);
        assert_eq!(collateral(&storage, credis_id), half);

        let called_at = now_of(&storage);
        assert!(call_credis(&storage, credis_id, called_at));
        let deadline = outbe_credis::settlement_deadline(&record(&storage, credis_id));
        for at in [deadline - 1, deadline] {
            advance_to(&storage, at);
            finalize_through(&storage, at);
            assert_eq!(expire(&storage, at), 0);
            assert_eq!(
                outbe_credis::runtime::effective_state(
                    &record(&storage, credis_id),
                    now_of(&storage)
                )
                .unwrap(),
                CredisState::Called
            );
        }
        let supply = view_balance(&storage, alice()) + view_pledged(&storage, alice());
        advance_to(&storage, deadline + CLOSED_DAY_DELAY);
        finalize_through(&storage, deadline + CLOSED_DAY_DELAY);
        assert_eq!(expire(&storage, deadline + CLOSED_DAY_DELAY), 1);
        assert_eq!(expire(&storage, deadline + CLOSED_DAY_DELAY + DAY), 0);
        assert!(!CredisContract::new(storage.clone())
            .credis_exists(credis_id)
            .unwrap());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(
            view_balance(&storage, alice()) + view_pledged(&storage, alice()),
            supply - half
        );
        assert_eq!(unallocated(&storage), half);
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
fn the_owner_forfeits_an_issued_credis_burning_its_main_accounts_pledge() {
    let mut provider = env();
    let (credis_id, day, liquid) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let id = seed_reservation(&storage, bob(), pledge_stables());
        let credis_id = issue(&storage, id, alice()).unwrap().0;
        let day = outbe_system_common::time::timestamp_to_date_key(CREATED_AT);
        let liquid = view_balance(&storage, alice());
        let same_block_err = forfeit_through_abi(&storage, bob(), credis_id, U256::ZERO)
            .err()
            .unwrap();
        assert_eq!(
            same_block_err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::CannotForfeitInIssueBlock
            )
            .to_string()
        );
        (credis_id, day, liquid)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
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
        assert_eq!(out.forfeitedGratisMinor, pledge_cost());
        assert_eq!(out.forfeitedAmountMinor, pledge_stables());
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
        assert!(runtime::settle_credis(
            storage.clone(),
            bob(),
            credis_id,
            Address::ZERO,
            U256::from(1u64)
        )
        .is_err());
        assert_eq!(unallocated(&storage), pledge_cost());
    });
    teardown();
}

/// Past the deadline the Credis is its group's to forfeit: the owner is refused and the
/// sweep credits it once.
#[test]
fn the_owner_cannot_forfeit_a_lapsed_call_that_the_sweep_forfeits() {
    let mut provider = env();
    let (id, lapsed) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        let called_at = now_of(&storage);
        assert!(call_credis(&storage, id, called_at));
        let lapsed = outbe_credis::settlement_deadline(&record(&storage, id)) + CLOSED_DAY_DELAY;
        (id, lapsed)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
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

#[test]
fn issue_pledges_from_the_free_balance_of_the_main_account() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(3u64));
        let id = seed_reservation(&storage, bob(), pledge_stables());
        issue(&storage, id, alice()).unwrap();
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() * U256::from(2u64)
        );
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, bob()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
    });
    teardown();
}

#[test]
fn no_read_or_event_links_a_main_account_to_a_credis() {
    use outbe_credis::precompile::ICredis;
    let mut provider = env();
    let alice_word = alloy_primitives::B256::left_padding_from(alice().as_slice());
    let holds_alice = |bytes: &[u8]| bytes.windows(20).any(|window| window == alice().as_slice());
    let issued_from = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        storage.block_number().unwrap()
    });
    let events_from = provider.get_ordered_events().len();
    let (calldata, credis_id) = StorageHandle::enter(&mut provider, |storage| {
        let id = seed_reservation(&storage, bob(), pledge_stables());
        let calldata = ICredisFactory::issueCredisCall {
            credisReservationId: id,
            pledge: sealed_pledge(&storage, alice(), id).into(),
        }
        .abi_encode();
        let out =
            crate::precompile::dispatch(storage.clone(), &calldata, bob(), U256::ZERO).unwrap();
        let issued = ICredisFactory::issueCredisCall::abi_decode_returns(&out).unwrap();
        (calldata, issued.credisId)
    });
    assert_eq!(issued_from, BLOCK_NUMBER);
    settle_principal(
        &mut provider,
        bob(),
        credis_id,
        pledge_stables() / U256::from(2u64),
    );
    assert!(!holds_alice(&calldata));
    for log in &provider.get_ordered_events()[events_from..] {
        assert!(!log.data.topics().contains(&alice_word), "{log:?}");
        assert!(!holds_alice(&log.data.data), "{log:?}");
    }
    StorageHandle::enter(&mut provider, |storage| {
        let read = outbe_credis::precompile::dispatch(
            storage.clone(),
            &ICredis::getCredisCall {
                credisId: credis_id,
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        assert!(!holds_alice(&read));
        let uri = credis_view(
            &storage,
            ICredis::tokenURICall {
                credisId: credis_id,
            },
        );
        assert!(!uri
            .to_lowercase()
            .contains(&alloy_primitives::hex::encode(alice().as_slice())));
    });
    teardown();
}

#[test]
fn full_settlement_deletes_the_credis() {
    use alloy_sol_types::SolEvent;
    use outbe_credis::precompile::ICredis;
    use outbe_system_common::addresses::CREDIS_ADDRESS;
    let mut provider = env();
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        open(&storage)
    });
    settle_principal(&mut provider, alice(), id, pledge_stables());
    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
        assert!(deleted(&storage, id));
        assert_eq!(credis.credis_count_of(alice()).unwrap(), 0);
        assert_eq!(
            credis.amount_and_outstanding_of(alice()).unwrap(),
            (U256::ZERO, U256::ZERO)
        );
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
    });
    let events = provider.get_events(CREDIS_ADDRESS);
    let burned: Vec<_> = events
        .iter()
        .filter_map(|log| ICredis::CredisBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(burned.len(), 1);
    assert_eq!((burned[0].credisId, burned[0].owner), (id, alice()));
    assert!(events
        .iter()
        .filter_map(|log| ICredis::Transfer::decode_log_data(log).ok())
        .any(|transfer| transfer.tokenId == id && transfer.to == Address::ZERO));
    teardown();
}

#[test]
fn full_settlement_or_owner_forfeit_in_the_issue_block_fails() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        let full = fee_plus(&storage, id, pledge_stables());
        let err =
            runtime::settle_credis(storage.clone(), alice(), id, Address::ZERO, full).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::CannotFullySettleInIssueBlock
            )
            .to_string()
        );
        let err = runtime::forfeit_by_owner(storage.clone(), alice(), id).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::CannotForfeitInIssueBlock
            )
            .to_string()
        );
        let part = fee_plus(&storage, id, pledge_stables() / U256::from(2u64));
        runtime::settle_credis(storage.clone(), alice(), id, Address::ZERO, part).unwrap();
        assert_eq!(
            record(&storage, id).outstanding_amount_minor,
            pledge_stables() / U256::from(2u64)
        );
    });
    teardown();
}

fn owner_forfeit_world() -> (
    outbe_primitives::storage::hashmap::HashMapStorageProvider,
    U256,
) {
    let mut provider = env();
    let id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        open(&storage)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    (provider, id)
}

#[test]
fn owner_forfeit_fault_at_each_write_then_retry() {
    let (mut clean, id) = owner_forfeit_world();
    clean.fail_after_mutation_at(usize::MAX);
    StorageHandle::enter(&mut clean, |storage| {
        runtime::forfeit_by_owner(storage.clone(), alice(), id).unwrap();
    });
    let mutations = clean.clear_mutation_failure();
    assert!(mutations > 0);
    for operation in 0..mutations {
        let (mut provider, id) = owner_forfeit_world();
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        provider.fail_after_mutation_at(operation);
        let failed = StorageHandle::enter(&mut provider, |storage| {
            runtime::forfeit_by_owner(storage.clone(), alice(), id)
        });
        assert!(
            failed.is_err(),
            "mutation {operation} unexpectedly succeeded"
        );
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        StorageHandle::enter(&mut provider, |storage| {
            runtime::forfeit_by_owner(storage.clone(), alice(), id).unwrap();
        });
        assert_eq!(
            provider.storage, clean.storage,
            "state after retry at {operation}"
        );
    }
    teardown();
}

#[test]
fn credis_settles_in_another_asset_when_its_vault_is_gone() {
    use alloy_sol_types::SolEvent;
    use outbe_credis::precompile::ICredis;
    use outbe_system_common::addresses::CREDIS_ADDRESS;
    let other = Address::repeat_byte(0x31);
    let foreign = Address::repeat_byte(0x32);
    let world = || {
        let mut provider = env();
        stub_asset(&mut provider, other, ISSUANCE_ISO);
        provider.stub_sub_call_at_selector(
            other,
            crate::sol_ext::IERC20::decimalsCall::SELECTOR,
            iso_word(18),
        );
        stub_asset(&mut provider, foreign, REFERENCE_ISO);
        let id = StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            outbe_vaultrouter::test_support::register_vault(
                &storage,
                other,
                Address::repeat_byte(0x41),
                ISSUANCE_ISO,
            )
            .unwrap();
            outbe_vaultrouter::test_support::register_vault(
                &storage,
                foreign,
                Address::repeat_byte(0x42),
                REFERENCE_ISO,
            )
            .unwrap();
            open(&storage)
        });
        provider.set_block_number(BLOCK_NUMBER + 1);
        (provider, id)
    };
    let not_allowed = || {
        outbe_primitives::error::PrecompileError::from(
            crate::errors::CredisFactoryError::SettlementAssetNotAllowed,
        )
        .to_string()
    };

    // While the Credis asset has a vault, another asset is refused.
    let (mut provider, id) = world();
    StorageHandle::enter(&mut provider, |storage| {
        outbe_vaultrouter::test_support::register_vault(
            &storage,
            settlement_asset(),
            Address::repeat_byte(0x43),
            ISSUANCE_ISO,
        )
        .unwrap();
        let err = runtime::settle_credis(storage.clone(), bob(), id, other, pledge_stables())
            .unwrap_err();
        assert_eq!(err.to_string(), not_allowed());
    });
    teardown();

    // With no vault for the Credis asset, an asset of another currency is still refused,
    // and an asset of the issuance currency pays at par in its own units.
    let (mut provider, id) = world();
    let owed = StorageHandle::enter(&mut provider, |storage| {
        let err = runtime::settle_credis(storage.clone(), bob(), id, foreign, pledge_stables())
            .unwrap_err();
        assert_eq!(err.to_string(), not_allowed());
        runtime::quote_settlement(&storage, id).unwrap()
    });
    StorageHandle::enter(&mut provider, |storage| {
        runtime::settle_credis(storage.clone(), bob(), id, other, owed).unwrap();
        assert!(deleted(&storage, id));
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
    });
    let pull = outbe_vaultrouter::test_support::last_deposit_from(&provider).unwrap();
    assert_eq!(pull.asset, other);
    assert_eq!(
        pull.assetsMinor,
        owed * U256::from(10u64).pow(U256::from(12u64))
    );
    let settled: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisSettled::decode_log_data(log).ok())
        .collect();
    assert_eq!(settled.len(), 1);
    assert_eq!((settled[0].asset, settled[0].amountMinor), (other, owed));
    teardown();
}

#[test]
fn issue_at_expires_at_passes_and_a_second_later_fails() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let late = seed_reservation(&storage, bob(), pledge_stables());
        let on_time = seed_reservation(&storage, alice(), pledge_stables());
        let expires_at = outbe_vaultrouter::api::credis_reservation_of(&storage, on_time)
            .unwrap()
            .expires_at;
        advance_to(&storage, expires_at);
        issue(&storage, on_time, alice()).unwrap();
        advance_to(&storage, expires_at + 1);
        let err = issue(&storage, late, alice()).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::CredisFactoryError::ReservationExpired
            )
            .to_string()
        );
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn issue_with_too_little_free_gratis_changes_nothing() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() - U256::ONE);
        let id = seed_reservation(&storage, bob(), pledge_stables());
        let before = outbe_vaultrouter::api::credis_reservation_of(&storage, id).unwrap();
        let err = issue(&storage, id, alice()).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratis::errors::GratisError::Rejected(
                    "insufficient balance or balance overflow".into()
                )
            )
            .to_string()
        );
        assert_eq!(
            outbe_vaultrouter::api::credis_reservation_of(&storage, id).unwrap(),
            before
        );
        assert_eq!(view_balance(&storage, alice()), pledge_cost() - U256::ONE);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            CredisContract::new(storage.clone())
                .credis_count_of(bob())
                .unwrap(),
            0
        );
    });
    teardown();
}

#[test]
fn one_main_account_backs_two_credis_and_each_frees_only_its_own_gratis() {
    let mut provider = env();
    let (first, second) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2u64));
        let first = issue(
            &storage,
            seed_reservation(&storage, bob(), pledge_stables()),
            alice(),
        )
        .unwrap()
        .0;
        let second = issue(
            &storage,
            seed_reservation(&storage, alice(), pledge_stables()),
            alice(),
        )
        .unwrap()
        .0;
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(
            view_pledged(&storage, alice()),
            pledge_cost() * U256::from(2u64)
        );
        (first, second)
    });
    settle_principal(&mut provider, bob(), first, pledge_stables());
    StorageHandle::enter(&mut provider, |storage| {
        assert!(deleted(&storage, first));
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            record(&storage, second).outstanding_gratis_minor,
            pledge_cost()
        );
    });
    settle_principal(&mut provider, alice(), second, pledge_stables());
    StorageHandle::enter(&mut provider, |storage| {
        assert!(deleted(&storage, second));
        assert_eq!(
            view_balance(&storage, alice()),
            pledge_cost() * U256::from(2u64)
        );
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
    teardown();
}
