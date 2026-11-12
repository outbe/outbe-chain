use alloy_primitives::{U256, U512};
use async_trait::async_trait;
use eyre::Result;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{sync::RwLock, task::JoinHandle, time::Instant};

use super::{
    config::{DexMarketConfig, DexProviderConfig},
    math,
    pool::Decimals,
};
use crate::errors::{check, DexError};
use crate::fixed::FixedValue;
use crate::provider::evm_rpc::{quantity, Block, Log, Rpc};
use crate::provider::{Provider, TickerPrice};

/// Rolling swap-volume window. One hour matches the exchange providers'
/// 60 one-minute candles so every source weighs on the same scale.
pub(super) const WINDOW_SECS: u64 = 3_600;
/// Window of the time-weighted pool rate: about one vote period. A move of the
/// pool inside one block changes the price of a vote by a small part only, and
/// feeders that read adjacent blocks get almost the same price.
pub(super) const TWAP_WINDOW_SECS: u64 = 120;
const SNAPSHOT_TTL: Duration = Duration::from_secs(30);

struct Snapshot {
    ticker: TickerPrice,
    block_timestamp: u64,
    acquired_at: Instant,
}

pub(crate) struct DexProvider {
    name: String,
    markets: HashMap<String, Arc<RwLock<Option<Snapshot>>>>,
    tasks: Vec<JoinHandle<()>>,
    max_block_age_secs: u64,
}

impl DexProvider {
    pub fn new(config: &DexProviderConfig) -> Result<Self> {
        config.validate()?;
        let runtime = tokio::runtime::Handle::try_current()?;
        let rpc = Rpc::new(&config.rpc_endpoint)?;
        let mut markets = HashMap::new();
        let mut tasks = Vec::new();
        for market in &config.markets {
            let state = Arc::new(RwLock::new(None));
            markets.insert(market.key(), Arc::clone(&state));
            let mut worker = MarketWorker::new(config.clone(), market.clone(), rpc.clone());
            tasks.push(runtime.spawn(async move {
                tracing::info!(provider = %worker.config.name, market = %worker.market.key(), "DEX source warming up");
                loop {
                    let acquired_at = Instant::now();
                    match worker.refresh().await {
                        Ok((block, ticker)) => {
                            tracing::debug!(provider = %worker.config.name, market = %worker.market.key(),
                                block = block.number, hash = %block.hash, "DEX snapshot ready");
                            *state.write().await = Some(Snapshot { ticker, block_timestamp: block.timestamp, acquired_at });
                        }
                        Err(error) => {
                            *state.write().await = None;
                            tracing::warn!(provider = %worker.config.name, market = %worker.market.key(),
                                error = %error, "DEX source unavailable; retrying");
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(worker.config.poll_interval_secs)).await;
                }
            }));
        }
        Ok(Self {
            name: config.name.clone(),
            markets,
            tasks,
            max_block_age_secs: config.max_block_age_secs,
        })
    }
}

impl Drop for DexProvider {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[async_trait]
impl Provider for DexProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn get_ticker_prices(
        &self,
        pairs: &[(String, String)],
    ) -> Result<HashMap<String, TickerPrice>> {
        let mut tickers = HashMap::new();
        for (base, quote) in pairs {
            let key = format!("{base}/{quote}");
            if let Some(state) = self.markets.get(&key) {
                if let Some(snapshot) = state.read().await.as_ref() {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                    if snapshot.acquired_at.elapsed() <= SNAPSHOT_TTL
                        && now.saturating_sub(snapshot.block_timestamp) <= self.max_block_age_secs
                    {
                        tickers.insert(key, snapshot.ticker.clone());
                    }
                }
            }
        }
        Ok(tickers)
    }
}

pub(super) struct MarketWorker {
    config: DexProviderConfig,
    market: DexMarketConfig,
    rpc: Rpc,
    decimals: Option<Decimals>,
    /// Last completely validated log range, even during startup backfill.
    cursor: Option<Block>,
    /// Per-block raw base volume: bounded by the rolling window, not swap count.
    volumes: BTreeMap<u64, (u64, U256)>,
    /// Pool rate at the end of each block, with the block timestamp: the
    /// blocks of the price window and the last block before it.
    rates: BTreeMap<u64, (u64, U256)>,
}

impl MarketWorker {
    pub fn new(config: DexProviderConfig, market: DexMarketConfig, rpc: Rpc) -> Self {
        Self {
            config,
            market,
            rpc,
            decimals: None,
            cursor: None,
            volumes: BTreeMap::new(),
            rates: BTreeMap::new(),
        }
    }

    pub async fn refresh(&mut self) -> Result<(Block, TickerPrice)> {
        check(
            self.rpc.chain_id().await? == self.config.chain_id,
            DexError::ChainIdMismatch,
        )?;
        // Read `confirmations` blocks behind the head, not the finalized tag:
        // finality on Ethereum lags ~13 minutes and moves once per epoch,
        // while a few confirmations already rule out ordinary reorgs.
        let latest = self.rpc.block("latest").await?;
        let head = if self.config.confirmations == 0 {
            latest
        } else {
            self.rpc
                .block_at(latest.number.saturating_sub(self.config.confirmations))
                .await?
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        check(
            head.timestamp <= now.saturating_add(30)
                && now.saturating_sub(head.timestamp) <= self.config.max_block_age_secs,
            DexError::StaleOrFutureBlock,
        )?;
        if let Some(cursor) = &self.cursor {
            let canonical = self.rpc.block_at(cursor.number).await?;
            if canonical != *cursor || head.number < cursor.number {
                self.cursor = None;
                self.volumes.clear();
                self.rates.clear();
                self.decimals = None;
                return Err(DexError::ChainHistoryChanged.into());
            }
        }
        // The hash check above covers the blocks up to the cursor. Read the
        // rates of later blocks again.
        let checked = self.cursor.as_ref().map_or(0, |cursor| cursor.number + 1);
        self.rates.split_off(&checked);
        let decimals = match self.decimals {
            Some(decimals) => decimals,
            None => {
                let decimals = self.market.read_decimals(&self.rpc, &head).await?;
                self.decimals = Some(decimals);
                decimals
            }
        };
        // Validate initialization before doing a potentially long log backfill.
        self.collect_rates(&head, decimals).await?;
        self.collect_volume(&head).await?;
        check(
            self.rpc.block_at(head.number).await? == head,
            DexError::BlockChangedDuringCollection,
        )?;
        let cutoff = head.timestamp.saturating_sub(WINDOW_SECS);
        self.volumes.retain(|_, (timestamp, _)| *timestamp > cutoff);
        let raw = self
            .volumes
            .values()
            .try_fold(U256::ZERO, |sum, (_, amount)| {
                sum.checked_add(*amount)
                    .ok_or(DexError::RollingVolumeOverflow)
            })?;
        let price = self.twap(&head);
        Ok((
            head,
            TickerPrice {
                price,
                volume: math::scale_fp18(raw, decimals.base)?,
            },
        ))
    }

    /// Reads the pool rate of each block that is not in `rates` yet, up to
    /// `head`. The first read starts at the last block before the price window.
    // ponytail: one rate read for each block. A chain with sub-second blocks
    // has hundreds of blocks in the window. If the start becomes too slow
    // there, read only the blocks with swaps.
    async fn collect_rates(&mut self, head: &Block, decimals: Decimals) -> Result<()> {
        let cutoff = head.timestamp.saturating_sub(TWAP_WINDOW_SECS);
        // After a gap longer than the window the stored rates are of no use.
        if matches!(self.rates.last_key_value(), Some((_, (timestamp, _))) if *timestamp <= cutoff)
        {
            self.rates.clear();
        }
        let last = self.rates.last_key_value().map(|(number, _)| *number);
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
            let previous = self.rpc.block_at(oldest.number - 1).await?;
            blocks.push(previous);
        }
        for block in blocks.iter().rev() {
            if last.is_some_and(|last| block.number <= last) {
                continue;
            }
            let rate = self.market.read_rate(&self.rpc, block, decimals).await?;
            self.rates
                .insert(block.number, (block.timestamp, rate.raw()));
        }
        // Keep the window and the last block before it: its rate applies at
        // the start of the window.
        let first = self
            .rates
            .iter()
            .rev()
            .find(|(_, (timestamp, _))| *timestamp <= cutoff)
            .map(|(number, _)| *number);
        if let Some(first) = first {
            self.rates = self.rates.split_off(&first);
        }
        Ok(())
    }

    /// Time-weighted pool rate of the `TWAP_WINDOW_SECS` before `head`. The
    /// rate of a block applies from its timestamp to the timestamp of the next
    /// block, so the rate of `head` has no weight yet.
    fn twap(&self, head: &Block) -> FixedValue {
        let cutoff = head.timestamp.saturating_sub(TWAP_WINDOW_SECS);
        let mut sum = U512::ZERO;
        let mut total = 0u64;
        for (&(start, rate), &(end, _)) in self.rates.values().zip(self.rates.values().skip(1)) {
            let seconds = end.saturating_sub(start.max(cutoff));
            sum += U512::from(rate) * U512::from(seconds);
            total += seconds;
        }
        if total == 0 {
            // No earlier block: the chain is as old as `head`.
            let rate = self.rates.values().next_back().map_or(U256::ZERO, |r| r.1);
            return FixedValue::from_raw(rate);
        }
        FixedValue::from_raw((sum / U512::from(total)).to::<U256>())
    }

    async fn collect_volume(&mut self, head: &Block) -> Result<()> {
        let cutoff = head.timestamp.saturating_sub(WINDOW_SECS);
        let mut from = if let Some(cursor) = &self.cursor {
            if cursor.number == head.number {
                return Ok(());
            }
            cursor
                .number
                .checked_add(1)
                .ok_or(DexError::BlockNumberOverflow)?
        } else {
            self.rpc.first_after(cutoff, head).await?
        };
        let topics = self.market.log_topics()?;
        let mut chunk_size = self.config.log_chunk_blocks;
        while from <= head.number {
            let to = from.saturating_add(chunk_size - 1).min(head.number);
            let logs = match self
                .rpc
                .logs(self.market.pool.address(), &topics, from, to)
                .await
            {
                Ok(logs) => logs,
                Err(_) if chunk_size > 1 => {
                    chunk_size = (chunk_size / 2).max(1);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let end = self.rpc.block_at(to).await?;
            check(
                end.timestamp <= head.timestamp,
                DexError::LogRangePastHeadTime,
            )?;
            let buckets = self.bucket_chunk(logs, from, &end, head).await?;
            // Commit only after validating the entire chunk. Retried requests
            // never append a partially processed chunk or count its logs twice.
            self.volumes.extend(buckets);
            self.volumes.retain(|_, (timestamp, _)| *timestamp > cutoff);
            self.cursor = Some(end);
            if to == head.number {
                break;
            }
            from = to + 1;
        }
        Ok(())
    }

    /// Validates one chunk of Swap logs in `[from, end.number]` against the
    /// canonical chain and sums the base-token amount per block.
    async fn bucket_chunk(
        &self,
        logs: Vec<Log>,
        from: u64,
        end: &Block,
        head: &Block,
    ) -> Result<BTreeMap<u64, (u64, U256)>> {
        let to = end.number;
        let mut seen = BTreeMap::new();
        let mut buckets: BTreeMap<u64, (u64, U256)> = BTreeMap::new();
        let mut blocks: BTreeMap<u64, Block> = BTreeMap::new();
        blocks.insert(to, end.clone());
        for log in logs {
            let number = quantity(&log.block_number)?;
            let index = quantity(&log.log_index)?;
            check((from..=to).contains(&number), DexError::LogOutsideRange)?;
            let amount = self.market.swap_volume(&log)?;
            // A block's log index is globally unique, including singleton
            // managers. Identical duplicates are harmless; conflicts fail.
            let identity = (log.block_hash, index);
            let value = (log.transaction_hash, log.data.clone(), log.topics.clone());
            if let Some(previous) = seen.insert(identity, value.clone()) {
                check(previous == value, DexError::ConflictingDuplicateLog)?;
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(entry) = blocks.entry(number) {
                entry.insert(self.rpc.block_at(number).await?);
            }
            let block = blocks.get(&number).ok_or(DexError::MissingLogBlock)?;
            check(
                block.hash == log.block_hash && block.timestamp <= head.timestamp,
                DexError::LogNotCanonical,
            )?;
            let bucket = buckets
                .entry(number)
                .or_insert((block.timestamp, U256::ZERO));
            bucket.1 = bucket
                .1
                .checked_add(amount)
                .ok_or(DexError::BlockVolumeOverflow)?;
        }
        Ok(buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ticker_cache_expires_from_acquisition_and_block_time() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let state = Arc::new(RwLock::new(Some(Snapshot {
            ticker: TickerPrice {
                price: FixedValue::parse("1").unwrap(),
                volume: FixedValue::ZERO,
            },
            block_timestamp: now,
            acquired_at: Instant::now(),
        })));
        let provider = DexProvider {
            name: "uniswap".into(),
            markets: HashMap::from([("COEN/USDC".into(), state.clone())]),
            tasks: vec![],
            max_block_age_secs: 1800,
        };
        let pairs = [("COEN".into(), "USDC".into())];
        assert_eq!(provider.get_ticker_prices(&pairs).await.unwrap().len(), 1);
        state.write().await.as_mut().unwrap().acquired_at =
            Instant::now() - Duration::from_secs(31);
        assert!(provider.get_ticker_prices(&pairs).await.unwrap().is_empty());
        {
            let mut guard = state.write().await;
            let snapshot = guard.as_mut().unwrap();
            snapshot.acquired_at = Instant::now();
            snapshot.block_timestamp = now - 1801;
        }
        assert!(provider.get_ticker_prices(&pairs).await.unwrap().is_empty());
    }
}
