use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::{SolError, SolEvent};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{
    metadosis_process_ready_binding, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
    StorageHandle,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::TributeContract;

use super::CHAIN_ID;
use crate::schema::TributeFactoryContract;

#[test]
fn su_hash_can_only_be_used_once() {
    use crate::schema::SU_HASH_RETAIN;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let events_before = storage.events.clone();
    let ordered_before = storage.get_ordered_events().to_vec();
    StorageHandle::enter(&mut storage, |storage| {
        let expect_reused = |err: PrecompileError, _expected_hash: B256| {
            assert_eq!(
                err.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::SuHashAlreadyUsed)
                    .to_string()
            );
        };

        // Distinct hashes across the array all succeed and populate the retention ring.
        let (a, b, c) = (
            B256::repeat_byte(0xAA),
            B256::repeat_byte(0xBB),
            B256::repeat_byte(0xDD),
        );
        let mut factory = TributeFactoryContract::new(storage.clone());
        factory
            .mark_su_hashes_used(&[a, b])
            .expect("distinct hashes ok");
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), 2);
        assert_eq!(factory.su_hash_ring.read(&0).unwrap(), a);
        assert_eq!(factory.su_hash_ring.read(&1).unwrap(), b);

        // The factory rejects reuse in a later call inside the retention window.
        expect_reused(factory.mark_su_hashes_used(&[a]).unwrap_err(), a);

        // The factory rejects a duplicate within a single array.
        let dup = B256::repeat_byte(0xCC);
        expect_reused(
            storage
                .with_checkpoint(|| {
                    TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[dup, dup])
                })
                .unwrap_err(),
            dup,
        );

        // Near the wrap point (seq = SU_HASH_RETAIN), a multi-hash batch `[c, a]`
        // rejects `a` even though `c` would evict ring slot 0 (`a`) during phase 2.
        factory.su_hash_ring_seq.write(SU_HASH_RETAIN).unwrap();
        expect_reused(
            storage
                .with_checkpoint(|| {
                    TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[c, a])
                })
                .unwrap_err(),
            a,
        );
        assert!(factory.has_used_su_hash(a).unwrap());
        assert!(!factory.has_used_su_hash(c).unwrap());

        // Wrapping at `seq = SU_HASH_RETAIN` (the (SU_HASH_RETAIN + 1)-th entry)
        // evicts the first hash (`a` at index 0) while keeping `b` and `c` live.
        factory
            .mark_su_hashes_used(&[c])
            .expect("wrap evicts oldest hash");
        assert!(!factory.has_used_su_hash(a).unwrap());
        assert!(factory.has_used_su_hash(b).unwrap());
        assert!(factory.has_used_su_hash(c).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);
        assert_eq!(factory.su_hash_ring.read(&0).unwrap(), c);

        // Self-eviction is skipped when the ring entry at `seq % SU_HASH_RETAIN`
        // already matches the incoming hash.
        let self_hash = B256::repeat_byte(0xEE);
        factory
            .su_hash_prune_ring()
            .seed_for_test_with_retain::<SU_HASH_RETAIN>(SU_HASH_RETAIN, self_hash)
            .unwrap();
        factory.mark_su_hashes_used(&[self_hash]).unwrap();
        assert!(factory.has_used_su_hash(self_hash).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);

        // Cursor overflow at u64::MAX reverts with SuHashRingSeqOverflow and rolls back.
        factory.su_hash_ring_seq.write(u64::MAX).unwrap();
        let ring_tail_idx = u64::MAX % SU_HASH_RETAIN;
        let ring_tail_before = factory.su_hash_ring.read(&ring_tail_idx).unwrap();
        let overflow = storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[a])
            })
            .unwrap_err();
        let expected_overflow =
            crate::precompile::ITributeFactory::SuHashRingSeqOverflow {}.abi_encode();
        assert!(
            matches!(&overflow, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected_overflow),
            "unexpected overflow error: {overflow:?}"
        );
        assert!(!factory.has_used_su_hash(a).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), u64::MAX);
        assert_eq!(
            factory.su_hash_ring.read(&ring_tail_idx).unwrap(),
            ring_tail_before
        );
    });
    assert_eq!(storage.events, events_before);
    assert_eq!(storage.get_ordered_events(), ordered_before);
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let factory = TributeFactoryContract::new(storage.clone());
        assert_eq!(
            factory.used_su_hashes.base_slot(),
            alloy_primitives::U256::ZERO
        );
        assert_eq!(
            factory.worldwide_days.base_slot(),
            alloy_primitives::U256::from(1)
        );
        // `pipeline_wwd` is a two-slot set. Its base is private on `StorageSet`,
        // so the occupation of slots 2..3 is the gap before slot 4.
        let pipeline_base = factory.worldwide_days.base_slot() + alloy_primitives::U256::from(1);
        let pipeline_positions = pipeline_base + alloy_primitives::U256::from(1);
        assert_eq!(pipeline_base, alloy_primitives::U256::from(2));
        assert_eq!(pipeline_positions, alloy_primitives::U256::from(3));
        assert_eq!(
            factory.forming_snapshot_finalized.base_slot(),
            alloy_primitives::U256::from(4)
        );
        assert_eq!(
            factory.su_hash_ring.base_slot(),
            alloy_primitives::U256::from(5)
        );
        assert_eq!(
            factory.su_hash_ring_seq.slot(),
            alloy_primitives::U256::from(6)
        );
        assert_eq!(
            factory.last_wwd_advance_day.slot(),
            alloy_primitives::U256::from(7)
        );
    });
}

#[test]
fn currency_price_unavailable_encodes_selector_and_pipeline_cap_and_historical_close() {
    let wwd = WorldwideDay::new(20261001);
    let err: PrecompileError =
        crate::errors::TributeFactoryError::CurrencyPriceUnavailable { worldwide_day: wwd }.into();
    let expected_bytes: Bytes = crate::precompile::ITributeFactory::CurrencyPriceUnavailable {
        worldwideDay: wwd.value(),
    }
    .abi_encode()
    .into();
    assert!(matches!(err, PrecompileError::RevertBytes(bytes) if bytes == expected_bytes));

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 30);
    StorageHandle::enter(&mut provider, |storage| {
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                let mut factory = TributeFactoryContract::new(storage.clone());
                let mut day_key = 20261001u32;
                for _ in 0..crate::worldwideday::constants::MAX_PIPELINE_WWDS {
                    let cur_wwd = WorldwideDay::new(day_key);
                    let (schedule, _) = crate::worldwideday::compute_wwd_schedule(cur_wwd)?;
                    assert!(crate::worldwideday::create_worldwide_day(
                        &mut factory,
                        cur_wwd,
                        schedule
                    )?);
                    assert_eq!(
                        TributeContract::new(storage.clone())
                            .day_emission_day
                            .read(&cur_wwd)?,
                        outbe_primitives::time::EmissionDate::new(schedule.day)
                    );
                    day_key = outbe_primitives::time::next_date_key(day_key).unwrap();
                }
                let overflow_wwd = WorldwideDay::new(day_key);
                let (overflow_schedule, _) =
                    crate::worldwideday::compute_wwd_schedule(overflow_wwd)?;
                let cap_err = crate::worldwideday::create_worldwide_day(
                    &mut factory,
                    overflow_wwd,
                    overflow_schedule,
                )
                .unwrap_err();
                assert!(cap_err.to_string().contains("7"));
                Ok(())
            })
            .unwrap();

        // The pipeline is full. Close one day, then create and close another.
        // Create leaves the day unsealed in `pipeline_wwd`. `close_worldwide_day`
        // is the single seal.
        let filled = WorldwideDay::new(20261001);
        let hist_wwd = WorldwideDay::new(20260901);
        let (hist_schedule, _) = crate::worldwideday::compute_wwd_schedule(hist_wwd).unwrap();
        storage
            .set_block_timestamp(U256::from(hist_schedule.offering_end))
            .unwrap();
        let binding2 = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding2, || {
                let mut factory = TributeFactoryContract::new(storage.clone());
                crate::worldwideday::close_worldwide_day(&mut factory, filled)?;
                assert!(!factory.pipeline_wwd.read_all()?.contains(&filled));
                assert!(TributeContract::new(storage.clone()).is_day_sealed(filled)?);

                assert!(crate::worldwideday::create_worldwide_day(
                    &mut factory,
                    hist_wwd,
                    hist_schedule
                )?);
                assert!(factory.pipeline_wwd.read_all()?.contains(&hist_wwd));
                assert!(!TributeContract::new(storage.clone()).is_day_sealed(hist_wwd)?);
                assert_eq!(
                    TributeContract::new(storage.clone())
                        .day_emission_day
                        .read(&hist_wwd)?,
                    outbe_primitives::time::EmissionDate::new(hist_schedule.day)
                );
                crate::worldwideday::close_worldwide_day(&mut factory, hist_wwd)?;
                assert!(!factory.pipeline_wwd.read_all()?.contains(&hist_wwd));
                assert!(TributeContract::new(storage.clone()).is_day_sealed(hist_wwd)?);
                Ok(())
            })
            .unwrap();
    });
    let factory_events = provider.get_events(outbe_primitives::addresses::TRIBUTE_FACTORY_ADDRESS);
    assert_eq!(
        factory_events.len(),
        crate::worldwideday::constants::MAX_PIPELINE_WWDS + 1
    );
    let last_opened = crate::precompile::ITributeFactory::WorldwideDayOpened::decode_log_data(
        factory_events.last().unwrap(),
    )
    .unwrap();
    let (hist_schedule, _) =
        crate::worldwideday::compute_wwd_schedule(WorldwideDay::new(20260901)).unwrap();
    assert_eq!(last_opened.worldwideDay, 20260901);
    assert_eq!(last_opened.formingStart, hist_schedule.forming_start);
    assert_eq!(last_opened.formingEnd, hist_schedule.forming_end);
    assert_eq!(last_opened.offeringStart, hist_schedule.offerings_start);
    assert_eq!(last_opened.offeringEnd, hist_schedule.offering_end);
    let tribute_events = provider.get_events(outbe_primitives::addresses::TRIBUTE_ADDRESS);
    assert_eq!(tribute_events.len(), 2);
    let sealed = outbe_tribute::precompile::ITribute::WorldwideDaySealed::decode_log_data(
        tribute_events.last().unwrap(),
    )
    .unwrap();
    assert_eq!(sealed.worldwideDay, 20260901);
}
