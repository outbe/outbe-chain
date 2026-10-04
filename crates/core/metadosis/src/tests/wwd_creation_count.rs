//! The creation count is not the number of Worldwide Days still retained.

use outbe_primitives::time::WorldwideDay;

use super::with_storage;
use crate::api;
use crate::commit::NewWwdSchedule;
use crate::schema::MetadosisContract;

#[test]
fn creation_count_survives_deleting_the_retained_day() {
    with_storage(|storage| {
        let wwd = WorldwideDay::new(20_240_101);
        let mut contract = MetadosisContract::new(storage.clone());
        assert_eq!(api::worldwide_days_created(storage.clone()).unwrap(), 0);
        contract
            .create_worldwide_day_for_test(
                wwd,
                NewWwdSchedule {
                    forming_start: 1_704_067_200,
                    forming_period_seconds: 86_400,
                    lookback_delay_seconds: 86_400,
                    offering_period_seconds: 86_400,
                    waiting_period_seconds: 3_600,
                },
            )
            .unwrap();
        contract.active_wwd.insert(wwd).unwrap();
        assert_eq!(api::worldwide_days(storage.clone()).unwrap().len(), 1);
        assert_eq!(api::worldwide_days_created(storage.clone()).unwrap(), 1);

        contract.active_wwd.remove(&wwd).unwrap();
        contract.delete_worldwide_day_for_test(wwd).unwrap();
        assert!(api::worldwide_days(storage.clone()).unwrap().is_empty());
        assert_eq!(api::worldwide_days_created(storage).unwrap(), 1);
    });
}
