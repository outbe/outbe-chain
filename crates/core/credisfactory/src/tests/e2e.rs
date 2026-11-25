//! Issue by the cash account plus atomic repayment/forfeiture tests.
use crate::{errors::CredisFactoryError, runtime, tests::*};
use alloy_primitives::{Address, U256};
use outbe_credis::CredisContract;
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_vaultrouter::VaultRouterContract;

fn fidelity_of(storage: &StorageHandle<'_>, account: Address) -> Vec<u8> {
    outbe_fidelity::FidelityContract::new(storage.clone())
        .cohorts_ct_of(account)
        .unwrap()
}

fn expect_issue_error(
    storage: &StorageHandle<'_>,
    caller: Address,
    id: U256,
    pledge: &[u8],
    expected: outbe_primitives::error::PrecompileError,
) {
    let err = runtime::issue_credis(storage.clone(), caller, id, pledge).unwrap_err();
    assert_eq!(err.to_string(), expected.to_string());
}

#[test]
fn cash_account_issues_the_credis_with_one_command() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        let id = seed_reservation(&storage, alice(), pledge_stables());
        let pledge = sealed_pledge(&storage, alice(), id);
        expect_issue_error(
            &storage,
            alice(),
            U256::from(999),
            &pledge,
            CredisFactoryError::ReservationNotFound.into(),
        );
        expect_issue_error(
            &storage,
            cca(),
            id,
            &pledge,
            CredisFactoryError::ReservationOwnerMismatch.into(),
        );
        let other = seed_reservation(&storage, alice(), pledge_stables());
        expect_issue_error(
            &storage,
            alice(),
            other,
            &pledge,
            outbe_gratis::errors::GratisError::Rejected("invalid pledge authorization".into())
                .into(),
        );
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);

        let reservation = outbe_vaultrouter::api::credis_reservation_of(&storage, id).unwrap();
        let (credis_id, amount) =
            runtime::issue_credis(storage.clone(), alice(), id, &pledge).unwrap();
        assert_eq!(amount, reservation.amount_minor);
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!(record.pledge_account, alice());
        assert_eq!(record.cca, cca());
        assert_eq!(record.reference_currency, REFERENCE_ISO);
        assert_eq!(record.policy_rate, policy_rate());
        assert_eq!(record.entry_price_minor, oracle_rate());
        assert_eq!(
            record.call_price_minor,
            crate::rules::calc_call_price(oracle_rate(), outbe_credis::constants::CALL_RATE_PCT)
                .unwrap()
        );
        assert_eq!(
            record.terms.call_notice_period_seconds,
            outbe_credis::constants::CALL_NOTICE_PERIOD
        );
    });
    teardown();
}

#[test]
fn a_main_account_backs_another_owner_and_settlements_free_its_gratis() {
    let mut provider = env();
    let credis_id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, bob(), pledge_stables());
        let credis_id = issue(&storage, id, alice()).unwrap().0;
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!((record.owner, record.pledge_account), (bob(), alice()));
        credis_id
    });
    settle_principal(&mut provider, bob(), credis_id, pledge_stables());
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
    });
    teardown();
}

#[test]
fn repayments_return_collateral_to_the_source_and_interest_only_returns_nothing() {
    let mut provider = env();
    let (credis_id, fidelity, interest) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let credis_id = open(&storage);
        let fidelity = fidelity_of(&storage, alice());
        advance_to(&storage, CREATED_AT + DAY);
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        let interest = CredisContract::accrued_fee(&record, CREATED_AT + DAY).unwrap();
        assert!(!interest.is_zero());
        (credis_id, fidelity, interest)
    });
    paying(&mut provider, credis_id, interest, |storage| {
        runtime::settle_credis(storage, bob(), credis_id, Address::ZERO, interest).unwrap()
    });
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
    });

    let half = pledge_cost() / U256::from(2);
    settle_principal(
        &mut provider,
        bob(),
        credis_id,
        pledge_stables() / U256::from(2),
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost() - half);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost() - half
        );
    });
    settle_principal(
        &mut provider,
        bob(),
        credis_id,
        pledge_stables() / U256::from(2),
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            view_balance(&storage, alice()) + view_pledged(&storage, alice()),
            pledge_cost()
        );
        assert_eq!(fidelity_of(&storage, alice()), fidelity);
        assert!(!CredisContract::new(storage.clone())
            .credis_exists(credis_id)
            .unwrap());
    });
    teardown();
}

/// The router takes the repayment directly from the caller. A payment that the router
/// refuses rolls back the Credis and the pledged collateral.
#[test]
fn a_payment_the_router_refuses_rolls_back_credis_and_pledged_collateral() {
    let mut provider = env();
    outbe_vaultrouter::test_support::stub_unanswered_deposit_from(&mut provider);
    let (id, record, pledged, liquid) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        let record = CredisContract::new(storage.clone()).get_credis(id).unwrap();
        let pledged = outbe_gratis::api::pledged_ct(storage.clone(), alice()).unwrap();
        let liquid = outbe_gratis::api::balance_ct(storage, alice()).unwrap();
        (id, record, pledged, liquid)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        let err =
            runtime::settle_credis(storage.clone(), bob(), id, Address::ZERO, pledge_stables())
                .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_vaultrouter::test_support::unanswered_deposit_from().to_string()
        );
        assert_eq!(
            CredisContract::new(storage.clone()).get_credis(id).unwrap(),
            record
        );
        assert_eq!(
            outbe_gratis::api::pledged_ct(storage.clone(), alice()).unwrap(),
            pledged
        );
        assert_eq!(
            outbe_gratis::api::balance_ct(storage, alice()).unwrap(),
            liquid
        );
    });
    let pull = outbe_vaultrouter::test_support::last_deposit_from(&provider).unwrap();
    assert_eq!((pull.payer, pull.asset), (bob(), settlement_asset()));
    teardown();
}

#[test]
fn forfeit_burns_the_pledged_gratis_of_the_unsettled_part() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        bootstrap_for(&storage, bob(), pledge_cost());
        let id = open(&storage);
        let other = open_for(&storage, bob());
        let fidelity = fidelity_of(&storage, alice());
        let liquid = outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
        let reserve = PromisLimitContract::new(storage.clone())
            .promis_limit()
            .unwrap();
        call_credis(&storage, id, CREATED_AT);
        assert_eq!(lapse(&storage, id), 1);
        assert_eq!(expire(&storage, now_of(&storage)), 0);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, bob()), pledge_cost());
        assert_eq!(
            CredisContract::new(storage.clone())
                .get_credis(other)
                .unwrap()
                .outstanding_gratis_minor,
            pledge_cost()
        );
        assert_eq!(
            view_balance(&storage, alice())
                + view_pledged(&storage, alice())
                + view_balance(&storage, bob())
                + view_pledged(&storage, bob()),
            pledge_cost()
        );
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .promis_limit()
                .unwrap(),
            reserve + pledge_cost()
        );
        assert_eq!(fidelity_of(&storage, alice()), fidelity);
        assert_eq!(
            outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap(),
            liquid
        );
    });
    teardown();
}

/// The reservation fixes every Credis term: neither midnight, an unfinalized day that
/// just closed nor a changed policy rate moves them before issuance.
#[test]
fn issuance_uses_reserved_terms_across_midnight_and_oracle_changes() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let midnight = (CREATED_AT / DAY + 1) * DAY;
        advance_to(&storage, midnight - 300);
        let id = seed_reservation(&storage, alice(), pledge_stables());
        let mut reservation = outbe_vaultrouter::api::credis_reservation_of(&storage, id).unwrap();
        reservation.entry_price_minor = U256::from(2_500_000u64);
        VaultRouterContract::new(storage.clone())
            .reservations
            .update(&reservation)
            .unwrap();
        advance_to(&storage, midnight + 300);
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .policy_rate
            .write(&ISSUANCE_ISO, U256::from(999_999))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(0).unwrap();
        let (credis_id, principal) = issue(&storage, id, alice()).unwrap();
        let record = CredisContract::new(storage).get_credis(credis_id).unwrap();
        assert_eq!(principal, reservation.amount_minor);
        assert_eq!(record.gratis_minor, reservation.gratis_minor);
        assert_eq!(record.entry_price_minor, reservation.entry_price_minor);
        assert_eq!(record.reference_currency, reservation.reference_currency);
        assert_eq!(record.policy_rate, reservation.policy_rate);
        assert_eq!(
            record.call_price_minor,
            crate::rules::calc_call_price(
                reservation.entry_price_minor,
                outbe_credis::constants::CALL_RATE_PCT
            )
            .unwrap()
        );
        assert_eq!(record.issued_at, midnight + 300);
        assert_eq!(record.last_settled_at, record.issued_at);
        assert_eq!(record.series_id, reservation.series_id);
        assert_eq!(record.sub_series, reservation.sub_series);
        assert_eq!(record.sub_series, 23);
    });
    teardown();
}

#[test]
fn credis_series_lifecycle_emits_open_issue_settle_call_close_and_forfeit_events() {
    use crate::precompile::ICredisFactory;
    use alloy_sol_types::SolEvent;
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    use outbe_credis::precompile::ICredis;
    use outbe_system_common::addresses::{CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS};

    let open_in_series = |storage: &StorageHandle<'_>,
                          who: Address,
                          shared_series: Option<outbe_core_common::series_id::SeriesId>|
     -> (U256, outbe_core_common::series_id::SeriesId) {
        let reservation_id = seed_reservation(storage, who, pledge_stables());
        let mut reservation =
            outbe_vaultrouter::api::credis_reservation_of(storage, reservation_id).unwrap();
        if let Some(series_id) = shared_series {
            reservation.series_id = series_id;
            reservation.entry_price_minor = oracle_rate();
            VaultRouterContract::new(storage.clone())
                .reservations
                .update(&reservation)
                .unwrap();
        }
        let series_id = reservation.series_id;
        let (id, _) = issue(storage, reservation_id, who).unwrap();
        (id, series_id)
    };

    let mut provider = env();
    let (id_settled, id_swept, series_id) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        bootstrap_for(&storage, bob(), pledge_cost());
        let (id_settled, series_id) = open_in_series(&storage, alice(), None);
        let (id_swept, _) = open_in_series(&storage, bob(), Some(series_id));
        (id_settled, id_swept, series_id)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    let id_owner_forfeited = StorageHandle::enter(&mut provider, |storage| {
        open_in_series(&storage, alice(), Some(series_id)).0
    });

    // Only the first issue in the series emits CredisSeriesOpened; all three emit CredisIssued.
    let series_opened: Vec<_> = provider
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisSeriesOpened::decode_log_data(log).ok())
        .collect();
    assert_eq!(series_opened.len(), 1);
    assert_eq!(
        series_opened[0].seriesId,
        alloy_primitives::FixedBytes::from(series_id)
    );
    assert_eq!(series_opened[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(series_opened[0].referenceCurrency, REFERENCE_ISO);

    let issued: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisIssued::decode_log_data(log).ok())
        .collect();
    assert_eq!(issued.len(), 3);
    assert!(issued
        .iter()
        .all(|ev| ev.seriesId == series_opened[0].seriesId
            && ev.issuanceCurrency == ISSUANCE_ISO
            && ev.referenceCurrency == REFERENCE_ISO
            && ev.policyRate == policy_rate()));

    // Partial + full settlement of `id_settled`.
    let half = pledge_stables() / U256::from(2u64);
    settle_principal(&mut provider, alice(), id_settled, half);
    settle_principal(&mut provider, alice(), id_settled, half);

    // Owner forfeits `id_owner_forfeited` in a subsequent block before series call/close.
    provider.set_block_number(BLOCK_NUMBER + 2);
    StorageHandle::enter(&mut provider, |storage| {
        runtime::forfeit_by_owner(storage.clone(), alice(), id_owner_forfeited).unwrap();
        // Breach the call window via `scan`, then let `id_swept` lapse through close and cleanup.
        let at = CREATED_AT + (u64::from(CALL_LOOKBACK_DAYS) + 2) * DAY;
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, at), 1);
        assert_eq!(lapse(&storage, id_swept), 1);
    });

    let settled: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisSettled::decode_log_data(log).ok())
        .collect();
    assert_eq!(settled.len(), 2);
    assert!(settled
        .iter()
        .all(|ev| ev.credisId == id_settled && ev.amountMinor - ev.feeMinor == half));
    assert_eq!(
        settled
            .iter()
            .map(|ev| ev.unpledgedGratisMinor)
            .sum::<U256>(),
        pledge_cost()
    );

    let series_called: Vec<_> = provider
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisSeriesCalled::decode_log_data(log).ok())
        .collect();
    assert_eq!(series_called.len(), 1);
    assert_eq!(series_called[0].seriesId, series_opened[0].seriesId);
    assert_eq!(series_called[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(series_called[0].referenceCurrency, REFERENCE_ISO);

    let series_closed: Vec<_> = provider
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisSeriesClosed::decode_log_data(log).ok())
        .collect();
    assert_eq!(series_closed.len(), 1);
    assert_eq!(series_closed[0].seriesId, series_opened[0].seriesId);
    assert_eq!(series_closed[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(series_closed[0].referenceCurrency, REFERENCE_ISO);
    assert_eq!(series_closed[0].forfeitedCount, 1);
    assert_eq!(series_closed[0].forfeitedAmountMinor, pledge_stables());

    let credis_forfeited: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisForfeited::decode_log_data(log).ok())
        .collect();
    assert_eq!(credis_forfeited.len(), 2);
    assert_eq!(
        credis_forfeited
            .iter()
            .map(|ev| ev.credisId)
            .collect::<Vec<_>>(),
        vec![id_owner_forfeited, id_swept]
    );

    // The full settlement deleted the settled Credis and emitted its burn.
    StorageHandle::enter(&mut provider, |storage| {
        assert!(!CredisContract::new(storage)
            .credis_exists(id_settled)
            .unwrap());
    });
    let burned: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].credisId, id_settled);

    teardown();
}

#[test]
fn reservation_snapshots_call_profile_terms_and_a_called_full_settlement_deletes_the_credis() {
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    let mut provider = env();
    let (id_called_settled, series_id, prod_terms) =
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let prod_terms = outbe_credis::config::CredisParams::MAINNET;
            let reservation_id = seed_reservation(&storage, alice(), pledge_stables());
            // Switch live config_profile to DEV after reservation is created; issue_credis must
            // copy the reservation's snapshotted PROD terms without re-reading config_profile.
            CredisContract::new(storage.clone())
                .config_profile
                .write(outbe_credis::config::PROFILE_TESTNET)
                .unwrap();
            let (id, _) = issue(&storage, reservation_id, alice()).unwrap();
            let record = CredisContract::new(storage.clone()).get_credis(id).unwrap();
            assert_eq!(
                record.call_price_minor,
                crate::rules::calc_call_price(record.entry_price_minor, prod_terms.call_rate)
                    .unwrap()
            );
            assert_eq!(
                record.terms.call_window_seconds,
                prod_terms.call_window_seconds
            );
            assert_eq!(
                record.terms.call_threshold_seconds,
                prod_terms.call_threshold_seconds
            );
            assert_eq!(
                record.terms.call_notice_period_seconds,
                prod_terms.call_notice_period_seconds
            );

            // Call the series, then settle the last member so series_member_count reaches 0.
            let at = CREATED_AT + (u64::from(CALL_LOOKBACK_DAYS) + 2) * DAY;
            advance_to(&storage, at);
            fill_days(
                &storage,
                last_closed_day(at),
                CALL_LOOKBACK_DAYS,
                above_call(),
            );
            assert_eq!(scan(&storage, at), 1);
            (id, record.series_id, prod_terms)
        });

    let (payoff, group) = StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
        let record = credis.get_credis(id_called_settled).unwrap();
        assert_eq!(record.series_id, series_id);
        assert!(record.called_at > 0);
        assert_eq!(
            record.terms.call_window_seconds,
            prod_terms.call_window_seconds
        );
        (
            runtime::quote_settlement(&storage, id_called_settled).unwrap(),
            credis.group_of(&record).unwrap(),
        )
    });
    settle_principal(&mut provider, alice(), id_called_settled, payoff);

    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage);
        assert!(!credis.credis_exists(id_called_settled).unwrap());
        assert_eq!(credis.group_member_count(group).unwrap(), 0);
        assert_ne!(credis.group_settlement_deadline.read(&group).unwrap(), 0);
    });

    teardown();
}

#[test]
fn a_called_series_settled_before_its_deadline_closes_with_zero_forfeit() {
    use crate::precompile::ICredisFactory;
    use alloy_sol_types::SolEvent;
    use outbe_core_common::series_id::SeriesId;
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    use outbe_system_common::addresses::CREDIS_FACTORY_ADDRESS;

    let mut provider = env();
    let (id, series_id) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage);
        let series_id = CredisContract::new(storage.clone())
            .get_credis(id)
            .unwrap()
            .series_id;
        let at = CREATED_AT + (u64::from(CALL_LOOKBACK_DAYS) + 2) * DAY;
        advance_to(&storage, at);
        fill_days(
            &storage,
            last_closed_day(at),
            CALL_LOOKBACK_DAYS,
            above_call(),
        );
        assert_eq!(scan(&storage, at), 1);
        (id, series_id)
    });

    let (payoff, group, deadline) = StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
        let record = credis.get_credis(id).unwrap();
        (
            runtime::quote_settlement(&storage, id).unwrap(),
            credis.group_of(&record).unwrap(),
            outbe_credis::settlement_deadline(&record),
        )
    });
    settle_principal(&mut provider, alice(), id, payoff);

    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
        assert!(!credis.credis_exists(id).unwrap());
        assert_eq!(credis.group_member_count(group).unwrap(), 0);
        assert_ne!(
            credis.called_group_slot.read(&group).unwrap(),
            0,
            "a called series stays queued until its deadline"
        );
        let past = deadline + CLOSED_DAY_DELAY;
        advance_to(&storage, past);
        finalize_through(&storage, past);
        daily_pass(&storage, past).unwrap();
        assert_eq!(credis.called_group_slot.read(&group).unwrap(), 0);
        assert_eq!(credis.cleanup_group_slot.read(&group).unwrap(), 0);
    });

    let closed: Vec<_> = provider
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisSeriesClosed::decode_log_data(log).ok())
        .collect();
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].seriesId,
        alloy_primitives::FixedBytes::from(series_id)
    );
    assert_eq!(closed[0].forfeitedCount, 0);
    assert_eq!(closed[0].forfeitedAmountMinor, U256::ZERO);
    assert_eq!(closed[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(closed[0].referenceCurrency, REFERENCE_ISO);
    assert_eq!(
        SeriesId::for_pair(
            series_id.day(),
            closed[0].issuanceCurrency,
            closed[0].referenceCurrency
        )
        .unwrap(),
        series_id
    );

    teardown();
}

#[test]
fn credis_series_opened_once_per_sub_series() {
    use crate::precompile::ICredisFactory::CredisSeriesOpened;
    use alloy_sol_types::SolEvent;
    use outbe_credis::precompile::ICredis::CredisIssued;

    let other_entry = U256::from(2_100_000u64);
    let mut provider = env();
    let series = StorageHandle::enter(&mut provider, |storage| {
        for owner in [alice(), bob(), cca()] {
            bootstrap_for(&storage, owner, pledge_cost());
        }
        let hour = Placement::hour(&storage, 4, oracle_rate());
        let first = open_at(&storage, alice(), hour);
        let second = open_at(&storage, bob(), hour);
        let credis = CredisContract::new(storage.clone());
        assert_eq!(
            credis.get_credis(first).unwrap().sub_series,
            credis.get_credis(second).unwrap().sub_series
        );
        credis.get_credis(first).unwrap().series_id
    });

    let opened = factory_events::<CredisSeriesOpened>(&provider);
    assert_eq!(opened.len(), 1);
    assert_eq!(
        opened[0].seriesId,
        alloy_primitives::FixedBytes::from(series)
    );
    assert_eq!(opened[0].subSeries, 4);
    assert_eq!(opened[0].entryPriceMinor, oracle_rate());
    assert_eq!(
        opened[0].callPriceMinor,
        crate::rules::calc_call_price(oracle_rate(), outbe_credis::constants::CALL_RATE_PCT)
            .unwrap()
    );

    StorageHandle::enter(&mut provider, |storage| {
        open_at(&storage, cca(), Placement::hour(&storage, 7, other_entry));
    });

    let opened = factory_events::<CredisSeriesOpened>(&provider);
    assert_eq!(opened.len(), 2);
    assert_eq!(
        opened[1].seriesId,
        alloy_primitives::FixedBytes::from(series)
    );
    assert_eq!(opened[1].subSeries, 7);
    assert_eq!(opened[1].entryPriceMinor, other_entry);
    let issued = provider
        .get_events(outbe_system_common::addresses::CREDIS_ADDRESS)
        .iter()
        .filter(|log| CredisIssued::decode_log_data(log).is_ok())
        .count();
    assert_eq!(issued, 3);
    teardown();
}
