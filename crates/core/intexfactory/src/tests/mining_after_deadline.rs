//! A paid Intex unit has no deadline: once settled it mines whenever its owner
//! chooses, including after the call deadline has closed the unpaid remainder.

use super::*;
use alloy_sol_types::SolEvent;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{
    decrypt_balance, derive_modify_key, derive_view_key, modify_mac, ModifyOperation,
};
use outbe_vaultrouter::settlement::SettlementPayment;

const RELAYER: Address = Address::repeat_byte(0x5e);

fn promis_auth(account: Address, amount: U256, nonce: u64) -> outbe_promisfactory::api::ModifyAuth {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    outbe_promisfactory::api::ModifyAuth {
        mac: modify_mac(
            &mk,
            &ModifyOperation {
                account,
                op: PromisOp::Mint,
                amount,
                op_nonce: nonce,
                chain_id: B256::from(U256::from(CHAIN_ID)),
            },
        ),
        op_nonce: nonce,
    }
}

/// Past the deadline the unpaid unit is refused settlement. The paid one mints
/// its Promis and moves the ledgers by exactly one unit.
#[test]
fn a_settled_unit_still_mines_after_the_call_deadline_while_unpaid_units_are_closed() {
    use crate::sol_ext::IERC1155;

    outbe_promis::enclave_client::test_enclave::install();
    let mut storage = factory_provider();
    // The NFT reports one unit in every class: one issued (unpaid) and one settled.
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(1),
    );
    let (promis_minor, nonce) = StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        runtime::issue_intex(&s, sample(7)).unwrap();
        outbe_intex::api::record_settled_units(&s, sid(7), 1).unwrap();
        outbe_intex::api::call_group_of(&s, sid(7), ISSUED_AT).unwrap();
        let promis_minor = outbe_intex::api::get_series(&s, sid(7))
            .unwrap()
            .promis_load_minor;
        let nonce = (0u64..)
            .find(|nonce| runtime::validate_pow(owner(), promis_minor, sid(7), 0, *nonce).is_ok())
            .unwrap();
        (promis_minor, nonce)
    });
    let close_at = u64::from(ISSUED_AT) + u64::from(CALL_NOTICE_PERIOD) + u64::from(TRANSIT_MARGIN);
    storage.set_timestamp(U256::from(close_at + 1));

    StorageHandle::enter(&mut storage, |s| {
        let series = outbe_intex::api::get_series(&s, sid(7)).unwrap();
        assert_eq!(
            outbe_intex::api::call_stamp(&s, &series).unwrap().called_at,
            ISSUED_AT
        );
        assert!(
            s.now_secs().unwrap() > close_at,
            "the fixture sits past the transit close"
        );

        // The unpaid unit is closed.
        let closed = runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            U256::ONE,
            SettlementPayment {
                payer: owner(),
                asset: settlement_asset(),
                snapshot_id: U256::ZERO,
            },
        )
        .unwrap_err();
        assert_eq!(
            closed.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::IntexFactoryError::SettlementDeadlinePassed,
            )
            .to_string(),
            "{closed}"
        );
        assert_eq!(outbe_intex::api::mined_units(&s, sid(7)).unwrap(), 0);

        // The paid unit still mines, relayed by a stranger.
        let call = IIntexFactory::minePromisCall {
            seriesId: sid(7).into(),
            owner: owner(),
            units: U256::ONE,
            nonce,
            mac: B256::from(promis_auth(owner(), promis_minor, 0).mac),
            opNonce: 0,
        };
        let minted =
            crate::precompile::dispatch(s.clone(), &call.abi_encode(), RELAYER, U256::ZERO)
                .unwrap();
        assert_eq!(
            IIntexFactory::minePromisCall::abi_decode_returns(&minted).unwrap(),
            promis_minor
        );
        assert!(
            outbe_intex::api::series_exists(&s, sid(7)).unwrap(),
            "its unpaid unit waits for the group's expiry to return it"
        );

        // The Promis landed on the owner's confidential ledger and consumed one op nonce.
        let sk = outbe_promis::enclave_client::test_enclave::state_key();
        let vk = derive_view_key(&sk, owner()).unwrap();
        let blob = outbe_promis::api::balance_ct(s.clone(), owner()).unwrap();
        assert_eq!(decrypt_balance(&vk, owner(), &blob).unwrap(), promis_minor);
        assert_eq!(outbe_promis::api::op_nonce(s.clone(), owner()).unwrap(), 1);
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .read_mine_seq(sid(7), owner())
                .unwrap(),
            1
        );
        assert!(outbe_promis::api::balance_ct(s.clone(), RELAYER)
            .unwrap()
            .is_empty());
        assert_eq!(outbe_promis::api::op_nonce(s.clone(), RELAYER).unwrap(), 0);
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .read_mine_seq(sid(7), RELAYER)
                .unwrap(),
            0
        );
    });
    let mined: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IIntexFactory::IntexMined::decode_log_data(log).ok())
        .collect();
    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].owner, owner());
    assert_eq!(mined[0].units, U256::ONE);
    assert_eq!(mined[0].promisMinor, promis_minor);
    outbe_promis::enclave_client::test_enclave::uninstall();
}

/// Once its group is forfeited, the settled units of a series still mine. Mining credits no
/// Promis Limit: the expiry already returned the unpaid units, and the last mined unit
/// deletes the spent series.
#[test]
fn settled_units_mine_after_the_group_expired_without_moving_the_promis_limit() {
    use crate::sol_ext::IERC1155;
    const SETTLED: u32 = 40;

    outbe_promis::enclave_client::test_enclave::install();
    let mut storage = factory_provider();
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(u64::from(SETTLED)),
    );
    let load = StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        runtime::issue_intex(&s, sample(7)).unwrap();
        outbe_intex::api::record_settled_units(&s, sid(7), SETTLED).unwrap();
        outbe_intex::api::call_group_of(&s, sid(7), ISSUED_AT).unwrap();
        outbe_intex::api::get_series(&s, sid(7))
            .unwrap()
            .promis_load_minor
    });
    let deadline = u64::from(ISSUED_AT) + u64::from(CALL_NOTICE_PERIOD);
    let due = IntexContract::day_end(IntexContract::deadline_day(deadline));
    storage.set_timestamp(U256::from(due));
    let forfeited = U256::from(100 - SETTLED) * load;
    let mined = U256::from(SETTLED) * load;

    StorageHandle::enter(&mut storage, |s| {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(2, due, CHAIN_ID),
            s.clone(),
        );
        crate::hooks::sweep_forfeits(&ctx).unwrap();
        crate::hooks::cleanup(&ctx).unwrap();
        let reserve = || {
            outbe_promislimit::PromisLimitContract::new(s.clone())
                .promis_limit()
                .unwrap()
        };
        assert_eq!(reserve(), forfeited);
        assert!(outbe_intex::api::series_exists(&s, sid(7)).unwrap());
        let counts = outbe_intex::api::unit_counts(&s, sid(7)).unwrap();
        assert_eq!(
            (counts.active, counts.settled, counts.forfeited),
            (0, SETTLED, 100 - SETTLED)
        );
        let intex = IntexContract::new(s.clone());
        assert_eq!(
            intex.called_group_slot.read(&group_of(&s, sid(7))).unwrap(),
            0
        );
        assert_eq!(intex.first_cleanup_day().unwrap(), None);

        let nonce = (0u64..)
            .find(|nonce| runtime::validate_pow(owner(), mined, sid(7), 0, *nonce).is_ok())
            .unwrap();
        let group = group_of(&s, sid(7));
        let minted = runtime::mine_promis(
            &s,
            sid(7),
            owner(),
            U256::from(SETTLED),
            runtime::MiningProof {
                nonce,
                auth: promis_auth(owner(), mined, 0),
            },
        )
        .unwrap();
        assert_eq!(minted, mined);
        assert_eq!(reserve(), forfeited, "mining credits no Promis Limit");
        assert!(!outbe_intex::api::series_exists(&s, sid(7)).unwrap());
        assert_eq!(intex.group_member_count(group).unwrap(), 0);
        let sk = outbe_promis::enclave_client::test_enclave::state_key();
        let vk = derive_view_key(&sk, owner()).unwrap();
        let blob = outbe_promis::api::balance_ct(s.clone(), owner()).unwrap();
        assert_eq!(decrypt_balance(&vk, owner(), &blob).unwrap(), mined);

        let later = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext::empty_for_tests(3, due + DAY, CHAIN_ID),
            s.clone(),
        );
        crate::hooks::sweep_forfeits(&later).unwrap();
        crate::hooks::cleanup(&later).unwrap();
        assert_eq!(reserve(), forfeited);
    });
    let forfeited_groups: Vec<_> = storage
        .get_events(outbe_primitives::addresses::INTEX_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_intex::precompile::IIntex::IntexGroupForfeited::decode_log_data(log).ok()
        })
        .map(|event| (event.forfeitedUnits, event.returnedPromisMinor))
        .collect();
    assert_eq!(
        forfeited_groups,
        vec![(u64::from(100 - SETTLED), forfeited)]
    );
    outbe_promis::enclave_client::test_enclave::uninstall();
}
