//! A worldwide-day VWAP that cannot be summed does not stop the cycle edge.

use alloy_primitives::U256;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::time::SECONDS_PER_DAY;

use super::*;

#[test]
fn overflowing_worldwide_day_vwap_keeps_the_forming_edge() {
    with_storage(|storage| {
        let wwd = outbe_primitives::time::WorldwideDay::new(20260302);
        let forming_start = wwd.start_timestamp();
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;

        let mut metadosis = MetadosisContract::new(storage.clone());
        metadosis
            .create_worldwide_day(
                wwd,
                forming_start,
                LOOKBACK_DELAY_HOURS,
                OFFERING_PERIOD_HOURS,
            )
            .unwrap();
        metadosis.add_active_wwd(wwd).unwrap();
        TributeContract::new(storage.clone()).seal_day(wwd).unwrap();

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        let pair = outbe_oracle::api::DAY_TYPE_PAIR;
        let mut oracle = OracleContract::new(storage.clone());
        // suffix(D) and daily(D+1) each hold a full U256 word. The day reader adds both.
        oracle
            .write_snapshot(forming_start, &[(pair, U256::from(1u64), U256::MAX)])
            .unwrap();
        oracle
            .write_snapshot(
                forming_start + SECONDS_PER_DAY,
                &[(pair, U256::from(1u64), U256::MAX)],
            )
            .unwrap();

        arm_genesis_ocomp(&storage, outbe_primitives::chain::CHAIN_ID);
        arm_reference_price(&storage, forming_end);
        form_due_fixture_day_limits(&storage, forming_end);
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, forming_end, outbe_primitives::chain::CHAIN_ID),
            storage.clone(),
        );
        with_active_scope(ctx.storage.clone(), |scope, parent| {
            crate::commands::start_metadosis(&ctx, scope, parent)
        })
        .expect("the forming edge commits when the day VWAP overflows");

        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis.get_wwd_status(wwd).unwrap(),
            status::LOOKBACK_DELAY
        );
        assert!(metadosis
            .worldwide_days
            .entry(wwd)
            .current_vwap()
            .read()
            .unwrap()
            .is_zero());
        let missing = OracleContract::new(storage)
            .get_worldwide_day_vwap_snapshot(wwd)
            .expect_err("the overflowing snapshot is not stored");
        assert!(
            missing.to_string().contains("not found"),
            "unexpected snapshot error: {missing}"
        );
    });
}
