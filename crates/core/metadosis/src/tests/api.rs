use super::*;
use crate::api;

#[test]
fn typed_queries_distinguish_missing_days_from_active_schedule() {
    with_storage(|storage| {
        let wwd = 20260920.into();
        assert!(api::worldwide_day(storage.clone(), wwd).unwrap().is_none());
        assert!(api::emission_day(storage.clone(), 20261014)
            .unwrap()
            .is_none());

        super::precompile::seed_offering_day(storage.clone());

        let day = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        let expected_forming_start = wwd.start_timestamp();
        let expected_forming_end =
            expected_forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let expected_offerings_start =
            expected_forming_end + outbe_chain_constants::get_metadosis_lookback_delay_seconds();
        let expected_offering_end = expected_offerings_start
            + outbe_chain_constants::get_metadosis_offering_period_seconds();
        assert_eq!(day.worldwide_day, wwd);
        assert_eq!(day.day, 20261014);
        assert_eq!(
            (
                day.forming_start,
                day.forming_end,
                day.offerings_start,
                day.offering_end
            ),
            (
                expected_forming_start,
                expected_forming_end,
                expected_offerings_start,
                expected_offering_end
            )
        );

        let ed = api::emission_day(storage, 20261014).unwrap().unwrap();
        assert_eq!(ed.day, 20261014);
        assert_eq!(ed.wwd, wwd);
        assert_eq!(ed.emission_limit_minor, U256::from(100));
        assert_eq!(ed.metadosis_start, 50);
        assert_eq!(ed.state, emission_day_state::NONE);
    });
}
