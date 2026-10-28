use super::*;
use alloy_sol_types::SolCall;
use outbe_emission::precompile::{dispatch as emission_dispatch, IEmission};

fn query<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output = emission_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

fn query_tribute<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output = tribute_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
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
        assert_eq!(day.day, 20261014);
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
    let wwd = 20260920.into();
    let day = 20261014u32;
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
                )?;
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
