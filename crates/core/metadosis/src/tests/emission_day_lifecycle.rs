use super::*;
use crate::api;
use outbe_primitives::time::{
    date_key_to_utc_timestamp, day_number_between, next_date_key, timestamp_to_date_key,
    WorldwideDay,
};

#[test]
fn init_genesis_day_creates_7_day_lookahead_with_bidirectional_links_and_closed_form_limits() {
    with_storage(|storage| {
        let wwd_0 = WorldwideDay::new(20260920);
        let genesis_ts = wwd_0.start_timestamp() + 3_600;
        let anchor_utc_day = timestamp_to_date_key(genesis_ts);
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, genesis_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let active_wwds = TributeContract::new(storage.clone())
            .active_wwd
            .read_all()
            .unwrap();
        assert_eq!(active_wwds.len(), 7);

        // Verify wwd = 20260920 -> day = 20261014 (and wwd = 20260925 -> day = 20261019)
        let wwd_20 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260920))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_20.day, 20261014);

        let ed_14 = api::emission_day(storage.clone(), 20261014)
            .unwrap()
            .unwrap();
        assert_eq!(ed_14.day, 20261014);
        assert_eq!(ed_14.wwd, WorldwideDay::new(20260920));
        assert_eq!(
            ed_14.metadosis_start,
            date_key_to_utc_timestamp(next_date_key(20261014)) + 1
        );
        assert_eq!(ed_14.state, emission_day_state::NONE);
        assert_eq!(
            ed_14.emission_limit_minor,
            outbe_primitives::emission::metadosis_day_emission_limit(
                day_number_between(anchor_utc_day, 20261014).unwrap()
            )
        );

        let wwd_25 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260925))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_25.day, 20261019);
        let ed_19 = api::emission_day(storage.clone(), 20261019)
            .unwrap()
            .unwrap();
        assert_eq!(ed_19.wwd, WorldwideDay::new(20260925));
        assert_eq!(
            ed_19.emission_limit_minor,
            outbe_primitives::emission::metadosis_day_emission_limit(
                day_number_between(anchor_utc_day, 20261019).unwrap()
            )
        );

        // Verify all 7 pairs have symmetric bi-directional links
        let mut wwd_key = 20260920;
        for _ in 0..7u32 {
            let wwd = WorldwideDay::new(wwd_key);
            let wwd_proj = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
            let ed_proj = api::emission_day(storage.clone(), wwd_proj.day)
                .unwrap()
                .unwrap();
            assert_eq!(ed_proj.wwd, wwd);
            assert_eq!(
                ed_proj.emission_limit_minor,
                outbe_primitives::emission::metadosis_day_emission_limit(
                    day_number_between(anchor_utc_day, ed_proj.day).unwrap()
                )
            );
            assert_eq!(
                ed_proj.metadosis_start,
                date_key_to_utc_timestamp(next_date_key(ed_proj.day)) + 1
            );
            wwd_key = next_date_key(wwd_key);
        }
    });
}

#[test]
fn oip_00335_schedule_places_emission_day_after_offering_wwd_and_resumes_oldest_due_after_halt() {
    with_storage(|storage| {
        let wwd_sep_01 = WorldwideDay::new(20260901);
        let genesis_ts = wwd_sep_01.start_timestamp() + 3_600;
        let anchor_utc_day = timestamp_to_date_key(genesis_ts);
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, genesis_ts, CHAIN_ID),
            storage.clone(),
        );
        crate::commands::init_genesis_day(&ctx).unwrap();

        let wwd_proj = api::worldwide_day(storage.clone(), wwd_sep_01)
            .unwrap()
            .unwrap();
        assert_eq!(
            WorldwideDay::from_timestamp(wwd_proj.offerings_start),
            WorldwideDay::new(20260924)
        );
        assert_eq!(
            wwd_proj.offering_end,
            date_key_to_utc_timestamp(20260925) + 12 * 3_600
        );
        assert_eq!(wwd_proj.day, 20260925);

        let expected_metadosis_start = date_key_to_utc_timestamp(20260926) + 1;
        assert_eq!(
            expected_metadosis_start,
            wwd_proj.offering_end + 12 * 3_600 + 1
        );

        let ed_25 = api::emission_day(storage.clone(), 20260925)
            .unwrap()
            .unwrap();
        assert_eq!(ed_25.day, 20260925);
        assert_eq!(ed_25.wwd, wwd_sep_01);
        assert_eq!(ed_25.metadosis_start, expected_metadosis_start);
        assert_eq!(
            ed_25.emission_limit_minor,
            outbe_primitives::emission::metadosis_day_emission_limit(
                day_number_between(anchor_utc_day, 20260925).unwrap()
            )
        );

        let metadosis = MetadosisContract::new(storage.clone());
        let before_start = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, expected_metadosis_start - 1, CHAIN_ID),
            storage.clone(),
        );
        assert!(crate::lifecycle::find_due_emission_day(&metadosis, &before_start)
            .unwrap()
            .is_none());

        let at_start = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(3, expected_metadosis_start, CHAIN_ID),
            storage.clone(),
        );
        let due_at_start = crate::lifecycle::find_due_emission_day(&metadosis, &at_start)
            .unwrap()
            .unwrap();
        assert_eq!(due_at_start.day, 20260925);
        assert_eq!(due_at_start.wwd, wwd_sep_01);

        // Simulate a halt from 25 Sep to 28 Sep 2026: the first block after restart
        // must still select and execute the oldest due emission day (20260925).
        let restart_ts = date_key_to_utc_timestamp(20260928) + 3_600;
        arm_reference_price(&storage, restart_ts);
        let restart_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(4, restart_ts, CHAIN_ID),
            storage.clone(),
        );
        let due_after_halt = crate::lifecycle::find_due_emission_day(&metadosis, &restart_ctx)
            .unwrap()
            .unwrap();
        assert_eq!(due_after_halt.day, 20260925);
        assert_eq!(due_after_halt.wwd, wwd_sep_01);

        crate::commands::start_metadosis(&restart_ctx).unwrap();
        let completed = api::emission_day(storage, 20260925).unwrap().unwrap();
        assert_eq!(completed.day, 20260925);
        assert_eq!(completed.wwd, wwd_sep_01);
        assert_eq!(completed.state, emission_day_state::SUCCEEDED);
    });
}
