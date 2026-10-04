//! A paid Intex unit has no deadline: once settled it mines whenever its owner
//! chooses, including after the call deadline has closed the unpaid remainder.

use super::*;
use alloy_sol_types::SolEvent;
use outbe_tee::protocol::PromisOp;
use outbe_tee_enclave::promis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

fn promis_auth(account: Address, amount: U256, nonce: u64) -> outbe_promisfactory::api::ModifyAuth {
    let sk = outbe_promis::enclave_client::test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    outbe_promisfactory::api::ModifyAuth {
        mac: modify_mac(
            &mk,
            account,
            PromisOp::Mint,
            amount,
            nonce,
            B256::from(U256::from(CHAIN_ID)),
        ),
        op_nonce: nonce,
    }
}

/// One settled unit survives the deadline: settlement of the unpaid remainder
/// is refused, the paid unit still mints its Promis and leaves the ledgers
/// exactly one unit further along.
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
        runtime::issue(&s, sample(7)).unwrap();
        seed_qualifying_day(&s);
        outbe_intex::api::record_settled_units(&s, sid(7), 1).unwrap();
        outbe_intex::api::mark_called(&s, sid(7), ISSUED_AT).unwrap();
        let promis_minor = outbe_intex::api::read_series(&s, sid(7))
            .unwrap()
            .promis_load_minor;
        let nonce = (0u64..)
            .find(|nonce| runtime::validate_pow(owner(), promis_minor, sid(7), 0, *nonce).is_ok())
            .unwrap();
        (promis_minor, nonce)
    });
    let deadline = u64::from(ISSUED_AT) + u64::from(CALL_NOTICE_PERIOD);
    storage.set_timestamp(U256::from(deadline + 1));

    StorageHandle::enter(&mut storage, |s| {
        let series = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(series.called_at, ISSUED_AT);
        assert!(
            s.timestamp().unwrap().to::<u64>() > deadline,
            "the fixture sits past the settlement deadline"
        );

        // The unpaid unit is closed.
        let closed = runtime::settle_intex(
            &s,
            sid(7),
            owner(),
            owner(),
            U256::ONE,
            payment_token(),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(
            closed.to_string().to_lowercase().contains("deadline"),
            "{closed}"
        );
        assert_eq!(outbe_intex::api::exercised_units(&s, sid(7)).unwrap(), 0);

        // The paid unit still mines.
        let minted = runtime::mine_promis(
            &s,
            sid(7),
            owner(),
            U256::ONE,
            nonce,
            promis_auth(owner(), promis_minor, 0),
        )
        .unwrap();
        assert_eq!(minted, promis_minor);
        assert_eq!(outbe_intex::api::exercised_units(&s, sid(7)).unwrap(), 1);

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
    });
    let mined: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IIntexFactory::PromisMined::decode_log_data(log).ok())
        .collect();
    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].owner, owner());
    assert_eq!(mined[0].units, U256::ONE);
    assert_eq!(mined[0].promisMinor, promis_minor);
    outbe_promis::enclave_client::test_enclave::uninstall();
}
