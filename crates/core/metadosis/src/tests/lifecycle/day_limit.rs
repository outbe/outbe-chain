use super::*;

#[test]
fn test_emission_sink_writes_metadosis_limit_for_worldwide_day() {
    with_storage(|storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        let timestamp = outbe_primitives::time::WorldwideDay::new(20241221).start_timestamp()
            + 2 * SECONDS_PER_HOUR;
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, CHAIN_ID),
            storage.clone(),
        );

        // The terminal sink now writes the limit onto the WorldwideDay record
        // (UTC+14 keyed) for the block timestamp, not a separate UTC-date-key map.
        let wwd = outbe_primitives::time::WorldwideDay::from_timestamp(timestamp);

        let day_limit = U256::from(500_000_000u64);
        crate::emission_sink::apply(&ctx, day_limit).unwrap();

        let metadosis = MetadosisContract::new(storage);
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd)
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            day_limit
        );
        // A neighboring day is untouched.
        assert_eq!(
            metadosis
                .worldwide_days
                .entry(wwd.previous_date_key())
                .metadosis_limit_amount()
                .read()
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn ocomp_day_limit_formation_leaves_the_accumulator_untouched() {
    fn apply_limit(
        provider: &mut HashMapStorageProvider,
        block_number: u64,
        wwd: outbe_primitives::time::WorldwideDay,
        amount: U256,
    ) -> outbe_primitives::error::Result<U256> {
        provider.enable_metadosis_mutation_frame(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        );
        provider.set_block_number(block_number);
        StorageHandle::enter(provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(
                    block_number,
                    wwd.start_timestamp() + 2 * SECONDS_PER_HOUR,
                    CHAIN_ID,
                ),
                storage,
            );
            crate::commands::apply_cycle_day_limit(&ctx, amount)
        })
    }

    let first = outbe_primitives::time::WorldwideDay::new(20260725);
    let second = outbe_primitives::time::WorldwideDay::new(20260726);
    let third = outbe_primitives::time::WorldwideDay::new(20260727);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        PromisLimitContract::new(storage)
            .checked_add_carry_over(U256::from(30))
            .unwrap();
    });
    apply_limit(&mut provider, 10, first, U256::from(100)).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        let metadosis = MetadosisContract::new(storage.clone());
        let first_day = metadosis.worldwide_days.entry(first);
        let first_formation = metadosis.ocomp_day_limit_formation(first).unwrap().unwrap();
        assert_eq!(first_formation.worldwide_day, first);
        assert_eq!(first_formation.base_limit, U256::from(100));
        assert_eq!(first_formation.carry_over_before, U256::from(30));
        assert_eq!(first_formation.carry_over_taken, U256::ZERO);
        assert_eq!(first_formation.carry_over_after, U256::from(30));
        assert_eq!(first_formation.day_limit, U256::from(100));
        assert_eq!(first_formation.block_number, 10);
        assert_eq!(
            crate::api::day_limit_formation_receipt(storage.clone(), first).unwrap(),
            Some(crate::DayLimitFormationReceipt::Formed(first_formation))
        );
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(100)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(30));

        PromisLimitContract::new(storage)
            .checked_add_carry_over(U256::from(7))
            .unwrap();
    });
    apply_limit(&mut provider, 10, first, U256::from(100)).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        let first_day = MetadosisContract::new(storage.clone())
            .worldwide_days
            .entry(first);
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(100)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(37));
        assert!(MetadosisContract::new(storage.clone())
            .set_metadosis_limit(first, U256::from(999))
            .is_err());
        assert_eq!(
            first_day.metadosis_limit_amount().read().unwrap(),
            U256::from(100)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(37));
    });
    assert!(apply_limit(&mut provider, 10, first, U256::from(101)).is_err());

    apply_limit(&mut provider, 20, second, U256::from(200)).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let promis = PromisLimitContract::new(storage.clone());
        let metadosis = MetadosisContract::new(storage);
        let second_day = metadosis.worldwide_days.entry(second);
        let second_formation = metadosis
            .ocomp_day_limit_formation(second)
            .unwrap()
            .unwrap();
        assert_eq!(second_formation.carry_over_taken, U256::ZERO);
        assert_eq!(second_formation.carry_over_before, U256::from(37));
        assert_eq!(second_formation.carry_over_after, U256::from(37));
        assert_eq!(second_formation.block_number, 20);
        assert_eq!(
            second_day.metadosis_limit_amount().read().unwrap(),
            U256::from(200)
        );
        assert_eq!(promis.get_total_unallocated().unwrap(), U256::from(37));
    });

    apply_limit(&mut provider, 30, third, U256::from(50)).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        let third_formation = metadosis.ocomp_day_limit_formation(third).unwrap().unwrap();
        assert_eq!(third_formation.base_limit, U256::from(50));
        assert_eq!(third_formation.carry_over_taken, U256::ZERO);
        assert_eq!(third_formation.day_limit, U256::from(50));

        MetadosisContract::new(storage.clone())
            .delete_worldwide_day(third)
            .unwrap();
        assert!(MetadosisContract::new(storage)
            .ocomp_day_limit_formation(third)
            .unwrap()
            .is_none());
    })
}

#[test]
fn ocomp_day_limit_rejection_and_every_mutation_failure_are_atomic() {
    fn seed(provider: &mut HashMapStorageProvider, carry_over: U256) {
        StorageHandle::enter(provider, |storage| {
            arm_genesis_ocomp(&storage, CHAIN_ID);
            PromisLimitContract::new(storage)
                .checked_add_carry_over(carry_over)
                .unwrap();
        });
    }

    fn apply_limit(
        provider: &mut HashMapStorageProvider,
        base_limit: U256,
    ) -> outbe_primitives::error::Result<U256> {
        provider.enable_metadosis_mutation_frame(
            outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        );
        let wwd = outbe_primitives::time::WorldwideDay::new(20260727);
        StorageHandle::enter(provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(
                    30,
                    wwd.start_timestamp() + 2 * SECONDS_PER_HOUR,
                    CHAIN_ID,
                ),
                storage,
            );
            crate::commands::apply_cycle_day_limit(&ctx, base_limit)
        })
    }

    // A formed day limit cannot be replaced, and the refusal leaves no trace.
    let mut rejected = HashMapStorageProvider::new(CHAIN_ID);
    outbe_fidelity::enclave_client::test_enclave::install();
    seed(&mut rejected, U256::from(1));
    apply_limit(&mut rejected, U256::from(100)).unwrap();
    let before_storage = rejected.storage.clone();
    let before_events = rejected.events.clone();
    let before_ordered = rejected.get_ordered_events().to_vec();
    assert!(apply_limit(&mut rejected, U256::from(101)).is_err());
    assert_eq!(rejected.storage, before_storage);
    assert_eq!(rejected.events, before_events);
    assert_eq!(rejected.get_ordered_events(), before_ordered.as_slice());
    StorageHandle::enter(&mut rejected, |storage| {
        assert_eq!(
            PromisLimitContract::new(storage.clone())
                .get_total_unallocated()
                .unwrap(),
            U256::from(1)
        );
        let formation = MetadosisContract::new(storage)
            .ocomp_day_limit_formation(outbe_primitives::time::WorldwideDay::new(20260727))
            .unwrap()
            .unwrap();
        assert_eq!(formation.base_limit, U256::from(100));
        assert_eq!(formation.day_limit, U256::from(100));
    });

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    outbe_fidelity::enclave_client::test_enclave::install();
    seed(&mut probe, U256::from(9));
    probe.fail_after_mutation_at(usize::MAX);
    apply_limit(&mut probe, U256::from(100)).unwrap();
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 2,
        "formation must mutate Metadosis and events"
    );
    let event = IMetadosis::OcompDayLimitFormed::decode_log(
        probe.get_ordered_events().last().expect("formation event"),
    )
    .unwrap();
    assert_eq!(event.data.worldwideDay, 20260727);
    assert_eq!(event.data.baseLimit, U256::from(100));
    assert_eq!(event.data.carryOverBefore, U256::from(9));
    assert_eq!(event.data.carryOverTaken, U256::ZERO);
    assert_eq!(event.data.carryOverAfter, U256::from(9));
    assert_eq!(event.data.formedDayLimit, U256::from(100));
    let replay_event_count = probe.get_ordered_events().len();
    apply_limit(&mut probe, U256::from(100)).unwrap();
    assert_eq!(probe.get_ordered_events().len(), replay_event_count);
    let clean_storage = probe.storage.clone();
    let clean_events = probe.events.clone();
    let clean_ordered = probe.get_ordered_events().to_vec();

    for operation in 0..mutation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        outbe_fidelity::enclave_client::test_enclave::install();
        seed(&mut provider, U256::from(9));
        let before_storage = provider.storage.clone();
        let before_events = provider.events.clone();
        let before_ordered = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(operation);

        assert!(apply_limit(&mut provider, U256::from(100)).is_err());
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, before_storage);
        assert_eq!(provider.events, before_events);
        assert_eq!(provider.get_ordered_events(), before_ordered.as_slice());

        apply_limit(&mut provider, U256::from(100)).unwrap();
        assert_eq!(
            provider.storage, clean_storage,
            "retry storage at {operation}"
        );
        assert_eq!(provider.events, clean_events, "retry events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            clean_ordered.as_slice(),
            "retry ordered events at {operation}"
        );
        StorageHandle::enter(&mut provider, |storage| {
            let formed = MetadosisContract::new(storage.clone())
                .ocomp_day_limit_formation(outbe_primitives::time::WorldwideDay::new(20260727))
                .unwrap()
                .unwrap();
            assert_eq!(formed.base_limit, U256::from(100));
            assert_eq!(formed.carry_over_taken, U256::ZERO);
            assert_eq!(formed.day_limit, U256::from(100));
            assert_eq!(
                PromisLimitContract::new(storage)
                    .get_total_unallocated()
                    .unwrap(),
                U256::from(9)
            );
        });
    }
}
