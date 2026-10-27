//! Check Oracle history and compare entry prices with genesis.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{Address, U256};
use outbe_nod::openings::entry_price_day;
use outbe_primitives::addresses::ORACLE_ADDRESS;
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn previous_day_entry_price(quotes: &[Quote], requested_at: u64) -> U256 {
    let end = requested_at / 86_400 * 86_400;
    let price = weighted_price(quotes, end.saturating_sub(86_400), end);
    assert!(
        !price.is_zero(),
        "entry price requires a positive previous-day VWAP"
    );
    price
}

fn oracle_price_at(world: &World, port: u16, day: u32, iso: u16, height: u64) -> U256 {
    eth::read_call_at_result(
        &world.rpc.url(port),
        ORACLE_ADDRESS,
        &eth::IOracle::getUtcDayVwapCall {
            base: Address::ZERO,
            quote: outbe_primitives::asset_type::currency_address(iso),
            utcDay: day,
        },
        height,
    )
    .expect("historical reference-currency day VWAP")
}

/// The Nod entry price is the VWAP of the UTC day before the Lysis completion block.
pub(super) fn assert_native_entry_price(
    world: &World,
    terms: &outbe_primitives::nod_encryption::NodTermsV1,
    height: u64,
) {
    let ports = world.validators.committee_ports();
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("native entry-price finality");
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("exact native entry-price checkpoint");
    let timestamp = world
        .rpc
        .block_timestamp(ports[0], height)
        .expect("native evaluation time");
    let day = entry_price_day(timestamp).expect("date step");
    let quotes = history(world, ports[0], height);
    let iso = terms.reference_currency;
    let expected = oracle_price_at(world, ports[0], day, iso, 0);
    assert!(
        !expected.is_zero(),
        "genesis contains a positive entry price"
    );
    let declared = match iso {
        840 => U256::from(1_000_000),
        978 => U256::from(250_000),
        _ => panic!("unsupported native fixture reference currency: {iso}"),
    };
    assert_eq!(
        expected, declared,
        "genesis entry price matches the fixture"
    );
    assert_eq!(
        terms.entry_price_minor, expected,
        "entry price matches the closed day in genesis"
    );
    for port in ports {
        assert_eq!(history(world, port, height), quotes);
        assert_eq!(oracle_price_at(world, port, day, iso, 0), expected);
        assert_eq!(oracle_price_at(world, port, day, iso, height), expected);
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("entry-price checkpoint retained"),
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
        for requested_at in [172_800, 200_000, 259_199] {
            assert_eq!(
                previous_day_entry_price(&quotes, requested_at),
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
