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
            outbe_common::pow::POW_DIFFICULTY
        );
    });
}

#[test]
fn config_dev_profile_drives_issuance() {
    with_factory(|s| {
        let f = outbe_intex::IntexContract::new(s.clone());
        // Select the dev profile through the single selector byte.
        f.config_profile
            .write(outbe_intex::config::PROFILE_DEV)
            .unwrap();
        assert_eq!(
            outbe_intex::config::read(&s).unwrap(),
            outbe_intex::config::IntexParams::DEV
        );

        // The auction start fixes the call terms from the live profile. Issuance seals them.
        let terms = crate::api::call_terms(&s).unwrap();
        let params = IssuanceParams {
            call_price_minor: terms.call_price(U256::from(ENTRY_PRICE)).unwrap(),
            call_trigger: terms.call_trigger,
            ..sample(7)
        };
        runtime::issue_intex(&s, params).unwrap();

        // Issuance captures the dev call-trigger and dev-derived prices.
        let dev = outbe_intex::config::IntexParams::DEV;
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
            f.notify_head.slot(),
            f.notify_tail.slot(),
            f.notify_at.base_slot(),
        ];
        for (index, slot) in slots.into_iter().enumerate() {
            assert_eq!(slot, U256::from(index), "field #{index}");
        }
    });
}

fn send_notices_in_block(
    provider: &mut super::router_fault::RouterFaultProvider,
) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: ISSUED_AT,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: u64::max(ISSUED_AT, 1),
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        );
        // The cycle runtime runs each trigger inside its own checkpoint.
        s.with_checkpoint(|| crate::notify::send_call_notices(&ctx))
    })
}

#[test]
fn a_node_local_failure_inside_the_call_notice_send_fails_the_drain() {
    use super::router_fault::RouterFaultProvider;
    use crate::notify::{enqueue_notice, pack_call_notice};
    use crate::sol_ext::IOriginRouter;

    let mut provider = RouterFaultProvider::new(
        factory_provider(),
        IOriginRouter::sendMarkCalledCall::SELECTOR,
    );
    let queued = [
        pack_call_notice(sid(7), ISSUED_AT),
        pack_call_notice(sid(8), ISSUED_AT),
    ];
    StorageHandle::enter(&mut provider, |s| {
        for day in [7, 8] {
            outbe_intex::api::create_series(
                &s,
                outbe_intex::CreateSeriesParams {
                    series_id: sid(day),
                    emission_day: EmissionDate::new(day),
                    issued_units: 1,
                    promis_load_minor: 1,
                    entry_price_minor: U256::from(1u64),
                    call_price_minor: U256::from(2u64),
                    call_trigger: prod_trigger(),
                    issued_at: ISSUED_AT,
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            )
            .unwrap();
        }
        let factory = IntexFactoryContract::new(s);
        for entry in queued {
            enqueue_notice(&factory, entry).unwrap();
        }
    });
    let queue = |provider: &mut RouterFaultProvider| {
        StorageHandle::enter(provider, |s| {
            let factory = IntexFactoryContract::new(s);
            (
                factory.notify_head.read().unwrap(),
                factory.notify_tail.read().unwrap(),
                [0, 1].map(|index| factory.notify_at.read(&index).unwrap()),
            )
        })
    };

    let result = send_notices_in_block(&mut provider);
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::SubCall(
            outbe_primitives::storage::SubCallError::DatabaseError(_)
        ))
    ));
    assert_eq!(queue(&mut provider), (0, 2, queued));

    provider.heal();
    send_notices_in_block(&mut provider).unwrap();
    assert_eq!(queue(&mut provider), (0, 0, [U256::ZERO; 2]));
}
