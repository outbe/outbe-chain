//! Independent WWD and frozen Nod prices from historical Oracle observations.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, U256};
use outbe_primitives::{
    addresses::{NOD_ADDRESS, ORACLE_ADDRESS},
    time::WorldwideDay,
};
use serde::Serialize;

use crate::internal::eth;
use crate::world::World;

const SNAPSHOT_LIMIT: u32 = 4_096;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Quote {
    snapshot_id: u64,
    timestamp: u64,
    rate: U256,
    volume: U256,
}

fn history(world: &World, port: u16, height: u64) -> Vec<Quote> {
    let history = eth::read_call_at(
        &world.rpc.url(port),
        ORACLE_ADDRESS,
        &eth::IOracle::getAllPriceSnapshotHistoryCall {
            count: SNAPSHOT_LIMIT + 1,
        },
        height,
    )
    .expect("complete bounded raw Oracle snapshot history");
    let count = history.snapshotIds.len();
    for size in [
        history.timestamps.len(),
        history.bases.len(),
        history.quotes.len(),
        history.rates.len(),
        history.volumes.len(),
    ] {
        assert_eq!(size, count, "raw snapshot columns have equal lengths");
    }
    let ids: BTreeSet<_> = history.snapshotIds.iter().copied().collect();
    assert!(
        ids.len() <= SNAPSHOT_LIMIT as usize,
        "snapshot reference would be truncated"
    );
    // This fresh fixture is shorter than retention and starts with snapshot0.
    // Require the entire catalog, rather than assuming the requested cap suffices.
    assert!(
        !ids.is_empty(),
        "fresh fixture seeded raw Oracle observations"
    );
    for (expected, actual) in ids.into_iter().enumerate() {
        assert_eq!(
            actual, expected as u64,
            "missing raw snapshot in fresh fixture"
        );
    }
    let usd = outbe_primitives::asset_type::currency_address(840);
    let mut quotes = BTreeMap::new();
    for i in 0..count {
        if history.bases[i] != Address::ZERO || history.quotes[i] != usd {
            continue;
        }
        let quote = Quote {
            snapshot_id: history.snapshotIds[i],
            timestamp: history.timestamps[i],
            rate: history.rates[i],
            volume: history.volumes[i],
        };
        assert!(
            quotes.insert(quote.snapshot_id, quote).is_none(),
            "one COEN/840 observation per raw snapshot"
        );
    }
    quotes.into_values().collect()
}

fn scale6(value: &str) -> U256 {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    assert!(!whole.is_empty() && whole.bytes().all(|v| v.is_ascii_digit()));
    assert!(fraction.len() <= 6 && fraction.bytes().all(|v| v.is_ascii_digit()));
    let whole: U256 = whole.parse().expect("controlled decimal whole part");
    let padded = format!("{fraction:0<6}");
    let fractional: U256 = padded.parse().expect("controlled decimal fraction");
    whole
        .checked_mul(U256::from(1_000_000))
        .and_then(|v| v.checked_add(fractional))
        .expect("controlled scale6 input fits U256")
}

fn feeder_terms(world: &World) -> (U256, U256) {
    let evidence = world.price_oracle.evidence_snapshot();
    let mut sources = BTreeMap::new();
    for source in &evidence.controlled_sources {
        if source.oracle_pair != "COEN/840" {
            continue;
        }
        assert_eq!(source.source_market, "mock_http:COEN/840");
        let terms = (scale6(&source.price), scale6(&source.volume));
        if let Some(previous) = sources.insert(source.validator_index, terms) {
            assert_eq!(
                previous, terms,
                "fresh lane preserves quote across feeder restarts"
            );
        }
    }
    let cohort = world
        .price_oracle
        .cohort()
        .expect("controlled feeder cohort");
    assert_eq!(
        sources.len(),
        cohort.quorum,
        "one configured source for each quorum feeder"
    );
    let rate = sources.values().next().expect("controlled sources").0;
    assert!(!rate.is_zero());
    let volume = sources.values().fold(U256::ZERO, |sum, (price, volume)| {
        assert_eq!(*price, rate, "unanimous controlled price fixture");
        assert!(!volume.is_zero(), "positive controlled source volume");
        sum.checked_add(*volume).expect("controlled quorum volume")
    });
    (rate, volume)
}

fn weighted_price(quotes: &[Quote], start: u64, end: u64) -> U256 {
    assert!(start < end, "nonempty FORMING interval");
    let mut numerator = U256::ZERO;
    let mut denominator = U256::ZERO;
    for quote in quotes
        .iter()
        .filter(|q| start <= q.timestamp && q.timestamp < end)
    {
        let weight = if quote.volume.is_zero() {
            U256::from(1_000_000)
        } else {
            quote.volume
        };
        numerator = numerator
            .checked_add(
                quote
                    .rate
                    .checked_mul(weight)
                    .expect("reference price-volume product"),
            )
            .expect("reference price-volume sum");
        denominator = denominator
            .checked_add(weight)
            .expect("reference volume sum");
    }
    assert!(
        !denominator.is_zero(),
        "FORMING interval contains priced observations"
    );
    numerator / denominator
}

pub(crate) fn fresh_wwd_price(world: &World, height: u64) -> U256 {
    let lifecycle = world
        .state
        .metadosis_fresh_lifecycle_observation
        .as_ref()
        .expect("singleton Nod fixture has a fresh runtime-created WWD");
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("WWD input checkpoint");
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("WWD checkpoint");
    let genesis = world
        .rpc
        .checkpoint_at(ports[0], 0)
        .expect("genesis checkpoint");
    let seeds = history(world, ports[0], 0);
    let observed = history(world, ports[0], height);
    assert!(
        observed.len() > seeds.len(),
        "real feeder added raw observations"
    );
    assert_eq!(
        &observed[..seeds.len()],
        seeds,
        "raw genesis observations are preserved"
    );
    let start = lifecycle.forming_start;
    let end = lifecycle.forming_end;
    let in_window = |q: &&Quote| start <= q.timestamp && q.timestamp < end;
    let seeded: Vec<_> = seeds.iter().filter(in_window).collect();
    assert_eq!(seeded.len(), 1, "one declared fresh-WWD seed in FORMING");
    assert_eq!(seeded[0].timestamp, start);
    assert_eq!(seeded[0].rate, U256::from(2));
    assert_eq!(seeded[0].volume, U256::from(1_000_000));
    let (rate, volume) = feeder_terms(world);
    let fed = &observed[seeds.len()..];
    assert!(
        fed.iter().filter(in_window).count() > 0,
        "at least one live feeder publication belongs to FORMING"
    );
    for quote in fed {
        assert_eq!(
            (quote.rate, quote.volume),
            (rate, volume),
            "raw publication matches controlled quorum source terms"
        );
    }
    let expected = weighted_price(&observed, start, end);
    let usd = outbe_primitives::asset_type::currency_address(840);
    for &port in &ports {
        assert_eq!(
            world.rpc.checkpoint_at(port, 0).expect("peer genesis"),
            genesis
        );
        assert_eq!(
            history(world, port, 0),
            seeds,
            "seed observations on {port}"
        );
        assert_eq!(
            history(world, port, height),
            observed,
            "raw observation population on {port}"
        );
        let snapshot = eth::read_call_at(
            &world.rpc.url(port),
            ORACLE_ADDRESS,
            &eth::IOracle::getWorldwideDayVwapSnapshotCall {
                worldwideDay: lifecycle.worldwide_day,
            },
            height,
        )
        .expect("stored WWD VWAP at input checkpoint");
        assert_eq!(
            (snapshot.startTime, snapshot.endTime),
            (start, end),
            "stored FORMING window"
        );
        assert_eq!(snapshot.bases.len(), snapshot.quotes.len());
        assert_eq!(snapshot.bases.len(), snapshot.vwaps.len());
        let prices: Vec<_> = snapshot
            .bases
            .iter()
            .zip(&snapshot.quotes)
            .zip(&snapshot.vwaps)
            .filter_map(|((base, quote), price)| {
                (*base == Address::ZERO && *quote == usd).then_some(*price)
            })
            .collect();
        assert_eq!(
            prices,
            vec![expected],
            "independent seeded-plus-fed WWD VWAP on {port}"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("recheck WWD checkpoint"),
            checkpoint
        );
    }
    eprintln!(
        "WWD_VWAP_EXPECTATION {}",
        serde_json::json!({
            "worldwide_day": lifecycle.worldwide_day, "start": start, "end": end,
            "height": height, "block_hash": checkpoint.block_hash, "state_root": checkpoint.state_root,
            "seed_count": seeds.len(), "controlled_rate": rate, "controlled_quorum_volume": volume,
            "observations": observed, "expected_vwap": expected,
        })
    );
    expected
}

fn previous_day_entry_price(quotes: &[Quote], prepared_at: u64) -> U256 {
    let end = prepared_at / 86_400 * 86_400;
    let price = weighted_price(quotes, end.saturating_sub(86_400), end);
    assert!(
        !price.is_zero(),
        "entry price requires a positive previous-day VWAP"
    );
    price
}

fn frozen_usd_price_at(world: &World, port: u16, day: u32, height: u64) -> U256 {
    // Share only the storage slot layout, never the production price evaluator.
    let slots = outbe_nod::openings::entry_price_slots(WorldwideDay::new(day), &[840])
        .expect("bounded USD snapshot slots");
    let words = slots
        .into_iter()
        .map(|slot| {
            let raw = eth::raw_json_with_params(
                &world.rpc.url(port),
                "eth_getStorageAt",
                serde_json::json!([NOD_ADDRESS, slot, format!("0x{height:x}")]),
            )
            .expect("historical frozen Nod snapshot word");
            serde_json::from_value::<U256>(raw).expect("canonical snapshot word")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        words[0],
        U256::from(1),
        "Nod entry-price snapshot is frozen"
    );
    words[1]
}

/// Recompute the USD entry price from the UTC day preceding preparation.
/// Find preparation from the first frozen snapshot, even when the request was delayed.
pub(crate) fn frozen_entry_price(world: &World) -> U256 {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("public JobIntent");
    let height = request.request_height;
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("price input checkpoint");
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("price checkpoint identity");
    assert_eq!(checkpoint.block_hash, request.request_block_hash);
    let intent = world
        .rpc
        .ocomp_job_record_at_on(ports[0], request.intent_id, height)
        .expect("price input JobIntent")
        .intent;
    let frozen_slot =
        outbe_nod::openings::entry_price_slots(WorldwideDay::new(request.worldwide_day), &[840])
            .expect("USD snapshot slots")[0];
    // The frozen flag is monotonic. Locate its first block with logarithmic RPC reads.
    let (mut low, mut high) = (0, height);
    while low < high {
        let mid = low + (high - low) / 2;
        let raw = eth::raw_json_with_params(
            &world.rpc.url(ports[0]),
            "eth_getStorageAt",
            serde_json::json!([NOD_ADDRESS, frozen_slot, format!("0x{mid:x}")]),
        )
        .expect("historical preparation flag");
        let frozen: U256 = serde_json::from_value(raw).expect("canonical frozen flag");
        if frozen.is_zero() {
            low = mid + 1;
        } else {
            assert_eq!(frozen, U256::from(1));
            high = mid;
        }
    }
    let preparation_height = low;
    let preparation_checkpoint = world
        .rpc
        .checkpoint_at(ports[0], preparation_height)
        .expect("preparation checkpoint");
    let prepared_at = world
        .rpc
        .block_timestamp(ports[0], preparation_height)
        .expect("preparation timestamp");
    let quotes = history(world, ports[0], preparation_height);
    let expected = previous_day_entry_price(&quotes, prepared_at);
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .ocomp_job_record_at_on(port, request.intent_id, height)
                .expect("peer price input JobIntent")
                .intent,
            intent
        );
        assert_eq!(
            history(world, port, preparation_height),
            quotes,
            "Oracle price history on {port}"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, preparation_height)
                .expect("peer preparation checkpoint"),
            preparation_checkpoint
        );
        assert_eq!(
            frozen_usd_price_at(world, port, request.worldwide_day, height),
            expected,
            "independent frozen entry price on {port}"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("recheck price checkpoint"),
            checkpoint
        );
    }
    eprintln!(
        "NOD_ENTRY_PRICE_EXPECTATION {}",
        serde_json::json!({
            "worldwide_day": request.worldwide_day, "height": height,
            "block_hash": checkpoint.block_hash, "state_root": checkpoint.state_root,
            "logical_evaluation_time": intent.logical_evaluation_time,
            "preparation_height": preparation_height, "prepared_at": prepared_at,
            "observations": quotes, "expected_entry_price": expected,
        })
    );
    expected
}

#[cucumber::then("the original Nod entry-price snapshot survives Oracle updates and restart")]
fn frozen_entry_price_survives_updates(world: &mut World) {
    let expected = frozen_entry_price(world);
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("original public JobIntent");
    let ports = world.validators.committee_ports();
    let height = ports
        .iter()
        .map(|&port| {
            world
                .rpc
                .finalized_result(port)
                .expect("latest finalized snapshot checkpoint")
        })
        .min()
        .expect("committee");
    assert!(
        height > request.request_height,
        "check the snapshot after its creation"
    );
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("snapshot checkpoint");
    for &port in &ports {
        assert_eq!(
            frozen_usd_price_at(world, port, request.worldwide_day, height),
            expected,
            "Oracle changes must not reprice the original Nod on {port}"
        );
        let current = eth::read_call_at(
            &world.rpc.url(port),
            ORACLE_ADDRESS,
            &eth::IOracle::getCoenExchangeRateForCall { isoCode: 840 },
            height,
        )
        .expect("changed Oracle quote");
        assert_ne!(
            current, expected,
            "fixture changed the Oracle price after freezing"
        );
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("peer snapshot checkpoint"),
            checkpoint
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(timestamp: u64, rate: u64, volume: u64) -> Quote {
        Quote {
            snapshot_id: timestamp,
            timestamp,
            rate: U256::from(rate),
            volume: U256::from(volume),
        }
    }

    #[test]
    fn exact_weighted_floor_uses_half_open_window_and_zero_volume_sentinel() {
        let quotes = [
            quote(9, 999, 100),
            quote(10, 2, 1),
            quote(11, 9, 2),
            quote(12, 999, 100),
        ];
        assert_eq!(weighted_price(&quotes, 10, 12), U256::from(6));
        let quotes = [quote(10, 2, 0), quote(11, 9, 2_000_000)];
        assert_eq!(weighted_price(&quotes, 10, 12), U256::from(6));
        assert_eq!(scale6("0.000002"), U256::from(2));
        assert_eq!(scale6("1000.000000"), U256::from(1_000_000_000));
        assert_eq!(scale6("1.25"), U256::from(1_250_000));
    }

    #[test]
    #[should_panic(expected = "FORMING interval contains priced observations")]
    fn observations_outside_the_window_cannot_supply_a_price() {
        weighted_price(&[quote(9, 2, 1), quote(12, 9, 2)], 10, 12);
    }

    #[test]
    fn entry_price_uses_previous_utc_day_and_exact_midnight_boundaries() {
        let quotes = [
            quote(86_399, 999_000_000, 100),
            quote(86_400, 100_000_000, 1),
            quote(172_799, 200_000_000, 3),
            quote(172_800, 999_000_000, 100),
        ];
        for prepared_at in [172_800, 200_000, 259_199] {
            assert_eq!(
                previous_day_entry_price(&quotes, prepared_at),
                U256::from(175_000_000)
            );
        }
    }

    #[test]
    #[should_panic(expected = "FORMING interval contains priced observations")]
    fn current_day_quotes_cannot_supply_an_empty_previous_day() {
        previous_day_entry_price(&[quote(172_800, 100_000_000, 1)], 200_000);
    }
}
