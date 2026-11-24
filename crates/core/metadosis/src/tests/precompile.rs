use super::*;
use alloy_sol_types::SolCall;
use outbe_emission::precompile::{dispatch as emission_dispatch, IEmission};

fn query<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output = emission_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
    C::abi_decode_returns(&output).unwrap()
}

fn query_worldwide_day<C: SolCall>(storage: StorageHandle<'_>, call: C) -> C::Return {
    let output =
        worldwide_day_dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO).unwrap();
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

        let wwd_err = worldwide_day_dispatch(
            storage,
            &IWorldwideDay::getWorldwideDayCall { wwd: 20260920 }.abi_encode(),
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
        let wwd: outbe_system_common::time::WorldwideDay = 20260920.into();
        let expected_forming_start = wwd.start_timestamp().unwrap();
        let expected_forming_end =
            expected_forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let expected_offerings_start = expected_forming_start
            + outbe_chain_constants::OFFERING_START_AFTER_FORMING_START_SECONDS;
        let expected_offering_end = expected_offerings_start
            + outbe_chain_constants::get_metadosis_offering_period_seconds();
        let day = query_worldwide_day(
            storage.clone(),
            IWorldwideDay::getWorldwideDayCall { wwd: 20260920 },
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
        // `activeWorldwideDays` lists offering days. This WWD has no forming
        // VWAP and the block clock is outside its offering window.
        let active: Vec<u32> =
            query_worldwide_day(storage.clone(), IWorldwideDay::activeWorldwideDaysCall {});
        assert!(active.is_empty());

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
        assert_eq!(ed.metadosisState, metadosis_state::PENDING);
    });
}

pub(super) fn seed_offering_day(storage: StorageHandle<'_>) {
    let wwd: outbe_system_common::time::WorldwideDay = 20260920.into();
    let day = 20261014u32;
    let forming_start = wwd.start_timestamp().unwrap();
    let forming_end = forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
    let offerings_start =
        forming_start + outbe_chain_constants::OFFERING_START_AFTER_FORMING_START_SECONDS;
    let offering_end =
        offerings_start + outbe_chain_constants::get_metadosis_offering_period_seconds();
    let binding = outbe_system_common::metadosis_bindings::metadosis_process_ready_binding(
        storage.chain_id().unwrap(),
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .clone()
        .with_scoped_mutation::<outbe_primitives::storage::CycleLifecyclePurpose, _>(
            binding,
            || {
                let mut factory = WorldwideDayContract::new(storage.clone());
                outbe_worldwideday::open_worldwide_day(
                    &mut factory,
                    wwd,
                    outbe_worldwideday::NewWwdSchedule {
                        day,
                        forming_start,
                        forming_end,
                        offerings_start,
                        offering_end,
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
            metadosis_state::PENDING,
        ))
        .unwrap();
}

#[test]
fn worldwide_day_reads_have_no_get_prefix() {
    let old_call = |signature: &str, args: &[u8]| {
        let mut data = alloy_primitives::keccak256(signature)[..4].to_vec();
        data.extend_from_slice(args);
        data
    };
    with_storage(|storage| {
        seed_offering_day(storage.clone());
        let active: Vec<u32> =
            query_worldwide_day(storage.clone(), IWorldwideDay::activeWorldwideDaysCall {});
        assert!(active.is_empty());
        let totals = worldwide_day_dispatch(
            storage.clone(),
            &IWorldwideDay::dayTotalsCall {
                emissionDay: 20261014,
            }
            .abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let totals = IWorldwideDay::dayTotalsCall::abi_decode_returns(&totals).unwrap();
        assert_eq!(totals.nodCount, 0);

        let day = U256::from(20261014u32).to_be_bytes::<32>();
        assert!(worldwide_day_dispatch(
            storage.clone(),
            &old_call("getDayTotals(uint32)", &day),
            Address::ZERO,
            U256::ZERO,
        )
        .is_err());
        assert!(worldwide_day_dispatch(
            storage,
            &old_call("getActiveWorldwideDays()", &[]),
            Address::ZERO,
            U256::ZERO,
        )
        .is_err());
    });
}
