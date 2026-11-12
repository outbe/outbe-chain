//! Gas votes: the gas price of each remote chain in COEN-wei per unit of gas.
//!
//! For each `[[gas_providers]]` entry the feeder reads `eth_feeHistory` on that
//! chain and the native token price in USD from a Chainlink feed on the same
//! chain. Then it converts the gas price to COEN with the COEN/840 price of
//! the same vote. Each native token has 18 decimals, as COEN.
//!
//! All feeders read the same blocks: the blocks up to the last block that is
//! `delay_secs` older than the start of the Outbe vote period. So the time of
//! a vote inside the period does not change the gas price.

use alloy_primitives::{Address, U256, U512};
use eyre::{ensure, eyre, Result};
use outbe_system_common::asset_type::{currency_address, COEN_ASSET};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

use crate::aggregator::AggregatedPrice;
use crate::config::FeederConfig;
use crate::provider::dex::math::scale_fp18;
use crate::provider::evm_rpc::{Block, Rpc};
use crate::provider::onchain_feed::{AggregatorV3, MAX_FEED_AGE_SECS};

/// Blocks of fee history in one gas price when the entry gives none.
const DEFAULT_FEE_HISTORY_BLOCKS: u16 = 20;
/// `delay_secs` when the entry gives none.
const DEFAULT_DELAY_SECS: u64 = 60;
/// Percentile of the priority fees in each block.
const PRIORITY_PERCENTILE: f64 = 50.0;
/// COEN/840 has six decimals. The conversion works at 18.
const COEN_ISO_TO_FP18: u64 = 1_000_000_000_000;

/// One remote chain to vote a gas price for.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasProviderConfig {
    /// Chain id of the remote chain. Its RPC comes from `rpc_endpoints`.
    pub chain_id: u32,
    /// Source of the native token price in USD.
    pub price_feed: PriceFeed,
    /// Blocks of fee history in one gas price, about 2 × the vote period
    /// divided by the block time of the chain.
    #[serde(default = "default_fee_history_blocks")]
    pub fee_history_blocks: u16,
    /// The last block read is this many seconds older than the start of the
    /// vote period, so that each RPC has it and it does not change. Keep it
    /// above the block time of the chain.
    #[serde(default = "default_delay_secs")]
    pub delay_secs: u64,
}

/// Source of the native token price in USD, for example
/// `price_feed = { chainlink = "0x…" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum PriceFeed {
    /// Chainlink `<NATIVE>/USD` feed contract on the same chain.
    Chainlink(Address),
}

fn default_fee_history_blocks() -> u16 {
    DEFAULT_FEE_HISTORY_BLOCKS
}

fn default_delay_secs() -> u64 {
    DEFAULT_DELAY_SECS
}

/// The gas votes of this vote. A chain that fails gets no vote, and the
/// Oracle keeps its last price.
/// `period_start` is the timestamp of the first block of the Outbe vote period.
pub async fn gas_votes(
    config: &FeederConfig,
    prices: &[AggregatedPrice],
    period_start: u64,
) -> Vec<(u32, U256)> {
    let usd = currency_address(840);
    let Some(coen_usd) = prices
        .iter()
        .find(|price| price.base == COEN_ASSET && price.quote == usd)
        .map(|price| price.price)
    else {
        warn!("no COEN/840 price in this vote; no gas votes");
        return Vec::new();
    };
    // The chains are read in parallel: the time is that of the slowest chain.
    let reads = config.gas_providers.iter().map(|entry| async move {
        let Some(endpoint) = config.rpc_endpoints.get(&entry.chain_id.to_string()) else {
            warn!(
                chain_id = entry.chain_id,
                "no rpc_endpoints entry; no gas vote for chain"
            );
            return None;
        };
        match gas_vote(endpoint, entry, coen_usd, period_start).await {
            Ok(price) => Some((entry.chain_id, price)),
            Err(error) => {
                warn!(chain_id = entry.chain_id, %error, "no gas vote for chain");
                None
            }
        }
    });
    futures::future::join_all(reads)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// The gas price of `entry` in COEN-wei per unit of gas, read over `endpoint`
/// at the anchor block of `period_start`.
async fn gas_vote(
    endpoint: &str,
    entry: &GasProviderConfig,
    coen_usd: U256,
    period_start: u64,
) -> Result<U256> {
    let rpc = Rpc::new(endpoint)?;
    let block = anchor_block(&rpc, period_start.saturating_sub(entry.delay_secs)).await?;
    let history = rpc
        .request(
            "eth_feeHistory",
            json!([
                format!("{:#x}", entry.fee_history_blocks),
                format!("{:#x}", block.number),
                [PRIORITY_PERCENTILE]
            ]),
        )
        .await?;
    let gas_wei = fee_history_gas_price(&history)?;
    let native_usd = match entry.price_feed {
        PriceFeed::Chainlink(feed) => chainlink_price(&rpc, feed, &block).await?,
    };
    gas_price_coen(gas_wei, native_usd, coen_usd)
}

/// The last block with a timestamp at or before `cutoff`. Fails when the RPC
/// has no newer block yet, because then it can miss the block that other
/// feeders read.
async fn anchor_block(rpc: &Rpc, cutoff: u64) -> Result<Block> {
    let head = rpc.block("latest").await?;
    ensure!(head.timestamp > cutoff, "the RPC is behind the anchor time");
    let first_after = rpc.first_after(cutoff, &head).await?;
    ensure!(first_after > 0, "no block before the anchor time");
    rpc.block_at(first_after - 1).await
}

/// The answer of a Chainlink feed at `block`, at 18 decimals. Rejects a
/// non-positive answer or a round that is stale at `block`.
async fn chainlink_price(rpc: &Rpc, feed: Address, block: &Block) -> Result<U256> {
    let decimals = rpc.call(feed, AggregatorV3::decimalsCall {}, block).await?;
    let round = rpc
        .call(feed, AggregatorV3::latestRoundDataCall {}, block)
        .await?;
    ensure!(
        round.answer.is_positive(),
        "price feed answer is not positive"
    );
    ensure!(
        U256::from(block.timestamp).saturating_sub(round.updatedAt)
            <= U256::from(MAX_FEED_AGE_SECS),
        "price feed round is stale"
    );
    Ok(scale_fp18(round.answer.into_raw(), decimals)?.raw())
}

/// `median(baseFeePerGas) + median(reward)` of an `eth_feeHistory` result.
/// Feeders read at different blocks, and the gas price of one block moves
/// much. The median keeps their votes inside the gas reward band.
///
/// The reward median skips zero rewards: on BSC each block has a system
/// transaction with a gas price of 0, so a quiet block has a p50 of 0.
/// A gas price of 0 is an error, not a vote.
fn fee_history_gas_price(history: &Value) -> Result<U256> {
    let base_fees = quantities(history["baseFeePerGas"].as_array())?;
    let rewards = quantities(
        history["reward"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block.get(0))
                    .cloned()
                    .collect()
            })
            .as_ref(),
    )?;
    let rewards: Vec<U256> = rewards
        .into_iter()
        .filter(|reward| !reward.is_zero())
        .collect();
    let reward = if rewards.is_empty() {
        U256::ZERO
    } else {
        median(rewards)?
    };
    let price = median(base_fees)? + reward;
    ensure!(!price.is_zero(), "eth_feeHistory gives a gas price of 0");
    Ok(price)
}

fn quantities(values: Option<&Vec<Value>>) -> Result<Vec<U256>> {
    values
        .ok_or_else(|| eyre!("eth_feeHistory result without the expected arrays"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| eyre!("eth_feeHistory value is not a quantity"))?
                .parse::<U256>()
                .map_err(Into::into)
        })
        .collect()
}

/// The middle value, or the floored midpoint of the two middle values.
fn median(mut values: Vec<U256>) -> Result<U256> {
    ensure!(!values.is_empty(), "eth_feeHistory returned no blocks");
    values.sort_unstable();
    let upper = values.len() / 2;
    if values.len() % 2 == 1 {
        return Ok(values[upper]);
    }
    let lower = values[upper - 1];
    Ok(lower + (values[upper] - lower) / U256::from(2))
}

/// `gas_wei × native/USD ÷ COEN/USD`, with `native_usd` at 18 decimals and
/// `coen_usd` at the six decimals of COEN/840.
fn gas_price_coen(gas_wei: U256, native_usd: U256, coen_usd: U256) -> Result<U256> {
    ensure!(!coen_usd.is_zero(), "COEN/840 price is zero");
    let price = U512::from(gas_wei) * U512::from(native_usd)
        / (U512::from(coen_usd) * U512::from(COEN_ISO_TO_FP18));
    ensure!(
        price <= U512::from(U256::MAX),
        "gas price does not fit in 256 bits"
    );
    Ok(price.to::<U256>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gas_price_in_coen_comes_from_the_fee_history_and_the_prices() {
        // Base fees 1, 3 and 2 gwei (median 2), priority fees 1 and 0.5 gwei
        // (median 0.75): 2.75 gwei. BNB at $600, COEN at $0.50.
        let history = json!({
            "baseFeePerGas": ["0x3b9aca00", "0xb2d05e00", "0x77359400"],
            "reward": [["0x3b9aca00"], ["0x1dcd6500"]],
        });
        let gas_wei = fee_history_gas_price(&history).unwrap();
        assert_eq!(gas_wei, U256::from(2_750_000_000u64));

        let bnb_usd = U256::from(600) * U256::from(10).pow(U256::from(18));
        let coen_usd = U256::from(500_000);
        assert_eq!(
            gas_price_coen(gas_wei, bnb_usd, coen_usd).unwrap(),
            U256::from(3_300_000_000_000u64)
        );
    }

    #[test]
    fn the_reward_median_skips_zero_rewards() {
        // BSC: base fee 0, rewards 0 (system transactions), 1 and 3 gwei.
        let history = json!({
            "baseFeePerGas": ["0x0", "0x0", "0x0", "0x0"],
            "reward": [["0x0"], ["0x0"], ["0x3b9aca00"], ["0xb2d05e00"]],
        });
        assert_eq!(
            fee_history_gas_price(&history).unwrap(),
            U256::from(2_000_000_000u64)
        );
    }

    #[test]
    fn a_gas_entry_names_its_price_feed_and_has_defaults() {
        let entry: GasProviderConfig = toml::from_str(
            r#"
            chain_id = 56
            price_feed = { chainlink = "0x0567F2323251f0Aab15c8dFb1967E4e8A7D42aeE" }
            "#,
        )
        .unwrap();
        assert!(matches!(entry.price_feed, PriceFeed::Chainlink(_)));
        assert_eq!(entry.fee_history_blocks, 20);
        assert_eq!(entry.delay_secs, 60);
    }

    /// Reads the gas price of real chains over public RPCs, at COEN = $0.50.
    /// Two runs in the same 120-second period print the same prices:
    /// `cargo test -p outbe-feeder live_gas_votes -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_gas_votes() {
        let chains = [
            (
                1,
                "https://ethereum-rpc.publicnode.com",
                "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419",
            ),
            (
                11155111,
                "https://ethereum-sepolia-rpc.publicnode.com",
                "0x694AA1769357215DE4FAC081bf1f309aDC325306",
            ),
            (
                97,
                "https://bsc-testnet-rpc.publicnode.com",
                "0x2514895c72f50D8bd4B4F9b1110F0D6bD2c97526",
            ),
        ];
        for (chain_id, endpoint, feed) in chains {
            let entry: GasProviderConfig = toml::from_str(&format!(
                "chain_id = {chain_id}\nprice_feed = {{ chainlink = \"{feed}\" }}"
            ))
            .unwrap();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            // The start of a 120-second vote period, as on Outbe.
            let period_start = now / 120 * 120;
            let price = gas_vote(endpoint, &entry, U256::from(500_000), period_start).await;
            println!("chain {chain_id}: {price:?} COEN-wei per gas");
        }
    }
}
