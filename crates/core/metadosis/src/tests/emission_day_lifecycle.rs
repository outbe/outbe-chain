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

        // Verify wwd = 20260920 -> day = 20261015 (and wwd = 20260925 -> day = 20261020)
        let wwd_20 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260920))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_20.day, 20261015);

        let ed_15 = api::emission_day(storage.clone(), 20261015)
            .unwrap()
            .unwrap();
        assert_eq!(ed_15.day, 20261015);
        assert_eq!(ed_15.wwd, WorldwideDay::new(20260920));
        assert_eq!(
            ed_15.metadosis_start,
            date_key_to_utc_timestamp(next_date_key(20261015).expect("date step")) + 1
        );
        assert_eq!(ed_15.state, emission_day_state::NONE);
        assert_eq!(
            ed_15.emission_limit_minor,
            outbe_primitives::emission::metadosis_day_emission_limit(
                day_number_between(anchor_utc_day, 20261015).unwrap()
            )
        );

        let wwd_25 = api::worldwide_day(storage.clone(), WorldwideDay::new(20260925))
            .unwrap()
            .unwrap();
        assert_eq!(wwd_25.day, 20261020);
        let ed_20 = api::emission_day(storage.clone(), 20261020)
            .unwrap()
            .unwrap();
        assert_eq!(ed_20.wwd, WorldwideDay::new(20260925));
        assert_eq!(
            ed_20.emission_limit_minor,
            outbe_primitives::emission::metadosis_day_emission_limit(
                day_number_between(anchor_utc_day, 20261020).unwrap()
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
                date_key_to_utc_timestamp(next_date_key(ed_proj.day).expect("date step")) + 1
            );
            wwd_key = next_date_key(wwd_key).expect("date step");
        }
    });
}
