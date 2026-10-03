//! Real-proof issuance plus atomic repayment/forfeiture tests.
use crate::{runtime, tests::common::*};
use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_credis::{CredisContract, CredisState};
use outbe_gratis::{client, pledge::PledgePool};
use outbe_primitives::{addresses::CREDIS_ADDRESS, storage::StorageHandle};
use outbe_vaultrouter::VaultRouterContract;

#[test]
fn issue_binds_complete_reservation_and_rolls_back_failed_claims() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost() * U256::from(2));
        let id = seed_reservation(&storage, alice(), pledge_stables());
        let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        let context =
            runtime::reservation_context(CHAIN_ID, id, &reservation.clone().into()).unwrap();
        let note = pledge_note(&storage, alice(), pledge_cost() * U256::from(2), 1);
        let proof = prove_latest(&storage, &note, pledge_cost(), context);
        fund_stake(&storage, pledge_stake());
        let pool = PledgePool::new(storage.clone());
        let root = pool.current_root.read().unwrap();
        let source = outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
        let vars = [0, 1, 2, 3, 4, 5, 6, 7, 8];
        for variant in vars {
            let mut altered = reservation.clone();
            match variant {
                0 => altered.smart_account = bob(),
                1 => altered.amount += U256::ONE,
                2 => altered.collateral += U256::ONE,
                3 => altered.snapshot_id += U256::ONE,
                4 => altered.policy_rate += U256::ONE,
                5 => altered.reference_currency += 1,
                6 => altered.call_anchor_price += U256::ONE,
                7 => altered.expires_at += 1,
                _ => altered.cca = bob(),
            }
            deploy_smart_account(&storage, bob());
            VaultRouterContract::new(storage.clone())
                .reservations
                .update(&altered)
                .unwrap();
            assert!(
                runtime::issue_credis(storage.clone(), cca(), id, &proof, pledge_stake()).is_err()
            );
            assert_eq!(pool.current_root.read().unwrap(), root);
            assert!(!pool
                .spent_nullifiers
                .read(&note.nullifier().unwrap())
                .unwrap());
            assert_eq!(view_balance(&storage, CREDIS_ADDRESS), U256::ZERO);
        }
        VaultRouterContract::new(storage.clone())
            .reservations
            .update(&reservation)
            .unwrap();
        assert!(runtime::issue_credis(storage.clone(), cca(), id, &proof, U256::ZERO).is_err());
        let bad_amount = prove_latest(&storage, &note, pledge_cost() - U256::ONE, context);
        assert!(
            runtime::issue_credis(storage.clone(), cca(), id, &bad_amount, pledge_stake()).is_err()
        );
        assert_eq!(pool.current_root.read().unwrap(), root);
        let (position_id, amount) =
            runtime::issue_credis(storage.clone(), cca(), id, &proof, pledge_stake()).unwrap();
        assert_eq!(amount, reservation.amount);
        assert_eq!(
            view_balance(&storage, CREDIS_ADDRESS),
            reservation.collateral
        );
        assert_eq!(
            outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap(),
            source
        );
        assert_eq!(pool.leaf_count.read().unwrap(), 2);
        let position = CredisContract::new(storage.clone())
            .get_position(position_id)
            .unwrap();
        assert_eq!(position.policy_rate, reservation.policy_rate);
        assert_eq!(
            position.call_price_minor,
            outbe_credis::calc_call_price(reservation.call_anchor_price).unwrap()
        );
        assert_eq!(
            position.call_notice_period_seconds,
            outbe_credis::constants::CALL_NOTICE_PERIOD
        );
        assert!(runtime::issue_credis(storage.clone(), cca(), id, &proof, pledge_stake()).is_err());
    });
    teardown();
}

#[test]
fn repayments_append_distinct_source_owned_notes_and_interest_only_appends_nothing() {
    let mut provider = env();
    let (original, context, position_id, returned) =
        StorageHandle::enter(&mut provider, |storage| {
            bootstrap(&storage, pledge_cost());
            let id = seed_reservation(&storage, alice(), pledge_stables());
            let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
            let context = runtime::reservation_context(CHAIN_ID, id, &reservation.into()).unwrap();
            let original = pledge_note(&storage, alice(), pledge_cost(), 1);
            let proof = prove_latest(&storage, &original, pledge_cost(), context);
            fund_stake(&storage, pledge_stake());
            let position_id =
                runtime::issue_credis(storage.clone(), cca(), id, &proof, pledge_stake())
                    .unwrap()
                    .0;
            let source = outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
            let fidelity = outbe_fidelity::FidelityContract::new(storage.clone())
                .cohorts_ct_of(alice())
                .unwrap();
            advance_to(&storage, CREATED_AT + DAY);
            let position = CredisContract::new(storage.clone())
                .get_position(position_id)
                .unwrap();
            let interest = CredisContract::accrued_interest(&position, CREATED_AT + DAY).unwrap();
            assert!(!interest.is_zero());
            runtime::settle(storage.clone(), bob(), position_id, interest).unwrap();
            assert_eq!(
                PledgePool::new(storage.clone()).leaf_count.read().unwrap(),
                1
            );
            let released = pledge_cost() / U256::from(2);
            settle_principal(
                &storage,
                bob(),
                position_id,
                pledge_stables() / U256::from(2),
            );
            settle_principal(
                &storage,
                bob(),
                position_id,
                pledge_stables() / U256::from(2),
            );
            let first = original
                .returned(context, position_id, released, released)
                .unwrap();
            let second = original
                .returned(context, position_id, released, pledge_cost())
                .unwrap();
            assert_ne!(first.commitment().unwrap(), second.commitment().unwrap());
            let pool = PledgePool::new(storage.clone());
            assert!(pool.commitments.read(&first.commitment().unwrap()).unwrap());
            assert!(pool
                .commitments
                .read(&second.commitment().unwrap())
                .unwrap());
            assert_eq!(pool.leaf_count.read().unwrap(), 3);
            assert_eq!(view_balance(&storage, CREDIS_ADDRESS), U256::ZERO);
            assert_eq!(
                outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
                pledge_cost()
            );
            assert_eq!(
                outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap(),
                source
            );
            assert_eq!(
                outbe_fidelity::FidelityContract::new(storage.clone())
                    .cohorts_ct_of(alice())
                    .unwrap(),
                fidelity
            );
            (original, context, position_id, vec![first, second])
        });
    // Returned notes use the same tree, membership and withdrawal circuit as deposits.
    StorageHandle::enter(&mut provider, |storage| {
        let mut tree = client::new_tree(CHAIN_ID).unwrap();
        for note in std::iter::once(&original).chain(returned.iter()) {
            tree.append(
                outbe_protocol::codec::field_from_b256(&note.commitment().unwrap()).unwrap(),
            )
            .unwrap();
        }
        for note in returned {
            let context =
                outbe_gratis::api::unpledge_context(CHAIN_ID, alice(), note.amount).unwrap();
            let proof = client::prove_unpledge(&note, &tree, note.amount, context).unwrap();
            outbe_gratis::api::unpledge(storage.clone(), &proof).unwrap();
        }
        assert_eq!(view_balance(&storage, alice()), pledge_cost());
        assert_eq!(view_balance(&storage, bob()), U256::ZERO);
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        assert_eq!(
            CredisContract::new(storage)
                .get_position(position_id)
                .unwrap()
                .state,
            CredisState::Settled as u8
        );
    });
    let _ = context;
    teardown();
}

#[test]
fn false_token_return_rolls_back_position_and_encrypted_backing() {
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
        let position = credis.get_position(id).unwrap();
        let root = PledgePool::new(storage.clone())
            .current_root
            .read()
            .unwrap();
        let backing = outbe_gratis::api::balance_ct(storage.clone(), CREDIS_ADDRESS).unwrap();
        assert!(runtime::settle(storage.clone(), bob(), id, pledge_stables()).is_err());
        assert_eq!(credis.get_position(id).unwrap(), position);
        assert_eq!(
            PledgePool::new(storage.clone())
                .current_root
                .read()
                .unwrap(),
            root
        );
        assert_eq!(
            outbe_gratis::api::balance_ct(storage, CREDIS_ADDRESS).unwrap(),
            backing
        );
    });
    teardown();
}

#[test]
fn forfeit_burns_only_remaining_position_backing_and_leaves_fidelity_untouched() {
    let mut provider = env();
    StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        bootstrap_for(&storage, bob(), pledge_cost());
        let id = open(&storage, 1);
        let other = open_for(&storage, bob(), 1);
        let fidelity = outbe_fidelity::FidelityContract::new(storage.clone())
            .cohorts_ct_of(alice())
            .unwrap();
        let source = outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
        let credis = CredisContract::new(storage.clone());
        let mut p = credis.get_position(id).unwrap();
        p.state = CredisState::Called as u8;
        p.called_at = CREATED_AT;
        credis.positions.update(&p).unwrap();
        credis.called_position_counts.write(&alice(), 1).unwrap();
        advance_to(&storage, CREATED_AT + NOTICE + 1);
        runtime::void_position(storage.clone(), id).unwrap();
        assert!(runtime::void_position(storage.clone(), id).is_err());
        assert_eq!(view_balance(&storage, CREDIS_ADDRESS), pledge_cost());
        assert_eq!(
            credis.get_position(other).unwrap().outstanding_gratis_minor,
            pledge_cost()
        );
        assert_eq!(
            outbe_gratis::api::total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        assert_eq!(
            outbe_gratis::api::pledged_total_supply(storage.clone()).unwrap(),
            pledge_cost()
        );
        assert_eq!(
            outbe_fidelity::FidelityContract::new(storage.clone())
                .cohorts_ct_of(alice())
                .unwrap(),
            fidelity
        );
        assert_eq!(
            outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap(),
            source
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
        let id = seed_reservation(&storage, alice(), pledge_stables());
        let reservation = outbe_vaultrouter::api::reservation_of(&storage, id).unwrap();
        let context =
            runtime::reservation_context(CHAIN_ID, id, &reservation.clone().into()).unwrap();
        let note = pledge_note(&storage, alice(), pledge_cost(), 1);
        let proof = prove_latest(&storage, &note, pledge_cost(), context);
        advance_to(&storage, midnight + 300);
        let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
        oracle
            .policy_rate
            .write(&ISSUANCE_ISO, U256::from(999_999))
            .unwrap();
        // Neither a refreshed quote nor yesterday's VWAP is available after midnight.
        oracle.utc_day_vwap_last_finalized.write(0).unwrap();
        fund_stake(&storage, pledge_stake());
        let (position_id, principal) =
            runtime::issue_credis(storage.clone(), cca(), id, &proof, pledge_stake()).unwrap();
        let position = CredisContract::new(storage)
            .get_position(position_id)
            .unwrap();
        assert_eq!(principal, reservation.amount);
        assert_eq!(position.gratis_minor, reservation.collateral);
        assert_eq!(position.entry_price_minor, reservation.entry_price);
        assert_eq!(position.policy_rate, reservation.policy_rate);
        assert_eq!(
            position.call_anchor_price_minor,
            reservation.call_anchor_price
        );
        assert_eq!(
            position.call_price_minor,
            outbe_credis::calc_call_price(reservation.call_anchor_price).unwrap()
        );
        assert_eq!(position.issued_at, midnight + 300);
        assert_eq!(position.last_settled_at, position.issued_at);
    });
    teardown();
}

#[test]
fn issue_repay_and_forfeit_never_access_source_storage_or_fidelity() {
    use outbe_primitives::addresses::{FIDELITY_ADDRESS, GRATIS_ADDRESS};
    let mut provider = env();
    let (reservation_id, proof) = StorageHandle::enter(&mut provider, |storage| {
        bootstrap(&storage, pledge_cost());
        deploy_smart_account(&storage, bob());
        let reservation_id = seed_reservation(&storage, bob(), pledge_stables());
        let record = outbe_vaultrouter::api::reservation_of(&storage, reservation_id).unwrap();
        let context =
            runtime::reservation_context(CHAIN_ID, reservation_id, &record.into()).unwrap();
        let note = pledge_note(&storage, alice(), pledge_cost(), 1);
        fund_stake(&storage, pledge_stake());
        (
            reservation_id,
            prove_latest(&storage, &note, pledge_cost(), context),
        )
    });
    provider.enable_storage_trace();
    StorageHandle::enter(&mut provider, |storage| {
        outbe_gratis::api::balance_ct(storage.clone(), alice()).unwrap();
        outbe_gratis::api::op_nonce(storage, alice()).unwrap();
    });
    let source_slots: Vec<_> = provider
        .storage_trace()
        .iter()
        .map(|op| (op.address, op.slot))
        .collect();
    assert!(source_slots
        .iter()
        .all(|(address, _)| *address == GRATIS_ADDRESS));
    provider.enable_storage_trace();
    StorageHandle::enter(&mut provider, |storage| {
        let id = runtime::issue_credis(
            storage.clone(),
            cca(),
            reservation_id,
            &proof,
            pledge_stake(),
        )
        .unwrap()
        .0;
        runtime::settle(storage.clone(), bob(), id, pledge_stables() / U256::from(2)).unwrap();
        let credis = CredisContract::new(storage.clone());
        let mut position = credis.get_position(id).unwrap();
        position.state = CredisState::Called as u8;
        position.called_at = CREATED_AT;
        credis.positions.update(&position).unwrap();
        credis.called_position_counts.write(&bob(), 1).unwrap();
        advance_to(&storage, CREATED_AT + NOTICE + 1);
        runtime::void_position(storage, id).unwrap();
    });
    for op in provider.storage_trace() {
        assert_ne!(
            op.address, FIDELITY_ADDRESS,
            "Fidelity storage accessed: {op:?}"
        );
        assert!(
            !source_slots.contains(&(op.address, op.slot)),
            "source storage accessed: {op:?}"
        );
    }
    teardown();
}
