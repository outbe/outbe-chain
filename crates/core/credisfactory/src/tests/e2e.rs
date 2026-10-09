//! Direct-pledge issuance plus atomic repayment/forfeiture tests.
use crate::{runtime, tests::common::*};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_credis::{CredisContract, CredisState};
use outbe_gratisfactory::runtime::{cancel_pledge, pledge_of};
use outbe_primitives::storage::StorageHandle;
use outbe_promislimit::PromisLimitContract;
use outbe_tee::protocol::GratisOp;
use outbe_vaultrouter::VaultRouterContract;

fn fidelity_of(storage: &StorageHandle<'_>, account: Address) -> Vec<u8> {
    outbe_fidelity::FidelityContract::new(storage.clone())
        .cohorts_ct_of(account)
        .unwrap()
}

fn call_and_lapse(storage: &StorageHandle<'_>, id: U256) {
    let credis = CredisContract::new(storage.clone());
    let mut p = credis.get_credis(id).unwrap();
    p.state = CredisState::Called as u8;
    p.called_at = CREATED_AT;
    credis.records.update(&p).unwrap();
    advance_to(storage, CREATED_AT + NOTICE + 1);
}

fn expect_issue_error(
    storage: &StorageHandle<'_>,
    caller: Address,
    id: U256,
    stake: U256,
    text: &str,
) {
    let err = runtime::issue_credis(storage.clone(), caller, id, stake).unwrap_err();
    assert!(
        err.to_string().contains(text),
        "expected {text:?}, got {err}"
    );
}

#[test]
fn issue_uses_the_reservation_pledge_once_and_rolls_back_failures() {
    let mut provider = env();
    let (id, reservation) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        fund_stake(&storage, pledge_stake());
        expect_issue_error(&storage, cca(), id, pledge_stake(), "pledge not found");
        expect_issue_error(
            &storage,
            cca(),
            U256::from(999),
            pledge_stake(),
            "reservation not found",
        );

        pledge(&storage, alice(), id, 1);
        let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        expect_issue_error(&storage, cca(), id, U256::ZERO, "attached COEN");
        expect_issue_error(&storage, bob(), id, pledge_stake(), "CCA is not active");
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
            "reservation cca mismatch",
        );
        let mut undeployed = reservation.clone();
        undeployed.smart_account = bob();
        VaultRouterContract::new(storage.clone())
            .reservations
            .update(&undeployed)
            .unwrap();
        expect_issue_error(&storage, cca(), id, pledge_stake(), "not deployed");
        VaultRouterContract::new(storage.clone())
            .reservations
            .update(&reservation)
            .unwrap();
        assert_eq!(pledge_of(&storage, id).unwrap().source, alice());

        let (credis_id, amount) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        assert_eq!(amount, reservation.amount);
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
        assert_eq!(record.policy_rate, reservation.policy_rate);
        assert_eq!(
            record.call_price_minor,
            outbe_credis::calc_call_price(reservation.call_anchor_price_minor).unwrap()
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
        expect_issue_error(&storage, cca(), id, pledge_stake(), "pledge not found");
        let err = cancel_pledge(storage.clone(), alice(), id).unwrap_err();
        assert!(err.to_string().contains("pledge not found"), "{err}");
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
        assert!(err.to_string().contains("pledge not found"), "{err}");
    });
    teardown();
}

#[test]
fn a_source_backs_another_smart_account_and_repayments_return_to_the_source() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        deploy_smart_account(&storage, bob());
        let id = seed_reservation(&storage, bob(), alice(), pledge_stables());
        let err = outbe_gratisfactory::runtime::pledge_gratis(
            storage.clone(),
            bob(),
            id,
            auth(GratisOp::Pledge, bob(), pledge_cost(), 0),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("not the reservation source"),
            "{err}"
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
        settle_principal(&storage, bob(), credis_id, pledge_stables());
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
    });
    teardown();
}

#[test]
fn repayments_return_collateral_to_the_source_and_interest_only_returns_nothing() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let credis_id = open(&storage, 1);
        let fidelity = fidelity_of(&storage, alice());
        advance_to(&storage, CREATED_AT + DAY);
        let record = CredisContract::new(storage.clone())
            .get_credis(credis_id)
            .unwrap();
        let interest = CredisContract::accrued_interest(&record, CREATED_AT + DAY).unwrap();
        assert!(!interest.is_zero());
        runtime::settle(storage.clone(), bob(), credis_id, interest).unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost());

        let half = pledge_cost() / U256::from(2);
        settle_principal(&storage, bob(), credis_id, pledge_stables() / U256::from(2));
        assert_eq!(view_balance(&storage, alice()), half);
        assert_eq!(view_pledged(&storage, alice()), pledge_cost() - half);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost() - half
        );
        settle_principal(&storage, bob(), credis_id, pledge_stables() / U256::from(2));
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
        assert_eq!(
            CredisContract::new(storage)
                .get_credis(credis_id)
                .unwrap()
                .state,
            CredisState::Settled as u8
        );
    });
    teardown();
}

#[test]
fn false_token_return_rolls_back_credis_and_pledged_collateral() {
    let mut provider = env();
    provider.stub_sub_call_at_selector(
        asset(),
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
        assert!(runtime::settle(storage.clone(), bob(), id, pledge_stables()).is_err());
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
            .get_total_unallocated()
            .unwrap();
        call_and_lapse(&storage, id);
        runtime::forfeit_credis(storage.clone(), id).unwrap();
        assert!(runtime::forfeit_credis(storage.clone(), id).is_err());
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
                .get_total_unallocated()
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

#[test]
fn issuance_uses_reserved_terms_across_midnight_and_oracle_changes() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        let midnight = (CREATED_AT / DAY + 1) * DAY;
        advance_to(&storage, midnight - 300);
        let id = seed_reservation(&storage, alice(), alice(), pledge_stables());
        let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        pledge(&storage, alice(), id, 1);
        advance_to(&storage, midnight + 300);
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .policy_rate
            .write(&ISSUANCE_ISO, U256::from(999_999))
            .unwrap();
        // Neither a refreshed quote nor yesterday's VWAP is available after midnight.
        oracle.utc_day_vwap_last_finalized.write(0).unwrap();
        fund_stake(&storage, pledge_stake());
        let (credis_id, principal) =
            runtime::issue_credis(storage.clone(), cca(), id, pledge_stake()).unwrap();
        let record = CredisContract::new(storage).get_credis(credis_id).unwrap();
        assert_eq!(principal, reservation.amount);
        assert_eq!(record.gratis_minor, reservation.gratis_minor);
        assert_eq!(record.entry_price_minor, reservation.entry_price_minor);
        assert_eq!(record.policy_rate, reservation.policy_rate);
        assert_eq!(
            record.call_anchor_price_minor,
            reservation.call_anchor_price_minor
        );
        assert_eq!(
            record.call_price_minor,
            outbe_credis::calc_call_price(reservation.call_anchor_price_minor).unwrap()
        );
        assert_eq!(record.issued_at, midnight + 300);
        assert_eq!(record.last_settled_at, record.issued_at);
    });
    teardown();
}
