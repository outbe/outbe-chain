//! Time-weighted pool rate of the last `TWAP_WINDOW_SECS`.

use alloy_primitives::{U256, U512};
use eyre::Result;
use std::collections::BTreeMap;

use super::{config::DexMarketConfig, pool::Decimals};
use crate::fixed::FixedValue;
use crate::provider::evm_rpc::{Block, Rpc};

/// Window of the time-weighted pool rate: about one vote period. A move of the
/// pool inside one block changes the price of a vote by a small part only, and
/// feeders that read adjacent blocks get almost the same price.
pub(super) const TWAP_WINDOW_SECS: u64 = 120;

/// Pool rate at the end of each block, with the block timestamp: the blocks of
/// the price window and the last block before it.
#[derive(Default)]
pub(super) struct RateWindow(BTreeMap<u64, (u64, U256)>);

impl RateWindow {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Drops the rates of block `number` and of later blocks.
    pub fn drop_from(&mut self, number: u64) {
        self.0.split_off(&number);
    }

    /// Reads the pool rate of each block that is not stored yet, up to `head`.
    /// The first read starts at the last block before the price window.
    // ponytail: one rate read for each block. A chain with sub-second blocks
    // has hundreds of blocks in the window. If the start becomes too slow
    // there, read only the blocks with swaps.
    pub async fn collect(
        &mut self,
        rpc: &Rpc,
        market: &DexMarketConfig,
        head: &Block,
        decimals: Decimals,
    ) -> Result<()> {
        let cutoff = head.timestamp.saturating_sub(TWAP_WINDOW_SECS);
        // After a gap longer than the window the stored rates are of no use.
        if matches!(self.0.last_key_value(), Some((_, (timestamp, _))) if *timestamp <= cutoff) {
            self.0.clear();
        }
        let last = self.0.last_key_value().map(|(number, _)| *number);
        // Go back from `head` to the block after the last stored block, or to
        // the last block before the window when no rate is stored.
        let mut blocks = vec![head.clone()];
        loop {
            let oldest = &blocks[blocks.len() - 1];
            let complete = match last {
                Some(last) => oldest.number <= last + 1,
                None => oldest.timestamp <= cutoff || oldest.number == 0,
            };
            if complete {
                break;
            }
            let previous = rpc.block_at(oldest.number - 1).await?;
            blocks.push(previous);
        }
        for block in blocks.iter().rev() {
            if last.is_some_and(|last| block.number <= last) {
                continue;
            }
            let rate = market.read_rate(rpc, block, decimals).await?;
            self.0.insert(block.number, (block.timestamp, rate.raw()));
        }
        // Keep the window and the last block before it: its rate applies at
        // the start of the window.
        let first = self
            .0
            .iter()
            .rev()
            .find(|(_, (timestamp, _))| *timestamp <= cutoff)
            .map(|(number, _)| *number);
        if let Some(first) = first {
            self.0 = self.0.split_off(&first);
        }
        Ok(())
    }

    /// Time-weighted pool rate of the `TWAP_WINDOW_SECS` before `head`. The
    /// rate of a block applies from its timestamp to the timestamp of the next
    /// block, so the rate of `head` has no weight yet.
    pub fn mean(&self, head: &Block) -> FixedValue {
        let cutoff = head.timestamp.saturating_sub(TWAP_WINDOW_SECS);
        let mut sum = U512::ZERO;
        let mut total = 0u64;
        for (&(start, rate), &(end, _)) in self.0.values().zip(self.0.values().skip(1)) {
            let seconds = end.saturating_sub(start.max(cutoff));
            sum += U512::from(rate) * U512::from(seconds);
            total += seconds;
        }
        if total == 0 {
            // No earlier block: the chain is as old as `head`.
            let rate = self.0.values().next_back().map_or(U256::ZERO, |r| r.1);
            return FixedValue::from_raw(rate);
        }
        FixedValue::from_raw((sum / U512::from(total)).to::<U256>())
    }
}
