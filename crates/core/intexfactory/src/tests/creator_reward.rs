use super::*;

/// A future fan-in deadline relative to the harness clock (`ISSUED_AT`).
const DEADLINE_FUTURE: u64 = ISSUED_AT as u64 + 1000;

#[test]
fn distribute_rejects_non_origin_router() {
    with_factory(|s| {
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        let err = runtime::distribute(&s, owner(), 7.into(), 10, U256::from(100u64)).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("origin router"));
    });
}

#[test]
fn ready_proceeds_pot_burns() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;

    let mut storage = factory_provider();
    let day = WorldwideDay::new(7);
    let amount = U256::from(100u64);

    StorageHandle::enter(&mut storage, |s| {
        // One winning chain completes the fan-in, so the pot burns now.
        outbe_intex::api::arm_proceeds(&s, day, &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
        runtime::distribute(&s, crate::constants::ORIGIN_ROUTER_ADDRESS, day, 10, amount).unwrap();

        assert_eq!(s.balance(VAULT_ROUTER_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::proceeds_deadline(&s, day).unwrap(),
            u64::MAX
        );
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 0);
    });

    let sig = IIntexFactory::ProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::ProceedsBurned::decode_log_data(log)
                .map(|ev| ev.worldwideDay == 7 && ev.amount == amount)
                .unwrap_or(false)
    });
    assert!(found, "expected ProceedsBurned event");
}

/// IntexFactory is a payable route, so the boundary credits value to its
/// address. Its dispatch must refuse value for every selector outside
/// `PAYABLE_SELECTORS`, or a funded call to a non-payable selector would strand
/// native value at an address with no accounting entry for it.
///
/// Characterization: the per-arm checks this replaced already covered these two
/// selectors with the same message, so the test pins current behavior rather
/// than proving a fix. Its value is catching a future removal of the guard.
#[test]
fn unpublished_selectors_refuse_native_value() {
    use crate::precompile::{dispatch, IIntexFactory};

    let calls = [IIntexFactory::settleIntexCall {
        seriesId: Default::default(),
        owner: Address::ZERO,
        units: U256::ZERO,
        asset: Address::ZERO,
        snapshotId: U256::ZERO,
    }
    .abi_encode()];

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for data in &calls {
            let funded = dispatch(storage.clone(), data, Address::ZERO, U256::from(1u64));
            assert!(
                matches!(
                    funded,
                    Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "unpublished selector must refuse value, got {funded:?}"
            );
        }
    });
}

/// `distribute` is the one published payable selector here, and it credits
/// auction proceeds straight from `msg.value`. The route table only decides that
/// this *address* may be credited. Nothing in it proves the dispatch actually
/// hands the value to the handler. This does: the handler rejects a zero amount,
/// so the two outcomes separate exactly on whether the value arrived.
#[test]
fn distribute_receives_the_call_value() {
    use alloy_sol_types::SolCall;

    use crate::constants::ORIGIN_ROUTER_ADDRESS;
    use crate::precompile::{dispatch, IIntexFactory};

    let data = IIntexFactory::distributeCall {
        worldwideDay: 7u32,
        srcChainId: 10u32,
    }
    .abi_encode();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let unfunded = dispatch(storage.clone(), &data, ORIGIN_ROUTER_ADDRESS, U256::ZERO);
        assert!(
            matches!(
                unfunded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a zero-value distribute must reach the handler with nothing, got {unfunded:?}"
        );

        let funded = dispatch(
            storage.clone(),
            &data,
            ORIGIN_ROUTER_ADDRESS,
            U256::from(1_000u64),
        );
        assert!(
            !matches!(
                funded,
                Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                    if message == "amount must be positive"
            ),
            "a funded distribute must reach the handler with the value, got {funded:?}"
        );
    });
}

#[test]
fn late_proceeds_after_finalize_burn() {
    use alloy_sol_types::SolEvent;

    let mut storage = factory_provider();
    let day = WorldwideDay::new(7);
    let amount = U256::from(100u64);
    let late = U256::from(40u64);

    StorageHandle::enter(&mut storage, |s| {
        outbe_intex::api::arm_proceeds(&s, day, &[10], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
        runtime::distribute(&s, crate::constants::ORIGIN_ROUTER_ADDRESS, day, 10, amount).unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);

        s.increase_balance(INTEX_FACTORY_ADDRESS, late).unwrap();
        runtime::distribute(&s, crate::constants::ORIGIN_ROUTER_ADDRESS, day, 11, late).unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::proceeds_deadline(&s, day).unwrap(),
            u64::MAX
        );
    });

    let sig = IIntexFactory::LateProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::LateProceedsBurned::decode_log_data(log)
                .map(|ev| ev.worldwideDay == 7 && ev.amount == late)
                .unwrap_or(false)
    });
    assert!(found, "expected LateProceedsBurned event");
}

#[test]
fn incomplete_fan_in_holds_the_pot_until_the_deadline() {
    use alloy_sol_types::SolEvent;

    let mut storage = factory_provider();
    let day = WorldwideDay::new(8);
    let arrived = U256::from(200u64);

    StorageHandle::enter(&mut storage, |s| {
        outbe_intex::api::arm_proceeds(&s, day, &[10, 11], DEADLINE_FUTURE).unwrap();
        s.increase_balance(INTEX_FACTORY_ADDRESS, arrived).unwrap();
        runtime::distribute(
            &s,
            crate::constants::ORIGIN_ROUTER_ADDRESS,
            day,
            10,
            arrived,
        )
        .unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), arrived);
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 1);

        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE - 1).unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), arrived);

        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE).unwrap();
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(
            outbe_intex::api::proceeds_deadline(&s, day).unwrap(),
            u64::MAX
        );
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 0);
    });

    let sig = IIntexFactory::ProceedsBurned::SIGNATURE_HASH;
    let found = storage.get_events(INTEX_FACTORY_ADDRESS).iter().any(|log| {
        log.topics().first() == Some(&sig)
            && IIntexFactory::ProceedsBurned::decode_log_data(log)
                .map(|ev| ev.worldwideDay == 8 && ev.amount == arrived)
                .unwrap_or(false)
    });
    assert!(found, "expected ProceedsBurned event");
}

#[test]
fn a_day_no_chain_ever_paid_into_leaves_the_awaiting_set() {
    with_factory(|s| {
        let wwd = WorldwideDay::new(2026_0301);
        // Two winning chains and not one delivery: the fan-in can never complete.
        outbe_intex::api::arm_proceeds(&s, wwd, &[10, 20], DEADLINE_FUTURE).unwrap();
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 1);

        // Before the deadline the day is still owed its proceeds.
        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE - 1).unwrap();
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 1);

        // Past it there is nothing left to wait for, so the day stops being swept.
        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE + 1).unwrap();
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 0);
        assert_eq!(
            outbe_intex::api::proceeds_deadline(&s, wwd).unwrap(),
            u64::MAX
        );
    });
}

#[test]
fn a_node_local_failure_while_settling_proceeds_fails_the_sweep() {
    let mut provider = factory_provider();
    let wwd = WorldwideDay::new(2026_0301);
    StorageHandle::enter(&mut provider, |s| {
        outbe_intex::api::arm_proceeds(&s, wwd, &[10, 20], DEADLINE_FUTURE).unwrap();
    });
    provider.fail_after_mutation_at(0);
    let result = StorageHandle::enter(&mut provider, |s| {
        runtime::sweep_proceeds_deadlines(&s, DEADLINE_FUTURE + 1)
    });
    provider.clear_mutation_failure();
    assert!(matches!(
        result,
        Err(outbe_primitives::error::PrecompileError::Storage(_))
    ));
    StorageHandle::enter(&mut provider, |s| {
        assert_eq!(outbe_intex::api::awaiting_proceeds_count(&s).unwrap(), 1);
        assert_eq!(outbe_intex::api::awaiting_proceeds_at(&s, 0).unwrap(), wwd);
    });
}
