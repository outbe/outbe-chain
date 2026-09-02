//! Real price-feeder acceptance steps over a harness-owned HTTP source.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use cucumber::then;

use crate::world::World;

const USD_ISO: u16 = 840;
const MOCK_PRICE: &str = "1.000000";
const MOCK_VOLUME: &str = "1000.000000";
const EXPECTED_RATE: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
const FX_TTL_SECS: u64 = 21_600;
const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingPricePublication {
    strictly_after_block: u64,
    expected_rate: U256,
    deadline: Instant,
}

#[then("the controlled COEN USD quote is finalized through the real price feeder")]
fn controlled_quote_is_finalized(world: &mut World) {
    let before = world
        .rpc
        .oracle_rate_data(world.validators.primary_port(), USD_ISO)
        .map_or(0, |rate| rate.last_block);
    start_feeder(world);
    wait_for_unanimous_publication(world, before, EXPECTED_RATE, true);
    let evidence = world.price_oracle.evidence_snapshot();
    assert!(evidence.ticker_requests > 0);
    assert!(evidence.candle_requests > 0);
    let quorum = oracle_quorum(world.validators.size());
    assert_eq!(evidence.feeder_pids.len(), quorum);
    assert_eq!(evidence.feeder_logs.len(), quorum);
    let mut distinct_pids = evidence.feeder_pids.clone();
    distinct_pids.sort_unstable();
    distinct_pids.dedup();
    assert_eq!(distinct_pids.len(), quorum);
}

/// Stop the feeder before a controlled-time restart and retain the last
/// finalized publication as the strict post-restart lower bound. Returning
/// `None` keeps scenarios without the feeder unchanged.
pub(crate) fn stop_before_clock_restart(world: &mut World) -> Option<u64> {
    if !world.price_oracle.is_feeder_running() {
        return None;
    }
    world
        .price_oracle
        .ensure_feeder_alive()
        .expect("price feeder stays live before clock restart");
    let previous = world.price_oracle.last_oracle_block().unwrap_or(0);
    world.price_oracle.stop_feeder();
    Some(previous)
}

/// Restart the feeder with reset poll/backoff state and return the strict
/// publication condition that the caller must observe alongside WWD progress.
pub(crate) fn resume_after_clock_restart(
    world: &mut World,
    previous_block: Option<u64>,
) -> Option<PendingPricePublication> {
    let previous_block = previous_block?;
    let expected_rate = feeder_restart_expected_rate(world.price_oracle.read_controlled_quote());
    start_feeder(world);
    Some(PendingPricePublication {
        strictly_after_block: previous_block,
        expected_rate,
        deadline: Instant::now() + PUBLICATION_TIMEOUT,
    })
}

/// Poll one post-restart publication without hiding an irreversible lifecycle
/// transition behind a nested wait. The caller owns the joint WWD/Oracle
/// barrier and stops polling after the first successful observation.
pub(crate) fn observe_pending_publication(
    world: &mut World,
    pending: &PendingPricePublication,
) -> bool {
    if observe_unanimous_publication(
        world,
        pending.strictly_after_block,
        pending.expected_rate,
        true,
    ) {
        return true;
    }
    assert!(
        Instant::now() < pending.deadline,
        "controlled quote did not publish a newer unanimous finalized Oracle rate within {PUBLICATION_TIMEOUT:?} after block {}",
        pending.strictly_after_block,
    );
    false
}

/// Atomically change the harness-owned quote and prove that the production
/// feeder published the new exact scale-6 rate on every validator.
pub(crate) fn publish_controlled_quote(world: &mut World, expected_rate: U256) {
    let strictly_after_block = world.price_oracle.last_oracle_block().unwrap_or(0);
    let quote = scale6_quote(expected_rate);
    world
        .price_oracle
        .publish_quote(&quote, MOCK_VOLUME)
        .expect("publish controlled Oracle quote generation");
    wait_for_unanimous_publication(world, strictly_after_block, expected_rate, true);
}

fn start_feeder(world: &mut World) {
    let (price, volume) = feeder_start_quote(world.price_oracle.read_controlled_quote());
    let validator_count = world.validators.size();
    let quorum = oracle_quorum(validator_count);
    let chain_id = world
        .rpc
        .chain_id(world.validators.primary_port())
        .expect("read feeder chain id");
    let vote_period = world
        .rpc
        .oracle_vote_period(world.validators.primary_port())
        .expect("read canonical Oracle vote period for feeder");

    let feeders = (0..quorum)
        .map(|validator_index| {
            let validator = world.validators.get(validator_index);
            let private_key = validator.evm_key().unwrap_or_else(|error| {
                panic!("validator-{validator_index} EVM key for feeder: {error:#}")
            });
            let validator_address = world
                .rpc
                .address_of(&private_key)
                .unwrap_or_else(|| panic!("derive validator-{validator_index} feeder address"));
            let rpc_url = world.rpc.url(world.validators.http_port(validator_index));
            (validator_index, rpc_url, private_key, validator_address)
        })
        .collect::<Vec<_>>();

    for (validator_index, rpc_url, private_key, validator_address) in feeders {
        world
            .price_oracle
            .start(
                validator_index,
                &rpc_url,
                chain_id,
                &private_key,
                &validator_address,
                crate::world::price_oracle::PriceQuote {
                    price: &price,
                    volume: &volume,
                },
                vote_period,
            )
            .unwrap_or_else(|error| {
                panic!("start validator-{validator_index} production price feeder: {error:#}")
            });
    }
}

fn oracle_quorum(active_validators: usize) -> usize {
    active_validators - active_validators / 3
}

fn feeder_start_quote(current: Option<(String, String)>) -> (String, String) {
    current.unwrap_or_else(|| (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned()))
}

fn feeder_restart_expected_rate(current: Option<(String, String)>) -> U256 {
    let (price, _) = feeder_start_quote(current);
    parse_scale6_rate(&price).unwrap_or_else(|| {
        panic!("controlled feeder quote `{price}` is not a canonical scale-6 rate")
    })
}

fn parse_scale6_rate(price: &str) -> Option<U256> {
    let (whole, fraction) = price.split_once('.').unwrap_or((price, ""));
    if whole.is_empty()
        || fraction.len() > 6
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<U256>().ok()?;
    let mut fraction = fraction.to_owned();
    fraction.extend(std::iter::repeat_n('0', 6 - fraction.len()));
    let fraction = if fraction.is_empty() {
        U256::ZERO
    } else {
        fraction.parse::<U256>().ok()?
    };
    whole
        .checked_mul(U256::from(1_000_000_u64))?
        .checked_add(fraction)
}

fn wait_for_unanimous_publication(
    world: &mut World,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) {
    let deadline = Instant::now() + PUBLICATION_TIMEOUT;
    loop {
        if observe_unanimous_publication(
            world,
            strictly_after_block,
            expected_rate,
            require_live_feeder,
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "controlled quote did not become one fresh finalized Oracle publication: expected_rate={expected_rate} after_block={strictly_after_block}"
        );
        sleep(Duration::from_millis(250));
    }
}

fn observe_unanimous_publication(
    world: &mut World,
    strictly_after_block: u64,
    expected_rate: U256,
    require_live_feeder: bool,
) -> bool {
    if require_live_feeder {
        world
            .price_oracle
            .ensure_feeder_alive()
            .expect("price feeder stays alive until finalized tally");
    }
    let ports = world.validators.committee_ports();
    let finalized = ports
        .iter()
        .map(|port| world.rpc.finalized(*port))
        .collect::<Vec<_>>();
    let rates = ports
        .iter()
        .map(|port| world.rpc.oracle_rate_data(*port, USD_ISO))
        .collect::<Vec<_>>();
    let (Some(finalized_height), Some(rate)) = (
        finalized.iter().flatten().copied().min(),
        rates.first().and_then(|rate| *rate),
    ) else {
        return false;
    };
    if !rates.iter().all(|candidate| *candidate == Some(rate))
        || rate.rate != expected_rate
        || rate.last_block <= strictly_after_block
        || rate.last_block > finalized_height
    {
        return false;
    }
    let timestamps = ports
        .iter()
        .map(|port| world.rpc.block_timestamp(*port, finalized_height))
        .collect::<Vec<_>>();
    let Some(finalized_timestamp) = timestamps.first().and_then(|value| *value) else {
        return false;
    };
    let same_timestamp = timestamps
        .iter()
        .all(|candidate| *candidate == Some(finalized_timestamp));
    let age = finalized_timestamp.saturating_sub(rate.last_timestamp);
    if !same_timestamp || rate.last_timestamp == 0 || age > FX_TTL_SECS {
        return false;
    }
    world.price_oracle.record_canonical_publication(
        ports.len(),
        rate.rate,
        rate.last_block,
        rate.last_timestamp,
        finalized_height,
        finalized_timestamp,
    );
    true
}

fn scale6_quote(rate: U256) -> String {
    let scale = U256::from(1_000_000_u64);
    let whole = rate / scale;
    let fraction = rate % scale;
    format!("{whole}.{fraction:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_quote_uses_the_canonical_six_decimal_scale() {
        assert_eq!(MOCK_PRICE, "1.000000");
        assert_eq!(EXPECTED_RATE, U256::from(1_000_000));
        assert_eq!(FX_TTL_SECS, 6 * 60 * 60);
        assert_eq!(scale6_quote(U256::from(1_080_001_u64)), "1.080001");
        assert_eq!(scale6_quote(U256::from(2_u64)), "0.000002");
    }

    #[test]
    fn feeder_restart_preserves_the_current_controlled_quote() {
        assert_eq!(
            feeder_start_quote(None),
            (MOCK_PRICE.to_owned(), MOCK_VOLUME.to_owned())
        );
        assert_eq!(
            feeder_start_quote(Some(("1.080001".into(), "77.000000".into()))),
            ("1.080001".into(), "77.000000".into())
        );
        assert_eq!(
            feeder_restart_expected_rate(Some(("1.080001".into(), "77.000000".into()))),
            U256::from(1_080_001_u64)
        );
        assert_eq!(feeder_restart_expected_rate(None), EXPECTED_RATE);
    }

    #[test]
    fn process_feeder_count_is_ceiling_two_thirds() {
        let expected = [0usize, 1, 2, 2, 3, 4, 4, 5, 6, 6, 7];
        for (active, expected_quorum) in expected.into_iter().enumerate() {
            assert_eq!(oracle_quorum(active), expected_quorum, "N={active}");
        }
    }
}
