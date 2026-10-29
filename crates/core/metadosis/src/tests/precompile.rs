use super::*;
use alloy_sol_types::SolCall;
use outbe_emission::precompile::{dispatch as emission_dispatch, IEmission};

fn query<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output = emission_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

fn query_tribute_factory<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output =
        tribute_factory_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

#[test]
fn missing_emission_day_and_worldwide_day_revert_cleanly() {
    with_storage(|storage| {
        let ed_err = emission_dispatch(
            storage.clone(),
            &IEmission::getEmissionDayCall { day: 20261014 }.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap_err();
        assert!(ed_err.to_string().contains("EmissionDay not found"));

        let wwd_err = tribute_factory_dispatch(
            storage,
            &ITributeFactory::getWorldwideDayCall { wwd: 20260920 }.abi_encode(),
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
        let wwd: outbe_primitives::time::WorldwideDay = 20260920.into();
        let expected_forming_start = wwd.start_timestamp();
        let expected_forming_end =
            expected_forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let expected_offerings_start =
            expected_forming_end + outbe_chain_constants::get_metadosis_lookback_delay_seconds();
        let expected_offering_end = expected_offerings_start
            + outbe_chain_constants::get_metadosis_offering_period_seconds();
        let day = query_tribute_factory(
            storage.clone(),
            ITributeFactory::getWorldwideDayCall { wwd: 20260920 },
        );
        assert_eq!(day.day, 20261014);
        assert_eq!(
            (
                day.formingStart,
                day.formingEnd,
                day.offeringsStart,
                day.offeringEnd,
            ),
            (
                expected_forming_start,
                expected_forming_end,
                expected_offerings_start,
                expected_offering_end,
            )
        );
        let active: Vec<u32> = query_tribute_factory(
            storage.clone(),
            ITributeFactory::getActiveWorldwideDaysCall {},
        );
        assert_eq!(active, vec![20260920]);

        let ed = query(storage, IEmission::getEmissionDayCall { day: 20261014 });
        assert_eq!(ed.day, 20261014);
        assert_eq!(ed.emissionLimit, 100);
        assert_eq!(ed.metadosisLimit, 100);
        assert_eq!(ed.wwd, 20260920);
        assert_eq!(ed.metadosisStart, 50);
        assert_eq!(
            ed.state,
            outbe_emission::schema::emission_day_state::PENDING
        );
        assert_eq!(ed.metadosisState, emission_day_state::NONE);
    });
}

pub(super) fn seed_offering_day(storage: StorageHandle<'_>) {
    let wwd: outbe_primitives::time::WorldwideDay = 20260920.into();
    let day = 20261014u32;
    let forming_start = wwd.start_timestamp();
    let forming_end = forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
    let offerings_start =
        forming_end + outbe_chain_constants::get_metadosis_lookback_delay_seconds();
    let offering_end =
        offerings_start + outbe_chain_constants::get_metadosis_offering_period_seconds();
    let binding = outbe_primitives::storage::metadosis_process_ready_binding(
        storage.chain_id().unwrap(),
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .clone()
        .with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
            binding,
            || {
                let mut factory = TributeFactoryContract::new(storage.clone());
                outbe_tributefactory::worldwideday::create_worldwide_day(
                    &mut factory,
                    wwd,
                    outbe_tributefactory::worldwideday::NewWwdSchedule {
                        day,
                        forming_start,
                        forming_end,
                        offerings_start,
                        offering_end,
                    },
                )?;
                outbe_tributefactory::worldwideday::activate_worldwide_day(&mut factory, wwd)?;
                Ok(())
            },
        )
        .unwrap();
    let metadosis = MetadosisContract::new(storage);
    metadosis
        .emission_days()
        .create(&crate::schema::EmissionDay::new_with_limit(
            day,
            wwd,
            50,
            100,
            emission_day_state::NONE,
        ))
        .unwrap();
}
