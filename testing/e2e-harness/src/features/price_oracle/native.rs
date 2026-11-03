//! Native fixture quotes for linked emission-day prices.

use super::*;

/// Publish this native fixture's first USD rate without an earlier default-rate sample.
pub(crate) fn start_native_quote(world: &mut World, rate: U256) {
    let cohort = resolve_cohort(world, false).expect("native Oracle cohort");
    let price = scale6_quote(rate);
    start_feeder_with_cohort_quote(
        world,
        OracleEvidencePhaseV1::Initial,
        cohort,
        &price,
        MOCK_VOLUME,
    );
    assert_native_quotes_ready(world);
}

/// Require real observations for every native reference pair before the day closes.
pub(crate) fn assert_native_quotes_ready(world: &mut World) {
    let cohort = world
        .price_oracle
        .cohort()
        .expect("native Oracle cohort before Lysis");
    let wwd = world
        .state
        .wwd
        .as_deref()
        .expect("native Offering day")
        .parse()
        .expect("numeric WWD");
    let day = world
        .rpc
        .metadosis_wwd_state_on(world.validators.primary_port(), wwd)
        .expect("native emission-day link")
        .day;
    let start = outbe_primitives::time::WorldwideDay::new(day)
        .to_timestamp_utc()
        .expect("valid native emission day");
    let end = start
        .checked_add(outbe_primitives::time::SECONDS_PER_DAY)
        .expect("native day end");
    let usd_rate = feeder_restart_expected_rate(world.price_oracle.read_controlled_quote());
    let pairs = std::iter::once((USD_ISO, usd_rate))
        .chain(
            world
                .price_oracle
                .fixed_pairs()
                .iter()
                .map(|pair| (pair.iso_code, pair.rate)),
        )
        .collect::<Vec<_>>();
    for (iso, rate) in pairs {
        let quote = outbe_primitives::asset_type::currency_address(iso);
        let (_, deadline) = publication_deadline(world, &cohort);
        loop {
            if observe_cohort_publication_inner(
                world,
                OracleEvidencePhaseV1::Initial,
                ExpectedPublication {
                    base: Address::ZERO,
                    quote,
                    strictly_after_block: cohort.checkpoint.height,
                    expected_rate: rate,
                },
                true,
                true,
            )
            .expect("native pair publication before UTC-day closure")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native reference quote did not publish"
            );
            sleep(Duration::from_millis(250));
        }
        let evidence = world.price_oracle.evidence_snapshot();
        let publication = evidence
            .canonical_publications
            .iter()
            .rev()
            .find(|p| p.phase == OracleEvidencePhaseV1::Initial && p.quote == format!("{quote:#x}"))
            .expect("native pair publication");
        assert_eq!(publication.quote, format!("{quote:#x}"));
        assert!(
            start <= publication.oracle_timestamp && publication.oracle_timestamp < end,
            "native entry-price observations must belong to emission day {day}"
        );
    }
}

/// Restore the normal later quote before the lifecycle step starts another cohort.
pub(crate) fn finish_native_quotes(world: &mut World) {
    world
        .price_oracle
        .publish_quote(
            OracleEvidencePhaseV1::ControlledUpdate,
            MOCK_PRICE,
            MOCK_VOLUME,
        )
        .expect("restore normal USD source after native Nod issuance");
    world.price_oracle.stop_feeder();
}
