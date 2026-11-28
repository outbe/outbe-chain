use super::*;

#[test]
fn typed_queries_distinguish_missing_days_from_active_schedule() {
    with_storage(|storage| {
        let wwd = 20260920.into();
        assert!(
            outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd)
                .unwrap()
                .is_none()
        );
        assert!(
            outbe_emission::api::get_emission_day(storage.clone(), 20261014)
                .unwrap()
                .is_none()
        );

        super::precompile::seed_offering_day(storage.clone());

        let day = outbe_worldwideday::api::get_worldwide_day(storage.clone(), wwd)
            .unwrap()
            .unwrap();
        let expected_forming_start = wwd.start_timestamp().unwrap();
        let expected_forming_end =
            expected_forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let expected_offering_start = expected_forming_start
            + outbe_chain_constants::OFFERING_START_AFTER_FORMING_START_SECONDS;
        let expected_offering_end = expected_offering_start
            + outbe_chain_constants::get_metadosis_offering_period_seconds();
        assert_eq!(day.wwd, wwd);
        assert_eq!(day.emission_day, 20261014);
        assert_eq!(
            (
                day.forming_start().unwrap(),
                day.forming_end().unwrap(),
                day.offering_start,
                day.offering_end
            ),
            (
                expected_forming_start,
                expected_forming_end,
                expected_offering_start,
                expected_offering_end
            )
        );

        let ed = outbe_emission::api::get_emission_day(storage, 20261014)
            .unwrap()
            .unwrap();
        assert_eq!(ed.day, 20261014);
        assert_eq!(ed.wwd, wwd);
        assert_eq!(ed.metadosis_limit, 100);
        assert_eq!(ed.metadosis_start, 50);
        assert_eq!(ed.metadosis_state, metadosis_state::PENDING);
    });
}
