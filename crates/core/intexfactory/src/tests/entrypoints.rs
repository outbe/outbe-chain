use super::*;

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let data = IIntexFactory::settleIntexCall {
            intexId: outbe_intex::intex_id(sid(7)),
            owner: owner(),
            units: U256::from(1),
            asset: Address::ZERO,
            snapshotId: U256::ZERO,
            amountCapMinor: U256::ZERO,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, owner(), U256::from(1)).is_err());
    });
}

#[test]
fn dispatch_mine_promis_routes_to_runtime() {
    with_factory(|s| {
        // Missing series -> the runtime error surfaces through dispatch.
        let data = IIntexFactory::minePromisCall {
            intexId: outbe_intex::intex_id(sid(7)),
            owner: owner(),
            units: U256::from(1),
            nonce: 0,
            mac: alloy_primitives::FixedBytes([0u8; 32]),
            opNonce: 0,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, owner(), U256::ZERO).is_err());
    });
}

#[test]
fn dispatch_pow_difficulty_serves_the_protocol_difficulty() {
    with_factory(|s| {
        let out = precompile::dispatch(
            s.clone(),
            &IIntexFactory::powDifficultyCall {}.abi_encode(),
            owner(),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IIntexFactory::powDifficultyCall::abi_decode_returns(&out).unwrap(),
            outbe_core_common::pow::POW_DIFFICULTY
        );
    });
}

#[test]
fn config_dev_profile_drives_issuance() {
    with_factory(|s| {
        let f = outbe_intex::IntexContract::new(s.clone());
        // Select the dev profile through the single selector byte.
        f.config_profile
            .write(outbe_intex::config::PROFILE_TESTNET)
            .unwrap();
        assert_eq!(
            outbe_intex::config::read(&s).unwrap(),
            outbe_intex::config::IntexParams::TESTNET
        );

        // The auction start fixes the call terms from the live profile. Issuance seals them.
        let terms = crate::api::call_terms(&s).unwrap();
        let params = IssuanceParams {
            call_price_minor: terms.call_price(U256::from(ENTRY_PRICE)).unwrap(),
            call_trigger: terms.call_trigger,
            ..sample(7)
        };
        runtime::open_intex_series(&s, params).unwrap();

        // Issuance captures the dev call-trigger and dev-derived prices.
        let dev = outbe_intex::config::IntexParams::TESTNET;
        let r = outbe_intex::api::get_series(&s, sid(7)).unwrap();
        assert_eq!(r.call_notice_period_seconds, dev.call_notice_period_seconds);
        assert_eq!(
            r.call_price_minor,
            U256::from(ENTRY_PRICE * u64::from(100 + dev.call_rate) / 100)
        );
        assert_eq!(
            r.call_trigger(),
            outbe_intex::IntexCallTrigger {
                call_window_seconds: dev.call_window_seconds,
                call_threshold_seconds: dev.call_threshold_seconds,
                call_notice_period_seconds: dev.call_notice_period_seconds,
                transit_margin_seconds: dev.transit_margin_seconds,
            }
        );
    });
}

/// Slots are dense in `order` sequence, so a field inserted rather than appended moves every
/// one after it.
#[test]
fn intex_factory_slot_layout_is_pinned() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        let slots = [
            f.mine_seq.base_slot(),
            f.target_chains.base_slot(),
            f.day_targets.base_slot(),
            f.notify_head.slot(),
            f.notify_tail.slot(),
            f.notify_at.base_slot(),
            f.outbox_head.base_slot(),
            f.outbox_tail.base_slot(),
            f.outbox_meta.base_slot(),
            f.outbox_label.base_slot(),
            f.outbox_payload.base_slot(),
        ];
        for (index, slot) in slots.into_iter().enumerate() {
            assert_eq!(slot, U256::from(index), "field #{index}");
        }
    });
}

#[test]
fn a_node_local_failure_inside_a_flush_send_fails_the_flush_and_keeps_the_message() {
    use super::bridge_fault::BridgeFaultProvider;
    use crate::transport::outbox::{Label, Message};
    use alloy_sol_types::SolCall;

    let mut provider = BridgeFaultProvider::new(
        factory_provider(),
        crate::transport::outbox::callRemoteWithGasLimitCall::SELECTOR,
    );
    StorageHandle::enter(&mut provider, |s| {
        super::seed_series_targets(&s, &[97]);
        IntexFactoryContract::new(s)
            .outbox()
            .push(Message {
                chain_id: 97,
                label: Label::new(crate::transport::wire::MSG_INTEX_SERIES, 7, [0, 1, 1]),
                message_gas: 1,
                payload: alloy_primitives::Bytes::from(vec![1u8]),
            })
            .unwrap();
    });
    let pending = |provider: &mut BridgeFaultProvider| {
        StorageHandle::enter(provider, |s| {
            IntexFactoryContract::new(s).outbox().len(97).unwrap()
        })
    };

    let failed = StorageHandle::enter(&mut provider, |s| crate::outbox::flush(&s));
    assert!(matches!(
        failed,
        Err(outbe_primitives::error::PrecompileError::SubCall(
            outbe_primitives::storage::SubCallError::DatabaseError(_)
        ))
    ));
    assert_eq!(pending(&mut provider), 1);

    provider.heal();
    StorageHandle::enter(&mut provider, |s| crate::outbox::flush(&s)).unwrap();
    assert_eq!(pending(&mut provider), 0);
}
