//! The registry of the target chains and the events of the outbox of IntexFactory.

use super::*;

use alloy_primitives::{Bytes, FixedBytes, B256};
use alloy_sol_types::SolEvent;
use outbe_intex::outbox::{Label, Message};
use outbe_intex::wire::{MSG_INTEX_SERIES, MSG_INTEX_SERIES_CALL_NOTICE};
use outbe_system_common::addresses::CROSSCHAIN_BRIDGE_ADDRESS;

use crate::targets::{self, pack_target, TargetMode};

fn register(s: &StorageHandle<'_>, chains: &[(u32, TargetMode)]) {
    let factory = IntexFactoryContract::new(s.clone());
    for &(chain, mode) in chains {
        factory
            .target_chains
            .push(pack_target(chain, mode))
            .unwrap();
    }
}

#[test]
fn the_registry_tells_the_series_chains_from_the_auction_chains() {
    with_factory(|s| {
        register(
            &s,
            &[
                (97, TargetMode::Auctions),
                (98, TargetMode::Series),
                (99, TargetMode::Off),
            ],
        );
        assert_eq!(
            targets::target_chains(&s).unwrap(),
            vec![(97, TargetMode::Auctions), (98, TargetMode::Series)]
        );
        assert_eq!(targets::series_target_chains(&s).unwrap(), vec![97, 98]);
        assert_eq!(targets::auction_target_chains(&s).unwrap(), vec![97]);
    });
}

#[test]
fn a_start_freezes_the_auction_chains_as_the_chains_of_the_day() {
    with_factory(|s| {
        register(&s, &[(97, TargetMode::Auctions), (98, TargetMode::Series)]);
        let day = EmissionDate::new(7);
        assert!(targets::day_targets(&s, day).unwrap().is_empty());
        assert_eq!(targets::freeze_day_targets(&s, day).unwrap(), vec![97]);
        IntexFactoryContract::new(s.clone())
            .target_chains
            .push(pack_target(96, TargetMode::Auctions))
            .unwrap();
        assert_eq!(targets::day_targets(&s, day).unwrap(), vec![97]);
    });
}

#[test]
fn the_views_show_the_registry_and_the_chains_of_a_day() {
    use alloy_sol_types::SolCall;
    with_factory(|s| {
        register(&s, &[(97, TargetMode::Auctions), (98, TargetMode::Series)]);
        targets::freeze_day_targets(&s, EmissionDate::new(7)).unwrap();
        let returned = precompile::dispatch(
            s.clone(),
            &IIntexFactory::targetChainsCall {}.abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap();
        let chains = IIntexFactory::targetChainsCall::abi_decode_returns(&returned).unwrap();
        assert_eq!(chains.chainIds, vec![97, 98]);
        assert_eq!(chains.modes, vec![2, 1]);
        let returned = precompile::dispatch(
            s.clone(),
            &IIntexFactory::targetsOfCall { emissionDay: 7 }.abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IIntexFactory::targetsOfCall::abi_decode_returns(&returned).unwrap(),
            vec![97]
        );
    });
}

fn call_notice(ids: &[SeriesId]) -> Message {
    let ids: Vec<FixedBytes<14>> = ids.iter().map(|id| (*id).into()).collect();
    Message {
        chain_id: 97,
        label: Label::new(MSG_INTEX_SERIES_CALL_NOTICE, 7, [ids.len() as u32, 0, 0]),
        message_gas: 1,
        payload: outbe_intex::wire::encode_intex_series_call_notice(7, 5, &ids).unwrap(),
    }
}

#[test]
fn a_sent_call_notice_emits_one_event_for_each_series() {
    let mut storage = factory_provider();
    storage.stub_sub_call_at(
        CROSSCHAIN_BRIDGE_ADDRESS,
        Bytes::from(B256::repeat_byte(0xEE).to_vec()),
    );
    StorageHandle::enter(&mut storage, |s| {
        register(&s, &[(97, TargetMode::Series)]);
        IntexFactoryContract::new(s.clone())
            .outbox()
            .push(call_notice(&[sid(7), sid(8)]))
            .unwrap();
        crate::outbox::flush(&s).unwrap();
    });
    let sent: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IIntexFactory::IntexSeriesCallNoticeSent::decode_log_data(log).ok())
        .map(|event| {
            (
                event.sendId,
                SeriesId::from(event.seriesId),
                event.dstChainId,
            )
        })
        .collect();
    assert_eq!(
        sent,
        vec![
            (B256::repeat_byte(0xEE), sid(7), 97),
            (B256::repeat_byte(0xEE), sid(8), 97)
        ]
    );
}

#[test]
fn a_failed_series_page_emits_its_send_failed_event() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(ISSUED_AT));
    StorageHandle::enter(&mut storage, |s| {
        register(&s, &[(97, TargetMode::Series)]);
        IntexFactoryContract::new(s.clone())
            .outbox()
            .push(Message {
                chain_id: 97,
                label: Label::new(MSG_INTEX_SERIES, 7, [1, 2, 3]),
                message_gas: 1,
                payload: Bytes::from(vec![1, MSG_INTEX_SERIES]),
            })
            .unwrap();
        crate::outbox::flush(&s).unwrap();
    });
    let failed: Vec<_> = storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IIntexFactory::IntexSeriesSendFailed::decode_log_data(log).ok())
        .map(|event| {
            (
                event.emissionDay,
                event.dstChainId,
                event.pageIndex,
                event.failures,
            )
        })
        .collect();
    assert_eq!(failed, vec![(7, 97, 1, 1)]);
}

#[test]
fn an_issued_day_puts_one_series_page_into_the_outbox_for_each_chain() {
    with_factory(|s| {
        let mut params = sample(7);
        params.target_chains = vec![97, 98];
        let legs = runtime::issue_intex(&s, params).unwrap();
        runtime::send_issuance(&s, legs).unwrap();
        let outbox = IntexFactoryContract::new(s.clone()).outbox();
        for chain in [97, 98] {
            let messages = outbox.messages(chain).unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(
                messages[0].label,
                Label::new(MSG_INTEX_SERIES, 7, [0, 1, 1])
            );
            assert_eq!(messages[0].message_gas, outbe_intex::gas::intex_series(1));
        }
    });
}
