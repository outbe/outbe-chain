//! Warm real feeder observations before closing a lifecycle pricing window.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, U256};
use eyre::{ensure, eyre, Result};
use outbe_primitives::addresses::ORACLE_ADDRESS;

use super::super::chain::{finalized_checkpoint, poll_until, verify_checkpoint};
use super::WINDOW_CLOSE_MARGIN_SECS;
use crate::internal::{eth, pricing_coverage::pricing_coverage_ready};
use crate::world::World;

const HOUR: u64 = 3_600;
const HISTORY_LIMIT: u32 = 4_096;
const WARM_TIMEOUT: Duration = Duration::from_secs(900);
const MIN_HOUR_SNAPSHOTS: u64 = 10;
/// Conservative stop/import margin; the actual closed Oracle call still decides.
const RESTART_ROUNDS: u64 = 2;
const NO_VWAP_DATA: &str = "no VWAP data in the requested time range";

/// Start a fresh hour after the window that contains blocks without feeder votes.
pub(super) fn prepare_fresh_hour(world: &mut World) -> Result<()> {
    let checkpoint = finalized_checkpoint(world);
    let port = world.validators.primary_port();
    let time = world
        .rpc
        .block_timestamp(port, checkpoint.height)
        .ok_or_else(|| eyre!("pricing checkpoint timestamp unavailable"))?;
    let (lookback, _) = window_policy(&world.rpc.url(port), checkpoint.height)?;
    let hour = time - time % HOUR + HOUR + lookback;
    verify_checkpoint(world, checkpoint);
    let (_, _, _, pending) = crate::features::ocomp::restart_committee_at_logical_time(
        world,
        hour + WINDOW_CLOSE_MARGIN_SECS,
    );
    let mut publication_ready = pending.is_none();
    poll_until(
        WARM_TIMEOUT,
        || format!("committee or feeders did not reach the covered hour {hour}"),
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
    Ok(())
}

pub(super) fn wait_for_coverage(world: &mut World, currencies: &[u16]) -> Result<u64> {
    let deadline = Instant::now() + WARM_TIMEOUT;
    loop {
        world.price_oracle.ensure_cohort_feeders_alive()?;
        if let Some(cutoff) = prospective_coverage(world, currencies)? {
            return Ok(cutoff);
        }
        ensure!(
            Instant::now() < deadline,
            "insufficient real feeder coverage for {currencies:?}"
        );
        sleep(Duration::from_secs(1));
    }
}

fn prospective_coverage(world: &World, currencies: &[u16]) -> Result<Option<u64>> {
    let checkpoint = finalized_checkpoint(world);
    let window = ProspectiveWindow::at(world, checkpoint.height)?;
    let url = world.rpc.url(world.validators.primary_port());
    let mut ready = true;
    for &currency in currencies {
        let counts = window.observations(&url, currency, checkpoint.height)?;
        ready &= counts
            .last()
            .is_some_and(|count| *count >= MIN_HOUR_SNAPSHOTS)
            && pricing_coverage_ready(&window.bounds, &counts, window.period);
        eprintln!("pricing_window evidence=public_coverage height={} currency={currency} bounds={:?} observations={counts:?} ready={ready}", checkpoint.height, window.bounds);
    }
    verify_checkpoint(world, checkpoint);
    Ok(ready.then_some(window.cutoff))
}

struct ProspectiveWindow {
    time: u64,
    start: u64,
    cutoff: u64,
    period: u64,
    bounds: Vec<u64>,
}

impl ProspectiveWindow {
    fn at(world: &World, height: u64) -> Result<Self> {
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
        bounds.push(height + RESTART_ROUNDS * period + 1);
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

pub(super) fn closed_window_is_priced(world: &World, currencies: &[u16]) -> Result<bool> {
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
        for &currency in currencies {
            let outcome = eth::read_call_at_with_revert_reason(
                &url,
                ORACLE_ADDRESS,
                &eth::IOracle::getFinalizedWindowVwapCall {
                    currency,
                    snapshotId: snapshot,
                },
                checkpoint.height,
            )?;
            ready &= price_is_ready(outcome)?;
        }
    }
    verify_checkpoint(world, checkpoint);
    Ok(ready)
}

/// Only the exact canonical NoVwapData revert permits a new warmed cutoff.
fn price_is_ready(outcome: eth::ViewCallOutcome<U256>) -> Result<bool> {
    match outcome {
        eth::ViewCallOutcome::Value(price) => {
            ensure!(!price.is_zero(), "closed Oracle price must be positive");
            Ok(true)
        }
        eth::ViewCallOutcome::Reverted(reason) => {
            ensure!(
                reason == NO_VWAP_DATA,
                "unexpected closed Oracle revert: {reason}"
            );
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_a_positive_production_price() {
        assert!(price_is_ready(eth::ViewCallOutcome::Value(U256::ONE)).unwrap());
        assert!(price_is_ready(eth::ViewCallOutcome::Value(U256::ZERO)).is_err());
    }

    #[test]
    fn only_exact_no_vwap_data_allows_another_cutoff() {
        assert!(!price_is_ready(eth::ViewCallOutcome::Reverted(NO_VWAP_DATA.into())).unwrap());
        for error in [
            "invalid VWAP snapshot",
            "body read unavailable",
            "no VWAP data in another range",
        ] {
            assert!(price_is_ready(eth::ViewCallOutcome::Reverted(error.into())).is_err());
        }
    }
}
