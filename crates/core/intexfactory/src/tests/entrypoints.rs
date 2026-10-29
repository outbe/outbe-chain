use super::*;

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let data = IIntexFactory::settleIntexCall {
            seriesId: sid(7).into(),
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
            seriesId: sid(7).into(),
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
            f.parked_message_cursor.slot(),
            f.parked_proceeds_cursor.slot(),
        ];
        for (index, slot) in slots.into_iter().enumerate() {
            assert_eq!(slot, U256::from(index), "field #{index}");
        }
    });
}

/// The sweep reads the router before it acts, so a router that answers nothing (or is not there at
/// all) has to leave the block alone rather than fail it.
#[test]
fn the_parked_sweep_is_a_no_op_without_a_router() {
    with_factory(|s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, ISSUED_AT as u64, CHAIN_ID),
            s.clone(),
        );
        crate::parked::resend_parked_messages(&ctx).expect("a silent router is not a failed block");

        let factory = IntexFactoryContract::new(s.clone());
        assert_eq!(factory.parked_message_cursor.read().unwrap(), 0);
        assert_eq!(factory.parked_proceeds_cursor.read().unwrap(), 0);
    });
}

fn parked_router(
    provider: &mut HashMapStorageProvider,
    count: [u8; 4],
    entry: [u8; 4],
    ret: Vec<u8>,
) {
    let router = crate::constants::ORIGIN_ROUTER_ADDRESS;
    provider.stub_sub_call_at_selector(router, count, word(1));
    provider.stub_sub_call_at_selector(router, entry, ret.into());
}

fn drain_parked(provider: &mut HashMapStorageProvider) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, ISSUED_AT as u64, CHAIN_ID),
            s,
        );
        crate::parked::resend_parked_messages(&ctx)
    })
}

#[test]
fn a_node_local_failure_while_draining_parked_messages_fails_the_sweep() {
    use crate::sol_ext::IOriginRouter;

    let mut provider = factory_provider();
    parked_router(
        &mut provider,
        IOriginRouter::parkedMessageCountCall::SELECTOR,
        IOriginRouter::parkedMessageCall::SELECTOR,
        IOriginRouter::parkedMessageCall::abi_encode_returns(&IOriginRouter::ParkedMessage {
            dstChainId: 56,
            gasLimit: 1,
            sent: false,
            payload: vec![1].into(),
        }),
    );
    provider.fail_mutation_at(0);
    let result = drain_parked(&mut provider);
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |s| {
        let factory = IntexFactoryContract::new(s);
        assert_eq!(factory.parked_message_cursor.read().unwrap(), 0);
    });

    drain_parked(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |s| {
        let factory = IntexFactoryContract::new(s);
        assert_eq!(factory.parked_message_cursor.read().unwrap(), 1);
    });
}

#[test]
fn a_node_local_failure_while_draining_parked_proceeds_fails_the_sweep() {
    use crate::sol_ext::IOriginRouter;

    let mut provider = factory_provider();
    parked_router(
        &mut provider,
        IOriginRouter::parkedProceedsCountCall::SELECTOR,
        IOriginRouter::parkedProceedsCall::SELECTOR,
        IOriginRouter::parkedProceedsCall::abi_encode_returns(&IOriginRouter::ParkedProceeds {
            worldwideDay: 2026_0301,
            srcChainId: 56,
            amount: 5,
            settled: false,
        }),
    );
    // The message cursor is written first, even with nothing parked.
    provider.fail_mutation_at(1);
    let result = drain_parked(&mut provider);
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |s| {
        let factory = IntexFactoryContract::new(s);
        assert_eq!(factory.parked_proceeds_cursor.read().unwrap(), 0);
    });

    drain_parked(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |s| {
        let factory = IntexFactoryContract::new(s);
        assert_eq!(factory.parked_proceeds_cursor.read().unwrap(), 1);
    });
}

fn send_notices_in_block(
    provider: &mut super::router_fault::RouterFaultProvider,
) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, ISSUED_AT as u64, CHAIN_ID),
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
                    worldwide_day: WorldwideDay::new(day),
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
