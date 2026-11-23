use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{CycleLifecyclePurpose, MutationPurposeTag, StorageHandle};
use outbe_system_common::metadosis_bindings::metadosis_process_ready_binding;
use outbe_system_common::time::WorldwideDay;

use crate::precompile::IWorldwideDay;
use crate::schema::WorldwideDayContract;

const CHAIN_ID: u64 = 1;

#[test]
fn pipeline_cap_and_historical_close() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 30);
    StorageHandle::enter(&mut provider, |storage| {
        let binding = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
                let mut factory = WorldwideDayContract::new(storage.clone());
                let mut day_key = 20261001u32;
                for _ in 0..crate::constants::MAX_PIPELINE_WWDS {
                    let cur_wwd = WorldwideDay::new(day_key);
                    let (schedule, _) = crate::compute_wwd_schedule(cur_wwd)?;
                    assert!(crate::open_worldwide_day(&mut factory, cur_wwd, schedule)?);
                    assert_eq!(
                        crate::emission_day_of(storage.clone(), cur_wwd)?,
                        Some(outbe_system_common::time::EmissionDate::new(schedule.day))
                    );
                    day_key = outbe_system_common::time::next_date_key(day_key).unwrap();
                }
                let overflow_wwd = WorldwideDay::new(day_key);
                let (overflow_schedule, _) = crate::compute_wwd_schedule(overflow_wwd)?;
                let cap_err =
                    crate::open_worldwide_day(&mut factory, overflow_wwd, overflow_schedule)
                        .unwrap_err();
                assert!(cap_err
                    .to_string()
                    .contains(&crate::constants::MAX_PIPELINE_WWDS.to_string()));
                Ok(())
            })
            .unwrap();

        // The pipeline is full. Close one day, then create and close another.
        // Create leaves the day unsealed in `pipeline_wwd`. `close_worldwide_day`
        // is the single seal.
        let filled = WorldwideDay::new(20261001);
        let hist_wwd = WorldwideDay::new(20260901);
        let (hist_schedule, _) = crate::compute_wwd_schedule(hist_wwd).unwrap();
        storage
            .set_block_timestamp(U256::from(hist_schedule.offering_end))
            .unwrap();
        let binding2 = metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding2, || {
                let mut factory = WorldwideDayContract::new(storage.clone());
                crate::close_worldwide_day(&mut factory, filled)?;
                assert!(!factory.pipeline_wwd.read_all()?.contains(&filled));
                assert!(crate::is_sealed(storage.clone(), filled)?);

                assert!(crate::open_worldwide_day(
                    &mut factory,
                    hist_wwd,
                    hist_schedule
                )?);
                assert!(factory.pipeline_wwd.read_all()?.contains(&hist_wwd));
                assert!(!crate::is_sealed(storage.clone(), hist_wwd)?);
                assert_eq!(
                    crate::emission_day_of(storage.clone(), hist_wwd)?,
                    Some(outbe_system_common::time::EmissionDate::new(
                        hist_schedule.day
                    ))
                );
                crate::close_worldwide_day(&mut factory, hist_wwd)?;
                assert!(!factory.pipeline_wwd.read_all()?.contains(&hist_wwd));
                assert!(crate::is_sealed(storage.clone(), hist_wwd)?);
                Ok(())
            })
            .unwrap();
    });
    let events = provider.get_events(outbe_system_common::addresses::WORLDWIDE_DAY_ADDRESS);
    let opened: Vec<_> = events
        .iter()
        .filter_map(|log| IWorldwideDay::WorldwideDayOpened::decode_log_data(log).ok())
        .collect();
    assert_eq!(opened.len(), crate::constants::MAX_PIPELINE_WWDS + 1);
    let last_opened = opened.last().unwrap();
    let (hist_schedule, _) = crate::compute_wwd_schedule(WorldwideDay::new(20260901)).unwrap();
    assert_eq!(last_opened.worldwideDay, 20260901);
    assert_eq!(last_opened.formingStart, hist_schedule.forming_start);
    assert_eq!(last_opened.formingEnd, hist_schedule.forming_end);
    assert_eq!(last_opened.offeringStart, hist_schedule.offerings_start);
    assert_eq!(last_opened.offeringEnd, hist_schedule.offering_end);
    let sealed: Vec<_> = events
        .iter()
        .filter_map(|log| IWorldwideDay::WorldwideDaySealed::decode_log_data(log).ok())
        .map(|event| event.worldwideDay)
        .collect();
    assert_eq!(sealed, vec![20261001, 20260901]);
}

fn open_on(storage: &StorageHandle<'_>, wwd: WorldwideDay) {
    let binding = metadosis_process_ready_binding(
        storage.chain_id().unwrap(),
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .clone()
        .with_scoped_mutation::<CycleLifecyclePurpose, _>(binding, || {
            let mut factory = WorldwideDayContract::new(storage.clone());
            let (schedule, _) = crate::compute_wwd_schedule(wwd)?;
            crate::open_worldwide_day(&mut factory, wwd, schedule).map(|_| ())
        })
        .unwrap();
}

#[test]
fn offering_is_the_next_wwd() {
    use outbe_system_common::time::date_key_to_utc_timestamp;
    let wwd = WorldwideDay::new(20_260_802);
    let (schedule, metadosis_start) = crate::compute_wwd_schedule(wwd).unwrap();
    let next_wwd_start = WorldwideDay::new(20_260_803).start_timestamp().unwrap();
    assert_eq!(schedule.offerings_start, next_wwd_start);
    assert_eq!(schedule.offering_end, next_wwd_start + 50 * 60 * 60);
    assert_eq!(schedule.forming_end, schedule.offering_end - 24 * 60 * 60);
    assert_eq!(schedule.day, 20_260_803);
    assert_eq!(metadosis_start, schedule.offering_end);

    // From 10:00 to 12:00 UTC three offering windows are open.
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(
        date_key_to_utc_timestamp(20_260_804).unwrap() + 11 * 60 * 60,
    ));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
    StorageHandle::enter(&mut provider, |storage| {
        for key in [20_260_802, 20_260_803, 20_260_804] {
            open_on(&storage, WorldwideDay::new(key));
        }
        let mut offering = crate::offering_worldwide_days(storage).unwrap();
        offering.sort();
        assert_eq!(
            offering,
            vec![
                wwd,
                WorldwideDay::new(20_260_803),
                WorldwideDay::new(20_260_804)
            ]
        );
    });
}

#[test]
fn the_offering_window_closes_at_its_end_before_the_seal() {
    let wwd = WorldwideDay::new(20_260_802);
    let (schedule, _) = crate::compute_wwd_schedule(wwd).unwrap();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_timestamp(U256::from(schedule.offering_end - 1));
    provider.enable_scoped_mutation_frames(MutationPurposeTag::CycleLifecycle, 8);
    StorageHandle::enter(&mut provider, |storage| {
        open_on(&storage, wwd);
        assert!(crate::is_offering_day(storage.clone(), wwd).unwrap());
        storage
            .set_block_timestamp(U256::from(schedule.offering_end))
            .unwrap();
        assert!(!crate::is_sealed(storage.clone(), wwd).unwrap());
        assert!(!crate::is_offering_day(storage.clone(), wwd).unwrap());
    });
}
