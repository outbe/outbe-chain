//! Close a lifecycle pricing window over hours the real feeders covered.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolError;
use eyre::{bail, ensure, eyre, Result};
use outbe_oracle::window::VwapSnapshotId;
use outbe_primitives::addresses::ORACLE_ADDRESS;

use super::super::chain::{finalized_checkpoint, poll_until, verify_checkpoint};
use super::WINDOW_CLOSE_MARGIN_SECS;
use crate::internal::eth;
use crate::internal::pricing_coverage::{
    first_stepped_hour, pricing_step, prospective_end, step_limit, PricingStep, HOUR,
    MIN_HOUR_SNAPSHOTS, RESTART_ROUNDS,
};
use crate::world::World;

const HISTORY_LIMIT: u32 = 4_096;
const ENTER_TIMEOUT: Duration = Duration::from_secs(300);
const DWELL_POLL: Duration = Duration::from_secs(2);
enum ClosedWindowPrice {
    Value(U256),
    Reverted(Bytes),
}

/// Step through covered hours until the production Oracle prices the window for every currency.
pub(super) fn close(world: &mut World, currencies: &[u16]) -> Result<()> {
    let started = Instant::now();
    let checkpoint = finalized_checkpoint(world);
    let port = world.validators.primary_port();
    let head_time = world
        .rpc
        .block_timestamp(port, checkpoint.height)
        .ok_or_else(|| eyre!("pricing checkpoint timestamp unavailable"))?;
    let (lookback, period) = window_policy(&world.rpc.url(port), checkpoint.height)?;
    verify_checkpoint(world, checkpoint);
    let mut tail = RESTART_ROUNDS * period;
    let mut left_at = (checkpoint.height, head_time);
    let mut hour = first_stepped_hour(head_time, lookback);
    let mut history = Vec::new();
    for step in 0..=step_limit(lookback) {
        let height = enter_hour(world, hour)?;
        let left_hour_end = left_at.1 - left_at.1 % HOUR + HOUR;
        let next_first = first_block_at_or_after(world, port, height, left_hour_end)?;
        tail = tail.max(next_first.saturating_sub(left_at.0));
        if step > 0 && closed_window_is_priced(world, currencies, hour)? {
            eprintln!(
                "pricing_window evidence=closed_window_priced cutoff={hour} steps={step} tail={tail} elapsed_secs={}",
                started.elapsed().as_secs()
            );
            return Ok(());
        }
        if step == step_limit(lookback) {
            break;
        }
        let (decision, decided) = dwell(world, currencies, tail)?;
        if decision == PricingStep::Close {
            eprintln!(
                "pricing_window evidence=close_predicted cutoff={} tail={tail}",
                decided.1 - decided.1 % HOUR + HOUR
            );
        }
        history.push(format!(
            "hour={hour} decided={decision:?} height={} tail={tail}",
            decided.0
        ));
        left_at = decided;
        hour = decided.1 - decided.1 % HOUR + HOUR;
    }
    bail!(
        "no covered pricing window for {currencies:?} within {} hours: {}",
        step_limit(lookback),
        history.join("; ")
    )
}

/// Restart the committee just inside `hour` and return the first finalized height in it.
fn enter_hour(world: &mut World, hour: u64) -> Result<u64> {
    let (_, pending) = crate::features::common::restart_committee_at_logical_time(
        world,
        hour + WINDOW_CLOSE_MARGIN_SECS,
    );
    let port = world.validators.primary_port();
    let mut publication_ready = pending.is_none();
    let enter_timeout = pending.as_ref().map_or(ENTER_TIMEOUT, |pending| {
        ENTER_TIMEOUT.max(pending.remaining_wait())
    });
    poll_until(
        enter_timeout,
        || format!("the committee or its feeders did not reach the pricing hour {hour}"),
        || {
            publication_ready = publication_ready
                || pending.as_ref().is_some_and(|pending| {
                    crate::features::price_oracle::observe_pending_publication(world, pending)
                });
            publication_ready
                && world
                    .rpc
                    .latest_block_timestamp(port)
                    .is_some_and(|now| now >= hour)
        },
    );
    let deadline = Instant::now() + ENTER_TIMEOUT;
    loop {
        let checkpoint = finalized_checkpoint(world);
        let time = world
            .rpc
            .block_timestamp(port, checkpoint.height)
            .ok_or_else(|| eyre!("pricing checkpoint timestamp unavailable"))?;
        verify_checkpoint(world, checkpoint);
        if time >= hour {
            return Ok(checkpoint.height);
        }
        ensure!(
            Instant::now() < deadline,
            "finality did not reach the pricing hour {hour}"
        );
        sleep(DWELL_POLL);
    }
}

/// Returns the planner's decision with the height and time it was made at.
fn dwell(world: &mut World, currencies: &[u16], tail: u64) -> Result<(PricingStep, (u64, u64))> {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let checkpoint = finalized_checkpoint(world);
    let initial = ProspectiveWindow::at(world, checkpoint.height, tail)?;
    verify_checkpoint(world, checkpoint);
    let genesis = std::fs::read(world.localnet.scenario_dir().join("genesis.json"))?;
    let genesis = serde_json::from_slice(&genesis)?;
    let timeout = dwell_wait_budget(&genesis, initial.period, tail)?
        .min(Duration::from_secs(initial.cutoff - initial.time));
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| eyre!("pricing dwell deadline overflow"))?;
    loop {
        world.price_oracle.ensure_cohort_feeders_alive()?;
        let checkpoint = finalized_checkpoint(world);
        let window = ProspectiveWindow::at(world, checkpoint.height, tail)?;
        ensure!(
            window.time < initial.cutoff,
            "pricing hour {} rolled over before coverage was established",
            initial.cutoff - HOUR,
        );
        let counts = currencies
            .iter()
            .map(|&currency| window.observations(&url, currency, checkpoint.height))
            .collect::<Result<Vec<_>>>()?;
        let decision = pricing_step(&window.bounds, &counts, window.period);
        eprintln!(
            "pricing_window evidence=dwell height={} bounds={:?} observations={counts:?} tail={tail} decision={decision:?}",
            checkpoint.height, window.bounds
        );
        verify_checkpoint(world, checkpoint);
        if decision != PricingStep::Dwell {
            return Ok((decision, (checkpoint.height, window.time)));
        }
        ensure!(
            Instant::now() < deadline,
            "the pricing hour at height {} stayed uncovered for {currencies:?}",
            checkpoint.height
        );
        sleep(DWELL_POLL);
    }
}

/// Budget ten real publications plus the measured restart tail from executed timing.
fn dwell_wait_budget(genesis: &serde_json::Value, period: u64, tail: u64) -> Result<Duration> {
    ensure!(period > 0, "Oracle vote period must be positive");
    let blocks = MIN_HOUR_SNAPSHOTS
        .checked_mul(period)
        .and_then(|blocks| blocks.checked_add(tail))
        .ok_or_else(|| eyre!("pricing dwell block budget overflow"))?;
    crate::features::common::consensus_block_wait_budget(genesis, blocks)
}

struct ProspectiveWindow {
    time: u64,
    start: u64,
    cutoff: u64,
    period: u64,
    bounds: Vec<u64>,
}

impl ProspectiveWindow {
    fn at(world: &World, height: u64, tail: u64) -> Result<Self> {
        let port = world.validators.primary_port();
        let time = world
            .rpc
            .block_timestamp(port, height)
            .ok_or_else(|| eyre!("pricing checkpoint timestamp unavailable"))?;
        let (lookback, period) = window_policy(&world.rpc.url(port), height)?;
        let cutoff = time - time % HOUR + HOUR;
        let start = cutoff
            .checked_sub(lookback)
            .ok_or_else(|| eyre!("pricing cutoff precedes lookback"))?;
        let mut bounds = hour_bounds(world, port, height, start..cutoff)?;
        bounds.push(prospective_end(height, tail));
        Ok(Self {
            time,
            start,
            cutoff,
            period,
            bounds,
        })
    }

    fn observations(&self, url: &str, currency: u16, height: u64) -> Result<Vec<u64>> {
        let history = eth::read_call_at_result(
            url,
            ORACLE_ADDRESS,
            &eth::IOracle::getPriceSnapshotHistoryCall {
                base: Address::ZERO,
                quote: outbe_primitives::asset_type::currency_address(currency),
                count: HISTORY_LIMIT,
            },
            height,
        )
        .map_err(|error| eyre!(error))?;
        ensure!(
            history.timestamps.len() == history.rates.len()
                && history.timestamps.len() == history.volumes.len()
                && history.timestamps.windows(2).all(|pair| pair[0] >= pair[1])
                && history
                    .timestamps
                    .iter()
                    .all(|timestamp| *timestamp <= self.time),
            "inconsistent pinned Oracle history"
        );
        ensure!(
            history.timestamps.len() < HISTORY_LIMIT as usize
                || history
                    .timestamps
                    .last()
                    .is_some_and(|timestamp| *timestamp < self.start),
            "bounded Oracle history cannot establish full window coverage"
        );
        Ok((self.start..self.cutoff)
            .step_by(HOUR as usize)
            .map(|hour| {
                history
                    .timestamps
                    .iter()
                    .filter(|timestamp| **timestamp >= hour && **timestamp < hour + HOUR)
                    .count() as u64
            })
            .collect())
    }
}

fn window_policy(url: &str, height: u64) -> Result<(u64, u64)> {
    let policy = eth::read_call_at_result(
        url,
        ORACLE_ADDRESS,
        &eth::IOracle::getVwapPolicyCall {},
        height,
    )
    .map_err(|error| eyre!(error))?;
    ensure!(
        policy.vwapUpdateIntervalSeconds == HOUR
            && policy.vwapLookbackSeconds > 0
            && policy.vwapLookbackSeconds.is_multiple_of(HOUR)
            && policy.vwapLookbackSeconds <= 24 * HOUR,
        "lifecycle fixture requires a bounded hourly Oracle window"
    );
    let params =
        eth::read_call_at_result(url, ORACLE_ADDRESS, &eth::IOracle::getParamsCall {}, height)
            .map_err(|error| eyre!(error))?;
    ensure!(params.votePeriod > 0, "Oracle vote period must be positive");
    Ok((policy.vwapLookbackSeconds, params.votePeriod))
}

fn hour_bounds(
    world: &World,
    port: u16,
    height: u64,
    hours: std::ops::Range<u64>,
) -> Result<Vec<u64>> {
    let mut bounds = Vec::new();
    for hour in hours.step_by(HOUR as usize) {
        let first = first_block_at_or_after(world, port, height, hour)?;
        let block = if first <= height
            && world
                .rpc
                .block_timestamp(port, first)
                .ok_or_else(|| eyre!("hour block unavailable"))?
                < hour + HOUR
        {
            first
        } else {
            0
        };
        bounds.push(block);
    }
    Ok(bounds)
}

/// Genesis is excluded: Oracle records the first executed block of each hour.
fn first_block_at_or_after(world: &World, port: u16, height: u64, time: u64) -> Result<u64> {
    let (mut low, mut high) = (1, height + 1);
    while low < high {
        let middle = low + (high - low) / 2;
        let timestamp = world
            .rpc
            .block_timestamp(port, middle)
            .ok_or_else(|| eyre!("canonical pricing header {middle} unavailable"))?;
        if timestamp < time {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    Ok(low)
}

fn closed_window_is_priced(world: &World, currencies: &[u16], cutoff: u64) -> Result<bool> {
    let checkpoint = finalized_checkpoint(world);
    let mut ready = true;
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        let snapshot = eth::read_call_at_result(
            &url,
            ORACLE_ADDRESS,
            &eth::IOracle::getVwapSnapshotIdCall {},
            checkpoint.height,
        )
        .map_err(|error| eyre!(error))?;
        let pinned = VwapSnapshotId::from_u256(snapshot).map_err(|error| eyre!("{error:?}"))?;
        ensure!(
            pinned.cutoff() == cutoff,
            "port {port} pins the window closing at {} instead of {cutoff}",
            pinned.cutoff()
        );
        for &currency in currencies {
            let call = eth::IOracle::getFinalizedWindowVwapCall {
                currency,
                snapshotId: snapshot,
            };
            let outcome =
                match eth::read_call_at_result(&url, ORACLE_ADDRESS, &call, checkpoint.height) {
                    Ok(price) => ClosedWindowPrice::Value(price),
                    Err(error) => {
                        let data = eth::read_call_revert_data_at(
                            &url,
                            ORACLE_ADDRESS,
                            Address::ZERO,
                            &call,
                            eth::RevertAt {
                                value: U256::ZERO,
                                height: checkpoint.height,
                            },
                        )
                        .map_err(|revert| {
                            eyre!("closed Oracle view failed ({error}); revert data: {revert}")
                        })?;
                        ClosedWindowPrice::Reverted(data)
                    }
                };
            ready &= price_is_ready(outcome)?;
        }
    }
    verify_checkpoint(world, checkpoint);
    Ok(ready)
}

/// Only the exact canonical `NoVwapData` revert permits a new warmed cutoff.
fn price_is_ready(outcome: ClosedWindowPrice) -> Result<bool> {
    match outcome {
        ClosedWindowPrice::Value(price) => {
            ensure!(!price.is_zero(), "closed Oracle price must be positive");
            Ok(true)
        }
        ClosedWindowPrice::Reverted(reason) => {
            let expected = eth::IOracle::NoVwapData {}.abi_encode();
            ensure!(
                reason.as_ref() == expected.as_slice(),
                "unexpected closed Oracle revert: 0x{}",
                alloy_primitives::hex::encode(reason)
            );
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_fixture_budget_uses_executed_timing_and_restart_tail() {
        let genesis = serde_json::json!({"config": {
            "minBlockTimeMs": 2000,
            "leaderTimeoutMs": 15000,
            "certificationTimeoutMs": 30000,
        }});
        assert_eq!(
            dwell_wait_budget(&genesis, 8, 16).unwrap(),
            Duration::from_secs(2880)
        );
        assert_eq!(
            dwell_wait_budget(&genesis, 8, 24).unwrap(),
            Duration::from_secs(3120)
        );
        assert!(dwell_wait_budget(&genesis, 0, 16).is_err());
        assert!(dwell_wait_budget(&genesis, u64::MAX, 16).is_err());
        assert!(dwell_wait_budget(&genesis, 8, u64::MAX).is_err());
    }

    #[test]
    fn accepts_only_a_positive_production_price() {
        assert!(price_is_ready(ClosedWindowPrice::Value(U256::ONE)).unwrap());
        assert!(price_is_ready(ClosedWindowPrice::Value(U256::ZERO)).is_err());
    }

    #[test]
    fn only_exact_no_vwap_data_allows_another_cutoff() {
        let missing = Bytes::from(eth::IOracle::NoVwapData {}.abi_encode());
        assert!(!price_is_ready(ClosedWindowPrice::Reverted(missing)).unwrap());
        for error in [
            eth::IOracle::InvalidVwapSnapshot {}.abi_encode(),
            eth::IOracle::NoTwapData {}.abi_encode(),
            Bytes::from_static(&[0x11, 0x22, 0x33, 0x44]).to_vec(),
        ] {
            assert!(price_is_ready(ClosedWindowPrice::Reverted(Bytes::from(error))).is_err());
        }
    }
}
