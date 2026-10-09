//! Repayment, forfeiture and stake behaviour of positions backed by a direct pledge.
use crate::{precompile::ICredisFactory, runtime, tests::common::*};
use alloy_primitives::{b256, keccak256, Bytes, B256, U256};
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

fn position(storage: &StorageHandle<'_>, id: U256) -> outbe_credis::Position {
    CredisContract::new(storage.clone())
        .get_position(id)
        .unwrap()
}

/// The collateral left on `id`, checked against the position's own accounting.
fn collateral(storage: &StorageHandle<'_>, id: U256) -> U256 {
    let remaining = outbe_gratisfactory::api::collateral_of(storage, id)
        .unwrap()
        .remaining_minor;
    assert_eq!(remaining, position(storage, id).outstanding_gratis_minor);
    remaining
}

#[test]
fn settle_runs_immediately_after_opening() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let sealed = position(&storage, id);
        settle_principal(&storage, alice(), id, U256::from(500_000u64));
        settle_principal(&storage, alice(), id, U256::from(500_000u64));
        let p = position(&storage, id);
        assert_eq!(p.outstanding_principal_minor, U256::from(1_000_000u64));
        assert_eq!(p.lifecycle_state().unwrap(), CredisState::Open);
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
        let p = position(&storage, id);
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
        let p = position(&storage, id);
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
            let p = position(&storage, id);
            assert_eq!(p.outstanding_principal_minor, U256::from(3u64));
            assert!(p.outstanding_gratis_minor.is_zero());
            assert_eq!(p.lifecycle_state().unwrap(), CredisState::Open);

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
                CredisState::Void
            } else {
                runtime::settle(storage.clone(), bob(), id, U256::from(3u64)).unwrap();
                CredisState::Settled
            };
            let p = position(&storage, id);
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
            CredisContract::accrued_interest(&position(&storage, id), now_of(&storage)).unwrap();
        assert!(!interest.is_zero());
        let principal = pledge_stables() / U256::from(4u64);
        let data = ICredisFactory::settleCredisCall {
            positionId: id,
            amountMinor: interest + principal,
        }
        .abi_encode();
        let out = crate::precompile::dispatch(storage.clone(), &data, alice(), U256::ZERO).unwrap();
        let decoded = ICredisFactory::settleCredisCall::abi_decode_returns(&out).unwrap();
        assert_eq!(decoded.principalPaidMinor, principal);
        assert_eq!(decoded.interestMinor, interest);
        assert_eq!(
            position(&storage, id).outstanding_principal_minor,
            pledge_stables() - principal
        );
    });
    teardown();
}

#[test]
fn settle_takes_only_what_the_position_needs() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        advance_to(&storage, CREATED_AT + 30 * DAY);
        let interest =
            CredisContract::accrued_interest(&position(&storage, id), now_of(&storage)).unwrap();
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
            position(&storage, first).lifecycle_state().unwrap(),
            CredisState::Called
        );
        assert_eq!(
            position(&storage, second).lifecycle_state().unwrap(),
            CredisState::Open
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
            position(storage, first).outstanding_gratis_minor
                + position(storage, second).outstanding_gratis_minor
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
        runtime::void_position(storage.clone(), second).unwrap();
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
fn a_position_settled_at_the_deadline_is_never_voided() {
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
        let open = position(&storage, bystander);
        let bins = outbe_credis::CallBins(&credis, open.reference_currency);
        let bin = outbe_primitives::call_bins::price_to_bin(open.call_price_minor).unwrap();
        assert_eq!(outbe_primitives::call_bins::len(&bins, bin).unwrap(), 1);
        assert_eq!(
            outbe_primitives::call_bins::entry_at(&bins, bin, 0).unwrap(),
            bystander
        );
        assert_eq!(
            position(&storage, bystander).lifecycle_state().unwrap(),
            CredisState::Open
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
        let deadline = outbe_credis::settlement_deadline(&credis.get_position(id).unwrap());
        let principal = pledge_stables() / U256::from(4);
        for now in [deadline - 1, deadline] {
            advance_to(&storage, now);
            let interest =
                CredisContract::accrued_interest(&credis.get_position(id).unwrap(), now).unwrap();
            let data = ICredisFactory::settleCredisCall {
                positionId: id,
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
        let before = credis.get_position(id).unwrap();
        let balance = view_balance(&storage, alice());
        let pledged = view_pledged(&storage, alice());
        assert_eq!(pledged, pledge_cost() / U256::from(2));
        advance_to(&storage, deadline + HOUR);
        let data = ICredisFactory::settleCredisCall {
            positionId: id,
            amountMinor: U256::MAX,
        }
        .abi_encode();
        let err =
            crate::precompile::dispatch(storage.clone(), &data, bob(), U256::ZERO).unwrap_err();
        assert!(err.to_string().contains("call window has lapsed"), "{err}");
        assert_eq!(credis.get_position(id).unwrap(), before);
        assert_eq!(view_balance(&storage, alice()), balance);
        assert_eq!(view_pledged(&storage, alice()), pledged);

        runtime::void_position(storage.clone(), id).unwrap();
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
fn the_stake_stays_with_the_smart_account_through_settlement_and_void() {
    for void in [false, true] {
        let mut provider = env();
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let id = open(&storage, 1);
            assert_eq!(storage.balance(alice()).unwrap(), pledge_stake());
            assert_eq!(storage.balance(CREDIS_FACTORY_ADDRESS).unwrap(), U256::ZERO);
            if void {
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
fn failed_origination_keeps_the_pledge_and_cca_weight_and_exit_freezes_new_positions() {
    let mut provider = env();
    let (position_id, next) = StorageHandle::enter(&mut provider, |storage| {
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
        let (position_id, _) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        assert_eq!(
            outbe_ccaregistry::api::reward_weight(&storage, cca(), day).unwrap(),
            pledge_cost()
        );
        outbe_ccaregistry::runtime::unbond(storage.clone(), cca()).unwrap();
        let next = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), next, 2);
        fund_stake(&storage, pledge_stake());
        (position_id, next)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let err = runtime::issue_credis(storage.clone(), cca(), next, pledge_stake()).unwrap_err();
        assert!(err.to_string().contains("CCA is not active"), "{err}");
        assert_eq!(
            position(&storage, position_id).outstanding_principal_minor,
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
        let position_id = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake())
            .unwrap()
            .0;
        let half = pledge_cost() / U256::from(2u64);
        settle_principal(
            &storage,
            bob(),
            position_id,
            pledge_stables() / U256::from(2u64),
        );
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost() + half);
        assert_eq!(collateral(&storage, position_id), half);

        let called_at = now_of(&storage);
        assert!(CredisContract::new(storage.clone())
            .mark_called(position_id, called_at)
            .unwrap());
        let deadline = outbe_credis::settlement_deadline(&position(&storage, position_id));
        for at in [deadline - 1, deadline] {
            advance_to(&storage, at);
            finalize_through(&storage, at);
            assert_eq!(expire(&storage, at), 0);
            assert_eq!(
                position(&storage, position_id).lifecycle_state().unwrap(),
                CredisState::Called
            );
        }
        let supply = view_balance(&storage, alice()) + view_pledged(&storage, alice());
        advance_to(&storage, deadline + HOUR);
        finalize_through(&storage, deadline + HOUR);
        assert_eq!(expire(&storage, deadline + HOUR), 1);
        assert_eq!(expire(&storage, deadline + HOUR), 0);
        assert_eq!(
            position(&storage, position_id).lifecycle_state().unwrap(),
            CredisState::Void
        );
        assert_eq!(collateral(&storage, position_id), U256::ZERO);
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

#[derive(Debug, PartialEq, Eq)]
struct Footprint {
    gas: u64,
    reads: u64,
    writes: u64,
    logs: B256,
}

/// Runs one step under production storage metering and digests the logs it emitted, in order.
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
    let footprint = Footprint {
        gas,
        reads,
        writes,
        logs: keccak256(digest),
    };
    (out, footprint)
}

fn credis_views(storage: &StorageHandle<'_>, id: U256) -> B256 {
    let calls = [
        ICredis::tokenURICall { positionId: id }.abi_encode(),
        ICredis::getPositionCall { positionId: id }.abi_encode(),
        ICredis::positionByIndexCall { index: U256::ZERO }.abi_encode(),
        ICredis::positionOfAddressByIndexCall {
            smartAccount: alice(),
            index: U256::ZERO,
        }
        .abi_encode(),
        ICredis::interestAccruedMinorCall { positionId: id }.abi_encode(),
        ICredis::credisPrincipalAndOutstandingOfCall {
            smartAccount: alice(),
        }
        .abi_encode(),
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
    StorageHandle::enter(&mut provider, |storage| {
        advance_to(&storage, CREATED_AT + 30 * DAY)
    });
    let (_, partial) = measure(&mut provider, |storage| {
        settle_principal(storage, alice(), id, half)
    });
    let (views, read) = measure(&mut provider, |storage| credis_views(storage, id));
    StorageHandle::enter(&mut provider, |storage| {
        advance_to(&storage, CREATED_AT + 60 * DAY)
    });
    let (_, close) = measure(&mut provider, |storage| {
        settle_principal(storage, alice(), id, half)
    });
    teardown();

    let mut provider = env();
    let deadline = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        CredisContract::new(storage.clone())
            .mark_called(id, CREATED_AT)
            .unwrap();
        let deadline = CREATED_AT + NOTICE + HOUR;
        advance_to(&storage, deadline);
        finalize_through(&storage, deadline);
        deadline
    });
    let (voided, void) = measure(&mut provider, |storage| expire(storage, deadline));
    teardown();

    assert_eq!(voided, 1);
    assert_eq!(
        views,
        b256!("0x6760edfdbc12ac89d83134f3e785d1a70d50c3e9579e51e842ac098f99e22e7d")
    );
    assert_eq!(
        [issue, partial, read, close, void],
        [
            Footprint {
                gas: 214300,
                reads: 93,
                writes: 41,
                logs: b256!("0xb4862d8cb352d98f119ceea44c3db1a06daadcb7151bfc52a7d5fa06eb041bca"),
            },
            Footprint {
                gas: 183700,
                reads: 137,
                writes: 34,
                logs: b256!("0xabf2b6c4044763cab91e5cbc28c63bdf510963c3b85ea94150562846159fe368"),
            },
            Footprint {
                gas: 15000,
                reads: 150,
                writes: 0,
                logs: b256!("0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"),
            },
            Footprint {
                gas: 214200,
                reads: 142,
                writes: 40,
                logs: b256!("0x4a9b0f0b14a9ee5d560ddb4d4d338a43f8b75326e3baf44001665b759c09523b"),
            },
            Footprint {
                gas: 231500,
                reads: 115,
                writes: 44,
                logs: b256!("0xf825cab70bcb9d919b04a03ead21d67a05ed196b2276ef0dbe224f7aa6c678b0"),
            },
        ]
    );
}
