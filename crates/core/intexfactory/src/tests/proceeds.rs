use super::*;

use alloy_sol_types::SolEvent;
use outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
use outbe_promislimit::PromisLimitContract;

use crate::constants::ORIGIN_ROUTER_ADDRESS;

#[test]
fn distribute_rejects_non_origin_router() {
    with_factory(|s| {
        s.increase_balance(INTEX_FACTORY_ADDRESS, U256::from(100u64))
            .unwrap();
        let err = runtime::distribute(&s, owner(), 7.into(), 10, U256::from(100u64)).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                crate::errors::IntexFactoryError::NotOriginRouter,
            )
            .to_string()
        );
    });
}

fn deliver(s: &StorageHandle<'_>, src_chain_id: u32, amount: U256) {
    s.increase_balance(INTEX_FACTORY_ADDRESS, amount).unwrap();
    runtime::distribute(s, ORIGIN_ROUTER_ADDRESS, 7.into(), src_chain_id, amount).unwrap();
}

fn pool(s: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(s.clone()).promis_limit().unwrap()
}

fn burned_events(storage: &HashMapStorageProvider) -> Vec<IIntexFactory::ProceedsBurned> {
    storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&IIntexFactory::ProceedsBurned::SIGNATURE_HASH))
        .map(|log| IIntexFactory::ProceedsBurned::decode_log_data(log).unwrap())
        .collect()
}

#[test]
fn proceeds_burn_into_the_promis_limit_pool() {
    let mut storage = factory_provider();
    let amount = U256::from(3u64) * UNITS_PER_PROTOCOL_MINOR;

    StorageHandle::enter(&mut storage, |s| {
        let before = pool(&s);
        deliver(&s, 10, amount);

        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(pool(&s), before + U256::from(3u64));
    });

    assert_eq!(
        burned_events(&storage),
        vec![IIntexFactory::ProceedsBurned {
            emissionDay: 7,
            srcChainId: 10,
            proceedsMinor: amount,
            creditedPromisMinor: U256::from(3u64),
        }]
    );
}

#[test]
fn coen_below_one_protocol_minor_carries_into_the_next_delivery() {
    let mut storage = factory_provider();
    let unit = UNITS_PER_PROTOCOL_MINOR;
    let rest = U256::from(7u64);

    StorageHandle::enter(&mut storage, |s| {
        let before = pool(&s);
        deliver(&s, 10, U256::from(2u64) * unit + rest);
        assert_eq!(pool(&s), before + U256::from(2u64));
        assert_eq!(
            PromisLimitContract::new(s.clone()).rest_minor().unwrap(),
            rest
        );

        deliver(&s, 10, unit - rest);
        assert_eq!(pool(&s), before + U256::from(3u64));
        assert_eq!(
            PromisLimitContract::new(s.clone()).rest_minor().unwrap(),
            U256::ZERO
        );
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
    });

    let credited: Vec<U256> = burned_events(&storage)
        .iter()
        .map(|e| e.creditedPromisMinor)
        .collect();
    assert_eq!(credited, vec![U256::from(2u64), U256::from(1u64)]);
}

#[test]
fn proceeds_burn_under_the_proceeds_source() {
    use outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS;
    use outbe_promislimit::{precompile::IPromisLimit, BurnSource};

    let mut storage = factory_provider();
    let amount = UNITS_PER_PROTOCOL_MINOR;
    StorageHandle::enter(&mut storage, |s| deliver(&s, 10, amount));

    let burns: Vec<IPromisLimit::NativeBurnedToPromisLimit> = storage
        .get_events(PROMIS_LIMIT_ADDRESS)
        .iter()
        .filter_map(|log| {
            IPromisLimit::NativeBurnedToPromisLimit::decode_raw_log(log.topics(), &log.data).ok()
        })
        .collect();
    assert_eq!(burns.len(), 1);
    assert_eq!(burns[0].source, BurnSource::Proceeds as u8);
    assert_eq!(burns[0].burnedUnit, amount);
}

#[test]
fn proceeds_above_the_factory_balance_change_nothing() {
    let mut storage = factory_provider();
    let unit = UNITS_PER_PROTOCOL_MINOR;

    StorageHandle::enter(&mut storage, |s| {
        s.increase_balance(INTEX_FACTORY_ADDRESS, unit).unwrap();
        let before = pool(&s);
        runtime::distribute(&s, ORIGIN_ROUTER_ADDRESS, 7.into(), 10, unit + unit).unwrap_err();

        assert_eq!(pool(&s), before);
        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), unit);
        assert_eq!(
            PromisLimitContract::new(s.clone()).rest_minor().unwrap(),
            U256::ZERO
        );
    });
    assert!(burned_events(&storage).is_empty());
}

#[test]
fn every_delivery_of_a_day_burns_and_credits() {
    let mut storage = factory_provider();
    let unit = UNITS_PER_PROTOCOL_MINOR;

    StorageHandle::enter(&mut storage, |s| {
        let before = pool(&s);
        deliver(&s, 10, unit);
        deliver(&s, 20, U256::from(4u64) * unit);
        deliver(&s, 10, unit);

        assert_eq!(s.balance(INTEX_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(pool(&s), before + U256::from(6u64));
    });

    let chains: Vec<u32> = burned_events(&storage)
        .iter()
        .map(|e| e.srcChainId)
        .collect();
    assert_eq!(chains, vec![10, 20, 10]);
}

/// IntexFactory is a payable route, so the boundary credits value to its
/// address. Its dispatch must refuse value for every selector outside
/// `PAYABLE_SELECTORS`, or a funded call to a non-payable selector would strand
/// native value at an address with no accounting entry for it.
///
/// Characterization: the test pins current behavior. Its value is catching a
/// future removal of the guard.
#[test]
fn unpublished_selectors_refuse_native_value() {
    use crate::precompile::{dispatch, IIntexFactory};

    let calls = [IIntexFactory::settleIntexCall {
        intexId: U256::ZERO,
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

/// `distribute` is the one published payable selector here, and it burns
/// auction proceeds straight from `msg.value`. The route table only decides that
/// this *address* may be credited. Nothing in it proves the dispatch actually
/// hands the value to the handler. This does: the handler rejects a zero amount,
/// so the two outcomes separate exactly on whether the value arrived.
#[test]
fn distribute_receives_the_call_value() {
    use alloy_sol_types::SolCall;

    use crate::precompile::{dispatch, IIntexFactory};

    let data = IIntexFactory::distributeCall {
        emissionDay: 7u32,
        srcChainId: 10u32,
    }
    .abi_encode();

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let zero_amount_err = outbe_primitives::error::PrecompileError::from(
            crate::errors::IntexFactoryError::ZeroAmount,
        )
        .to_string();
        let unfunded = dispatch(storage.clone(), &data, ORIGIN_ROUTER_ADDRESS, U256::ZERO);
        assert_eq!(
            unfunded.as_ref().map_err(ToString::to_string),
            Err(zero_amount_err.clone()),
            "a zero-value distribute must reach the handler with nothing, got {unfunded:?}"
        );

        let funded = dispatch(
            storage.clone(),
            &data,
            ORIGIN_ROUTER_ADDRESS,
            U256::from(1_000u64),
        );
        assert_ne!(
            funded.as_ref().map_err(ToString::to_string),
            Err(zero_amount_err),
            "a funded distribute must reach the handler with the value, got {funded:?}"
        );
    });
}
