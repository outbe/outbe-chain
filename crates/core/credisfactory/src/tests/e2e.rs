//! Direct-pledge issuance plus atomic repayment/forfeiture tests.
use crate::{runtime, tests::*};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_credis::{CredisContract, CredisState};
use outbe_enclave::protocol::GratisOp;
use outbe_gratisfactory::runtime::{cancel_pledge, pledge_of};
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
    stake: U256,
    expected: outbe_primitives::error::PrecompileError,
) {
    let err = runtime::issue_credis(storage.clone(), caller, id, stake).unwrap_err();
    assert_eq!(err.to_string(), expected.to_string());
}

#[test]
fn issue_uses_the_reservation_pledge_once_and_rolls_back_failures() {
    let mut provider = env();
    let (id, reservation) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        fund_stake(&storage, pledge_stake());
        expect_issue_error(
            &storage,
            cca(),
            id,
            pledge_stake(),
            outbe_gratisfactory::errors::GratisFactoryError::PledgeNotFound.into(),
        );
        expect_issue_error(
            &storage,
            cca(),
            U256::from(999),
            pledge_stake(),
            crate::errors::CredisFactoryError::ReservationNotFound.into(),
        );

        pledge(&storage, alice(), id, 1);
        let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        expect_issue_error(
            &storage,
            cca(),
            id,
            U256::ZERO,
            crate::errors::CredisFactoryError::CcaStakeMismatch.into(),
        );
        expect_issue_error(
            &storage,
            bob(),
            id,
            pledge_stake(),
            outbe_ccaregistry::errors::CcaError::NotRegistered.into(),
        );
        storage
            .increase_balance(
                outbe_primitives::addresses::CCA_REGISTRY_ADDRESS,
                outbe_ccaregistry::constants::BOND_REQUIREMENT,
            )
            .unwrap();
        outbe_ccaregistry::runtime::bond(
            storage.clone(),
            bob(),
            outbe_ccaregistry::constants::BOND_REQUIREMENT,
            "Other CCA".into(),
        )
        .unwrap();
        expect_issue_error(
            &storage,
            bob(),
            id,
            pledge_stake(),
            crate::errors::CredisFactoryError::ReservationCcaMismatch.into(),
        );
        assert_eq!(pledge_of(&storage, id).unwrap().source, alice());

        let (credis_id, amount) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        assert_eq!(amount, reservation.amount_minor);
        assert!(pledge_of(&storage, id).unwrap().source.is_zero());
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!(record.source, alice());
        assert_eq!(record.reference_currency, REFERENCE_ISO);
        assert_eq!(record.policy_rate, policy_rate());
        assert_eq!(record.call_anchor_price_minor, oracle_rate());
        assert_eq!(
            record.call_price_minor,
            crate::rules::calc_call_price(oracle_rate(), outbe_credis::constants::CALL_RATE_PCT)
                .unwrap()
        );
        assert_eq!(
            record.call_notice_period_seconds,
            outbe_credis::constants::CALL_NOTICE_PERIOD
        );
        (id, reservation)
    });
    // A later block derives a fresh Credis id, so only the spent pledge can stop a replay.
    provider.set_block_number(BLOCK_NUMBER + 1);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            outbe_vaultrouter::api::reservation_of(&storage, id).unwrap(),
            reservation
        );
        fund_stake(&storage, pledge_stake());
        expect_issue_error(
            &storage,
            cca(),
            id,
            pledge_stake(),
            outbe_gratisfactory::errors::GratisFactoryError::PledgeNotFound.into(),
        );
        let err = cancel_pledge(storage.clone(), alice(), id).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratisfactory::errors::GratisFactoryError::PledgeNotFound
            )
            .to_string()
        );
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());
    });
    teardown();
}

#[test]
fn a_cancelled_pledge_cannot_back_an_issue() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        pledge(&storage, alice(), id, 1);
        cancel_pledge(storage.clone(), alice(), id).unwrap();
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        fund_stake(&storage, pledge_stake());
        let err = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratisfactory::errors::GratisFactoryError::PledgeNotFound
            )
            .to_string()
        );
    });
    teardown();
}

#[test]
fn a_source_backs_another_owner_and_repayments_return_to_the_source() {
    let mut provider = env();
    let credis_id = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = seed_reservation(&storage, bob(), alice(), pledge_stables());
        let err = outbe_gratisfactory::runtime::pledge_gratis(
            storage.clone(),
            bob(),
            id,
            auth(GratisOp::Pledge, bob(), pledge_cost(), 0),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratisfactory::errors::GratisFactoryError::NotReservationSource
            )
            .to_string()
        );
        pledge(&storage, alice(), id, 1);
        fund_stake(&storage, pledge_stake());
        let credis_id = runtime::issue_credis(storage.clone(), cca(), id, pledge_stake())
            .unwrap()
            .0;
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!((record.owner, record.source), (bob(), alice()));
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
        let credis_id = open(&storage, 1);
        let fidelity = fidelity_of(&storage, alice());
        advance_to(&storage, CREATED_AT + DAY);
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&record, CREATED_AT + DAY).unwrap();
        assert!(!interest.is_zero());
        (credis_id, fidelity, interest)
    });
    paying(&mut provider, credis_id, interest, |storage| {
        runtime::settle_credis(storage, bob(), credis_id, interest).unwrap()
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
        let settled = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        assert_eq!(
            outbe_credis::runtime::effective_state(&settled, now_of(&storage)).unwrap(),
            CredisState::Settled
        );
    });
    teardown();
}

#[test]
fn false_token_return_rolls_back_credis_and_pledged_collateral() {
    let mut provider = env();
    provider.stub_sub_call_at_selector(
        settlement_asset(),
        crate::sol_ext::IERC20::transferFromCall::SELECTOR,
        Bytes::from(vec![0; 32]),
    );
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let credis = CredisContract::new(storage.clone());
        let record = credis.get_credis(id).unwrap();
        let pledged = outbe_gratis::api::pledged_ct(storage.clone(), alice()).unwrap();
        let liquid = outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
        assert!(runtime::settle_credis(storage.clone(), bob(), id, pledge_stables()).is_err());
        assert_eq!(credis.get_credis(id).unwrap(), record);
        assert_eq!(
            outbe_gratis::api::pledged_ct(storage.clone(), alice()).unwrap(),
            pledged
        );
        assert_eq!(
            outbe_gratis::api::balance_ct(storage, alice()).unwrap(),
            liquid
        );
    });
    teardown();
}

#[test]
fn a_token_that_moves_less_than_owed_rolls_back_credis_and_pledged_collateral() {
    let mut provider = env();
    let (id, record, pledged, liquid) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
        let record = CredisContract::new(storage.clone()).get_credis(id).unwrap();
        let pledged = outbe_gratis::api::pledged_ct(storage.clone(), alice()).unwrap();
        let liquid = outbe_gratis::api::balance_ct(storage, alice()).unwrap();
        (id, record, pledged, liquid)
    });
    provider.stub_sub_call_sequence_at_selector(
        settlement_asset(),
        crate::sol_ext::IERC20::balanceOfCall::SELECTOR,
        [U256::ZERO, U256::ONE].map(|balance| Bytes::from(balance.to_be_bytes::<32>().to_vec())),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let err = runtime::settle_credis(storage.clone(), bob(), id, pledge_stables()).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_vaultrouter::settlement::SettlementError::AmountMismatch
            )
            .to_string()
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
    teardown();
}

#[test]
fn forfeit_burns_only_remaining_credis_backing_and_leaves_fidelity_untouched() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        bootstrap_for(&storage, bob(), pledge_cost());
        let id = open(&storage, 1);
        let other = open_for(&storage, bob(), 1);
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
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        let mut reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        reservation.call_anchor_price_minor = U256::from(2_500_000u64);
        VaultRouterContract::new(storage.clone())
            .reservations
            .update(&reservation)
            .unwrap();
        pledge(&storage, alice(), id, 1);
        advance_to(&storage, midnight + 300);
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .policy_rate
            .write(&ISSUANCE_ISO, U256::from(999_999))
            .unwrap();
        oracle.utc_day_vwap_last_finalized.write(0).unwrap();
        fund_stake(&storage, pledge_stake());
        let (credis_id, principal) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        let record = CredisContract::new(storage).get_credis(credis_id).unwrap();
        assert_eq!(principal, reservation.amount_minor);
        assert_eq!(record.gratis_minor, reservation.gratis_minor);
        assert_eq!(record.entry_price_minor, reservation.entry_price_minor);
        assert_eq!(record.reference_currency, reservation.reference_currency);
        assert_eq!(record.policy_rate, reservation.policy_rate);
        assert_eq!(
            record.call_anchor_price_minor,
            reservation.call_anchor_price_minor
        );
        assert_eq!(
            record.call_price_minor,
            crate::rules::calc_call_price(
                reservation.call_anchor_price_minor,
                outbe_credis::constants::CALL_RATE_PCT
            )
            .unwrap()
        );
        assert_eq!(record.issued_at, midnight + 300);
        assert_eq!(record.last_settled_at, record.issued_at);
    });
    teardown();
}

#[test]
fn credis_series_lifecycle_emits_open_issue_settle_call_close_and_forfeit_events() {
    use crate::precompile::ICredisFactory;
    use alloy_sol_types::SolEvent;
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    use outbe_credis::precompile::ICredis;
    use outbe_gratisfactory::precompile::IGratisFactory;
    use outbe_primitives::addresses::{
        CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS, GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS,
    };

    let open_in_series = |storage: &StorageHandle<'_>,
                          who: Address,
                          nonce: u64,
                          shared_series: Option<outbe_core_common::series_id::SeriesId>|
     -> (U256, outbe_core_common::series_id::SeriesId) {
        let reservation_id = seed_reservation(storage, who, who, pledge_stables());
        let mut reservation =
            outbe_vaultrouter::api::reservation_of(storage, reservation_id).unwrap();
        if let Some(series_id) = shared_series {
            reservation.series_id = series_id;
            reservation.call_anchor_price_minor = oracle_rate();
            VaultRouterContract::new(storage.clone())
                .reservations
                .update(&reservation)
                .unwrap();
        }
        let series_id = reservation.series_id;
        pledge(storage, who, reservation_id, nonce);
        fund_stake(storage, pledge_stake());
        let (id, _) =
            runtime::issue_credis(storage.clone(), cca(), reservation_id, pledge_stake()).unwrap();
        (id, series_id)
    };

    let mut provider = env();
    let (id_settled, id_swept, series_id) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        bootstrap_for(&storage, bob(), pledge_cost());
        let (id_settled, series_id) = open_in_series(&storage, alice(), 1, None);
        let (id_swept, _) = open_in_series(&storage, bob(), 1, Some(series_id));
        (id_settled, id_swept, series_id)
    });
    provider.set_block_number(BLOCK_NUMBER + 1);
    let id_owner_forfeited = StorageHandle::enter(&mut provider, |storage| {
        open_in_series(&storage, alice(), 2, Some(series_id)).0
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
    assert_eq!(series_opened[0].policyRate, policy_rate());

    let issued: Vec<_> = provider
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisIssued::decode_log_data(log).ok())
        .collect();
    assert_eq!(issued.len(), 3);
    assert!(issued
        .iter()
        .all(|ev| ev.seriesId == series_opened[0].seriesId
            && ev.issuanceCurrency == ISSUANCE_ISO
            && ev.referenceCurrency == REFERENCE_ISO));

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
        .get_events(CREDIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| ICredisFactory::CredisSettled::decode_log_data(log).ok())
        .collect();
    assert_eq!(settled.len(), 2);
    assert!(settled
        .iter()
        .all(|ev| ev.credisId == id_settled && ev.principalSettledMinor == half));

    let released: Vec<_> = provider
        .get_events(GRATIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGratisFactory::GratisPledgeReleased::decode_log_data(log).ok())
        .collect();
    assert_eq!(released.len(), 2);
    assert_eq!(
        released.iter().map(|ev| ev.gratisMinor).sum::<U256>(),
        pledge_cost()
    );

    let series_called: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisSeriesCalled::decode_log_data(log).ok())
        .collect();
    assert_eq!(series_called.len(), 1);
    assert_eq!(series_called[0].seriesId, series_opened[0].seriesId);
    assert_eq!(series_called[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(series_called[0].referenceCurrency, REFERENCE_ISO);

    let series_closed: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisSeriesClosed::decode_log_data(log).ok())
        .collect();
    assert_eq!(series_closed.len(), 1);
    assert_eq!(series_closed[0].seriesId, series_opened[0].seriesId);
    assert_eq!(series_closed[0].issuanceCurrency, ISSUANCE_ISO);
    assert_eq!(series_closed[0].referenceCurrency, REFERENCE_ISO);
    assert_eq!(series_closed[0].forfeitedCount, 1);
    assert_eq!(series_closed[0].forfeitedAmountMinor, pledge_cost());

    let pledge_burned: Vec<_> = provider
        .get_events(GRATIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGratisFactory::GratisPledgeBurned::decode_log_data(log).ok())
        .collect();
    assert_eq!(pledge_burned.len(), 2);
    let cleanup_burned: U256 = pledge_burned
        .iter()
        .filter(|ev| ev.credisId == id_swept)
        .map(|ev| ev.gratisMinor)
        .sum();
    assert_eq!(series_closed[0].forfeitedAmountMinor, cleanup_burned);

    let gratis_burned: Vec<_> = provider
        .get_events(GRATIS_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_gratis::precompile::IGratis::GratisBurnedFromPledge::decode_log_data(log).ok()
        })
        .collect();
    assert_eq!(gratis_burned.len(), 2);

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
    assert_eq!(
        pledge_burned
            .iter()
            .map(|ev| ev.credisId)
            .collect::<Vec<_>>(),
        vec![id_owner_forfeited, id_swept]
    );

    // R5c: after the series has 0 live members (all settled or forfeited), getCredis(id_settled)
    // still returns the settled record with preserved CallTerms.
    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage);
        assert_eq!(credis.series_member_count(series_id).unwrap(), 0);
        let settled_record = credis.get_credis(id_settled).unwrap();
        assert!(settled_record.is_settled());
        assert_eq!(settled_record.series_id, series_id);
        assert_eq!(settled_record.call_anchor_price_minor, oracle_rate());
        assert!(settled_record.call_price_minor > oracle_rate());
        assert!(settled_record.call_window_seconds > 0);
        assert!(settled_record.call_threshold_seconds > 0);
        assert!(settled_record.call_notice_period_seconds > 0);
    });

    teardown();
}

#[test]
fn reservation_snapshots_call_profile_terms_and_called_settled_record_retains_call_terms_after_series_close(
) {
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    let mut provider = env();
    let (id_called_settled, series_id, prod_terms) =
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let prod_terms = outbe_credis::config::CredisParams::PROD;
            let reservation_id = seed_reservation(&storage, alice(), alice(), pledge_stables());
            // Switch live config_profile to DEV after reservation is created; issue_credis must
            // copy the reservation's snapshotted PROD terms without re-reading config_profile.
            CredisContract::new(storage.clone())
                .config_profile
                .write(outbe_credis::config::PROFILE_DEV)
                .unwrap();
            pledge(&storage, alice(), reservation_id, 1);
            fund_stake(&storage, pledge_stake());
            let (id, _) =
                runtime::issue_credis(storage.clone(), cca(), reservation_id, pledge_stake())
                    .unwrap();
            let record = CredisContract::new(storage.clone()).get_credis(id).unwrap();
            assert_eq!(record.call_rate, prod_terms.call_rate);
            assert_eq!(record.call_window_seconds, prod_terms.call_window_seconds);
            assert_eq!(
                record.call_threshold_seconds,
                prod_terms.call_threshold_seconds
            );
            assert_eq!(
                record.call_notice_period_seconds,
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

    let payoff = StorageHandle::enter(&mut provider, |storage| {
        runtime::quote_settlement(&storage, id_called_settled).unwrap()
    });
    settle_principal(&mut provider, alice(), id_called_settled, payoff);

    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage);
        assert_eq!(credis.series_member_count(series_id).unwrap(), 0);
        let record = credis.get_credis(id_called_settled).unwrap();
        assert!(record.is_settled());
        assert!(record.called_at > 0);
        let deadline = outbe_credis::runtime::settlement_deadline(&record);
        assert_eq!(
            deadline,
            credis.series_settlement_deadline.read(&series_id).unwrap()
        );
        assert_eq!(record.call_window_seconds, prod_terms.call_window_seconds);
        assert_eq!(
            record.call_threshold_seconds,
            prod_terms.call_threshold_seconds
        );
        assert_eq!(
            record.call_notice_period_seconds,
            prod_terms.call_notice_period_seconds
        );
    });

    teardown();
}

#[test]
fn a_called_series_settled_before_its_deadline_closes_with_zero_forfeit() {
    use alloy_sol_types::SolEvent;
    use outbe_core_common::series_id::SeriesId;
    use outbe_credis::constants::CALL_LOOKBACK_DAYS;
    use outbe_credis::precompile::ICredis;
    use outbe_primitives::addresses::CREDIS_ADDRESS;

    let mut provider = env();
    let (id, series_id) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let id = open(&storage, 1);
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

    let payoff = StorageHandle::enter(&mut provider, |storage| {
        runtime::quote_settlement(&storage, id).unwrap()
    });
    settle_principal(&mut provider, alice(), id, payoff);

    StorageHandle::enter(&mut provider, |storage| {
        let credis = CredisContract::new(storage.clone());
        let record = credis.get_credis(id).unwrap();
        assert!(record.is_settled());
        assert_eq!(credis.series_member_count(series_id).unwrap(), 0);
        assert_ne!(
            credis.called_series_slot.read(&series_id).unwrap(),
            0,
            "a called series stays queued until its deadline"
        );
        let past = outbe_credis::settlement_deadline(&record) + CLOSED_DAY_DELAY;
        advance_to(&storage, past);
        finalize_through(&storage, past);
        daily_pass(&storage, past).unwrap();
        assert_eq!(credis.called_series_slot.read(&series_id).unwrap(), 0);
        assert_eq!(credis.cleanup_series_slot.read(&series_id).unwrap(), 0);
    });

    let closed: Vec<_> = provider
        .get_events(CREDIS_ADDRESS)
        .iter()
        .filter_map(|log| ICredis::CredisSeriesClosed::decode_log_data(log).ok())
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
