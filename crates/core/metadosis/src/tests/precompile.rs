use super::*;
use alloy_sol_types::SolCall;

fn query<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output =
        metadosis_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

fn query_tribute<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output = tribute_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

#[test]
fn missing_emission_day_and_worldwide_day_revert_cleanly() {
    with_storage(|storage| {
        let ed_err = metadosis_dispatch(
            storage.clone(),
            &IMetadosis::getEmissionDayCall { day: 20261015 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        assert!(ed_err.to_string().contains("EmissionDay not found"));

        let wwd_err = tribute_dispatch(
            storage,
            &ITribute::getWorldwideDayCall { wwd: 20260920 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        assert!(wwd_err.to_string().contains("WorldwideDay not found"));
    });
}

#[test]
fn worldwide_day_and_emission_day_abi_preserve_bidirectional_link() {
    with_storage(|storage| {
        seed_offering_day(storage.clone());
        let day = query_tribute(
            storage.clone(),
            ITribute::getWorldwideDayCall { wwd: 20260920 },
        );
        assert_eq!(day.day, 20261015);
        assert_eq!(
            (
                day.formingStart,
                day.formingEnd,
                day.offeringsStart,
                day.offeringEnd,
            ),
            (10, 20, 30, 40)
        );
        let active: Vec<u32> =
            query_tribute(storage.clone(), ITribute::getActiveWorldwideDaysCall {});
        assert_eq!(active, vec![20260920]);

        let ed = query(storage, IMetadosis::getEmissionDayCall { day: 20261015 });
        assert_eq!(ed.emissionDay, 20261015);
        assert_eq!(ed.emissionLimitMinor, U256::from(100));
        assert_eq!(ed.wwd, 20260920);
        assert_eq!(ed.metadosisStart, 50);
        assert_eq!(ed.state, emission_day_state::NONE);
    });
}

pub(super) fn seed_offering_day(storage: StorageHandle<'_>) {
    let wwd = 20260920.into();
    let day = 20261015u32;
    let mut tribute = TributeContract::new(storage.clone());
    outbe_tribute::worldwideday::create_worldwide_day(
        &mut tribute,
        wwd,
        outbe_tribute::worldwideday::NewWwdSchedule {
            day,
            forming_start: 10,
            forming_end: 20,
            offerings_start: 30,
            offering_end: 40,
        },
    )
    .unwrap();
    let metadosis = MetadosisContract::new(storage);
    metadosis
        .emission_days
        .create(&crate::schema::EmissionDay {
            day,
            emission_limit_minor: U256::from(100),
            wwd,
            metadosis_start: 50,
            state: emission_day_state::NONE,
        })
        .unwrap();
}
