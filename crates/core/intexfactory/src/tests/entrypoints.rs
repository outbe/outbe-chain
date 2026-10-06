use super::*;

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let data = IIntexFactory::settleIntexWithPayNoteCall {
            seriesId: sid(7).into(),
            owner: owner(),
            units: U256::from(1),
            payNoteProof: Default::default(),
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
fn config_unset_resolves_by_chain_id() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        // Undo the fixture's explicit choice: an unset selector resolves by
        // network, and CHAIN_ID is not mainnet.
        f.config_profile.write(crate::config::PROFILE_AUTO).unwrap();
        assert_eq!(
            crate::config::read(&s).unwrap(),
            crate::config::IntexParams::DEV
        );
        // An explicit selector still wins over the network default.
        f.config_profile.write(crate::config::PROFILE_PROD).unwrap();
        assert_eq!(
            crate::config::read(&s).unwrap(),
            crate::config::IntexParams::PROD
        );
        assert_eq!(
            crate::config::IntexParams::PROD.commit_bond_minor,
            100_000_000u128 * 1_000_000_000_000_000_000u128
        );
        assert_eq!(
            crate::config::IntexParams::DEV.commit_bond_minor,
            100u128 * 1_000_000_000_000_000_000u128
        );
    });
}

#[test]
fn config_dev_profile_drives_issuance_and_qualification() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        // Select the dev profile through the single selector byte.
        f.config_profile.write(crate::config::PROFILE_DEV).unwrap();
        assert_eq!(
            crate::config::read(&s).unwrap(),
            crate::config::IntexParams::DEV
        );

        runtime::issue(&s, sample(7)).unwrap();

        // Issuance captures the dev call-trigger and dev-derived prices.
        let dev = crate::config::IntexParams::DEV;
        let r = outbe_intex::api::read_series(&s, sid(7)).unwrap();
        assert_eq!(r.call_notice_period_seconds, dev.call_notice_period_seconds);
        assert_eq!(
            r.floor_price_minor,
            U256::from(ENTRY_PRICE * u64::from(100 + dev.floor_rate) / 100)
        );
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
            }
        );

        // Qualification is the floor comparison alone: a day closing one unit past
        // the dev-derived floor qualifies the series.
        write_day_vwap(
            &OracleContract::new(s.clone()),
            REFERENCE_ISO,
            PAIR_ID,
            ISSUED_AT as u64 + 2 * DAY,
            r.floor_price_minor + U256::from(1),
        );
        assert!(runtime::is_series_qualified(&s, sid(7)).unwrap());
    });
}

#[test]
fn config_unknown_selector_errors() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        f.config_profile.write(99u8).unwrap();
        assert!(crate::config::read(&s).is_err());
    });
}

/// The network default: only mainnet runs the real timings.
#[test]
fn config_auto_profile_follows_the_network() {
    use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};

    assert_eq!(
        crate::config::IntexParams::for_chain_id(MAINNET_CHAIN_ID),
        crate::config::IntexParams::PROD
    );
    for chain_id in [TESTNET_CHAIN_ID, DEVNET_CHAIN_ID, 31_337] {
        assert_eq!(
            crate::config::IntexParams::for_chain_id(chain_id),
            crate::config::IntexParams::DEV
        );
    }
}

/// Pin the selector slot index: the seeder writes a raw slot, so the schema must
/// map `config_profile` to the same one.
#[test]
fn config_profile_slot_matches_seeder_layout() {
    with_factory(|s| {
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(f.config_profile.slot(), U256::from(5));
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
            f.call_bin_tree_root.base_slot(),
            f.call_bin_tree_mid.base_slot(),
            f.call_bin_tree_leaf.base_slot(),
            f.call_bin_count.base_slot(),
            f.config_profile.slot(),
            f.call_currency_cursor.slot(),
            f.call_scan_cursor.base_slot(),
            f.call_group_count.base_slot(),
            f.call_group_members.base_slot(),
            f.call_group_bin.base_slot(),
            f.call_sweep_day.slot(),
            f.call_bin_group_days.base_slot(),
            f.notify_head.slot(),
            f.notify_tail.slot(),
            f.notify_at.base_slot(),
            f.expiry_tree_root.slot(),
            f.expiry_tree_mid.base_slot(),
            f.expiry_tree_leaf.base_slot(),
            f.called_group_deadline.base_slot(),
            f.called_group_count.base_slot(),
            f.called_group_members.base_slot(),
            f.max_call_window_seconds.base_slot(),
            f.min_call_threshold_seconds.base_slot(),
            f.expiry_bucket_len.base_slot(),
            f.expiry_bucket_live.base_slot(),
            f.expiry_bucket_at.base_slot(),
            f.called_group_slot.base_slot(),
            f.expiry_sweep_day.slot(),
            f.expiry_cursor.slot(),
            f.call_pending_day.slot(),
            f.parked_message_cursor.slot(),
            f.parked_proceeds_cursor.slot(),
            f.vwap_sent_day.slot(),
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
        crate::parked::drain(&ctx).expect("a silent router is not a failed block");

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
        crate::parked::drain(&ctx)
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

fn drain_notices_in_trigger(
    provider: &mut super::router_fault::RouterFaultProvider,
) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, ISSUED_AT as u64, CHAIN_ID),
            s.clone(),
        );
        // The cycle runtime runs each trigger inside its own checkpoint.
        s.with_checkpoint(|| crate::notify::drain_notices(&ctx))
    })
}

#[test]
fn a_node_local_failure_inside_the_called_notice_send_fails_the_drain() {
    use super::router_fault::RouterFaultProvider;
    use crate::notify::{enqueue_notice, pack_called_notice};
    use crate::sol_ext::IOriginRouter;

    let mut provider = RouterFaultProvider::new(
        factory_provider(),
        IOriginRouter::sendMarkCalledCall::SELECTOR,
    );
    let queued = [
        pack_called_notice(sid(7), ISSUED_AT),
        pack_called_notice(sid(8), ISSUED_AT),
    ];
    StorageHandle::enter(&mut provider, |s| {
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

    let result = drain_notices_in_trigger(&mut provider);
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::SubCall(
            outbe_primitives::storage::SubCallError::DatabaseError(_)
        ))
    ));
    assert_eq!(queue(&mut provider), (0, 2, queued));

    provider.heal();
    drain_notices_in_trigger(&mut provider).unwrap();
    assert_eq!(queue(&mut provider), (0, 0, [U256::ZERO; 2]));
}
