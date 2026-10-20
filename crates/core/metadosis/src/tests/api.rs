use super::*;
use crate::api;

#[test]
fn typed_queries_distinguish_missing_days_from_active_schedule() {
    with_storage(|storage| {
        let wwd = 20260920.into();
        assert!(api::worldwide_day(storage.clone(), wwd).unwrap().is_none());
        assert!(api::emission_day(storage.clone(), 20261015)
            .unwrap()
            .is_none());
        assert!(api::bootstrap_end_time(storage.clone()).unwrap().is_none());

        super::precompile::seed_offering_day(storage.clone());

        let day = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(day.worldwide_day, wwd);
        assert_eq!(day.day, 20261015);
        assert_eq!(
            (
                day.forming_start,
                day.forming_end,
                day.offerings_start,
                day.offering_end
            ),
            (10, 20, 30, 40)
        );

        let ed = api::emission_day(storage.clone(), 20261015)
            .unwrap()
            .unwrap();
        assert_eq!(ed.day, 20261015);
        assert_eq!(ed.wwd, wwd);
        assert_eq!(ed.emission_limit_minor, U256::from(100));
        assert_eq!(ed.metadosis_start, 50);
        assert_eq!(ed.state, emission_day_state::NONE);

        MetadosisContract::new(storage.clone())
            .bootstrap_end_time
            .write(30)
            .unwrap();
        assert_eq!(api::bootstrap_end_time(storage).unwrap(), Some(30));
    });
}
