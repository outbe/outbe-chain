//! The outbox sends the messages of each chain in order, keeps a failed one at the head for its
//! next try, and lets the other chains go on.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::CROSSCHAIN_BRIDGE_ADDRESS;

use crate::transport::gas::ica_gas_limit;
use crate::transport::outbox::{
    callRemoteWithGasLimitCall, receiveFromOutbeCall, Label, Message, Outbox, Outcome,
    RETRY_SECONDS,
};

const NOW: u64 = 1_700_000_000;
const MODULE: Address = Address::repeat_byte(0x77);

fn outbox<'s>(s: &StorageHandle<'s>) -> Outbox<'s> {
    use outbe_primitives::storage::dsl::Map;
    let map = |slot: u64| U256::from(slot);
    Outbox {
        head: Map::new(map(1), MODULE, s.clone()),
        tail: Map::new(map(2), MODULE, s.clone()),
        meta: Map::new(map(3), MODULE, s.clone()),
        label: Map::new(map(4), MODULE, s.clone()),
        payload: Map::new(map(5), MODULE, s.clone()),
    }
}

fn message(chain_id: u32, day: u32) -> Message {
    Message {
        chain_id,
        label: Label::new(3, day, [1, 2, 3]),
        message_gas: 500_000,
        payload: Bytes::from(vec![1, 3, day as u8]),
    }
}

fn provider() -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(1);
    storage.set_timestamp(U256::from(NOW));
    storage
}

fn accept_sends(storage: &mut HashMapStorageProvider) {
    storage.stub_sub_call_at(
        CROSSCHAIN_BRIDGE_ADDRESS,
        Bytes::from(B256::repeat_byte(0xAB).to_vec()),
    );
}

fn flush(s: &StorageHandle<'_>, chains: &[u32], now: u64, budget: u32) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    outbox(s)
        .flush(s, chains, now, budget, |outcome| {
            outcomes.push(outcome);
            Ok(())
        })
        .unwrap();
    outcomes
}

#[test]
fn a_chain_gets_its_messages_in_order_through_the_bridge() {
    let mut storage = provider();
    accept_sends(&mut storage);
    let outcomes = StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(message(97, 1)).unwrap();
        outbox(&s).push(message(97, 2)).unwrap();
        let outcomes = flush(&s, &[97], NOW, 64);
        assert_eq!(outbox(&s).len(97).unwrap(), 0);
        assert_eq!(outbox(&s).head.read(&97).unwrap(), 0);
        assert_eq!(outbox(&s).tail.read(&97).unwrap(), 0);
        outcomes
    });
    let days: Vec<u32> = outcomes
        .iter()
        .map(|outcome| match outcome {
            Outcome::Sent {
                label, message_id, ..
            } => {
                assert_eq!(*message_id, B256::repeat_byte(0xAB));
                label.emission_day
            }
            Outcome::Failed { .. } => panic!("no send fails"),
        })
        .collect();
    assert_eq!(days, vec![1, 2]);

    let call = storage
        .sub_call_inputs()
        .iter()
        .find(|input| input.target == CROSSCHAIN_BRIDGE_ADDRESS)
        .map(|input| callRemoteWithGasLimitCall::abi_decode(&input.calldata).unwrap())
        .unwrap();
    assert_eq!(call.domain, 97);
    assert_eq!(call.gasLimit, U256::from(ica_gas_limit(500_000)));
    assert_eq!(call.calls.len(), 1);
    assert_eq!(
        call.calls[0].to,
        outbe_core_common::addresses::TARGET_ROUTER_ADDRESS
    );
    assert_eq!(call.calls[0].value, U256::ZERO);
    assert_eq!(
        receiveFromOutbeCall::abi_decode(&call.calls[0].data)
            .unwrap()
            .message,
        message(97, 1).payload
    );
}

#[test]
fn a_failed_send_stays_at_the_head_and_waits_its_pause() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(message(97, 1)).unwrap();
        outbox(&s).push(message(97, 2)).unwrap();
        let outcomes = flush(&s, &[97], NOW, 64);
        assert!(matches!(
            outcomes.as_slice(),
            [Outcome::Failed { failures: 1, label, .. }] if label.emission_day == 1
        ));
        let head = outbox(&s).head_of(97).unwrap().unwrap();
        assert_eq!(head.failures, 1);
        assert_eq!(head.next_try, NOW + RETRY_SECONDS);
        assert_eq!(outbox(&s).len(97).unwrap(), 2);

        assert!(flush(&s, &[97], NOW + RETRY_SECONDS - 1, 64).is_empty());
        let outcomes = flush(&s, &[97], NOW + RETRY_SECONDS, 64);
        assert!(matches!(
            outcomes.as_slice(),
            [Outcome::Failed { failures: 2, .. }]
        ));
    });
}

#[test]
fn a_stuck_chain_does_not_hold_the_other_chains() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(message(97, 1)).unwrap();
        outbox(&s).push(message(98, 2)).unwrap();
    });
    storage.stub_sub_call_at_selector(
        CROSSCHAIN_BRIDGE_ADDRESS,
        callRemoteWithGasLimitCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xAB).to_vec()),
    );
    StorageHandle::enter(&mut storage, |s| {
        let outcomes = flush(&s, &[97, 98], NOW, 64);
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outbox(&s).len(97).unwrap(), 0);
        assert_eq!(outbox(&s).len(98).unwrap(), 0);
    });
}

#[test]
fn the_budget_bounds_the_sends_of_one_flush() {
    let mut storage = provider();
    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |s| {
        for day in 1..=3 {
            outbox(&s).push(message(97, day)).unwrap();
        }
        assert_eq!(flush(&s, &[97], NOW, 2).len(), 2);
        assert_eq!(outbox(&s).len(97).unwrap(), 1);
        assert_eq!(
            outbox(&s).head_of(97).unwrap().unwrap().label.emission_day,
            3
        );
    });
}

#[test]
fn a_chain_outside_the_list_is_not_tried() {
    let mut storage = provider();
    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(message(97, 1)).unwrap();
        assert!(flush(&s, &[98], NOW, 64).is_empty());
        assert_eq!(outbox(&s).len(97).unwrap(), 1);
    });
}

#[test]
fn a_flush_with_too_little_gas_stops_without_a_failed_try() {
    let mut storage = provider();
    accept_sends(&mut storage);
    storage.set_gas_limit(crate::transport::outbox::SEND_GAS_RESERVE - 1);
    StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(message(97, 1)).unwrap();
        assert!(flush(&s, &[97], NOW, 64).is_empty());
        let head = outbox(&s).head_of(97).unwrap().unwrap();
        assert_eq!((head.failures, head.next_try), (0, 0));
    });
}

#[test]
fn the_label_and_the_meta_keep_every_field() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |s| {
        let label = Label::new(7, 20_260_101, [u32::MAX, 0, 255]);
        outbox(&s)
            .push(Message {
                chain_id: u32::MAX,
                label,
                message_gas: u64::MAX,
                payload: Bytes::from(vec![9; 100]),
            })
            .unwrap();
        let messages = outbox(&s).messages(u32::MAX).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].label, label);
        assert_eq!(messages[0].message_gas, u64::MAX);
        assert_eq!(messages[0].payload, Bytes::from(vec![9; 100]));
    });
}

#[test]
fn gas_that_runs_out_inside_a_message_keeps_the_messages_sent_before_it() {
    let small = message(97, 1);
    let big = Message {
        payload: Bytes::from(vec![7; 30_000]),
        ..message(97, 2)
    };
    let mut alone = provider();
    accept_sends(&mut alone);
    StorageHandle::enter(&mut alone, |s| outbox(&s).push(small.clone()).unwrap());
    alone.set_gas_limit(u64::MAX);
    alone.enable_production_storage_gas_metering();
    let one_send = StorageHandle::enter(&mut alone, |s| {
        assert_eq!(flush(&s, &[97], NOW, 64).len(), 1);
        s.gas_used().unwrap()
    });

    let mut storage = provider();
    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |s| {
        outbox(&s).push(small).unwrap();
        outbox(&s).push(big).unwrap();
    });
    storage.set_gas_limit(one_send + crate::transport::outbox::SEND_GAS_RESERVE + 1_000);
    storage.enable_production_storage_gas_metering();
    let outcomes = StorageHandle::enter(&mut storage, |s| flush(&s, &[97], NOW, 64));
    assert!(matches!(
        outcomes.as_slice(),
        [Outcome::Sent { label, .. }] if label.emission_day == 1
    ));
    storage.set_gas_limit(u64::MAX);
    StorageHandle::enter(&mut storage, |s| {
        assert_eq!(outbox(&s).len(97).unwrap(), 1);
        let head = outbox(&s).head_of(97).unwrap().unwrap();
        assert_eq!(
            (head.label.emission_day, head.failures, head.next_try),
            (2, 0, 0)
        );
    });
}
