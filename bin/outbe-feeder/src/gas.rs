//! Gas votes: the gas price of each remote chain in COEN-wei per unit of gas.
//!
//! For each `[[gas_providers]]` entry the feeder reads `eth_feeHistory` on that
//! chain and the native token price in USD from a Chainlink feed on the same
//! chain. Then it converts the gas price to COEN with the COEN/840 price of
//! the same vote. Each native token has 18 decimals, as COEN.

use alloy_primitives::{Address, U256, U512};
use eyre::{ensure, eyre, Result};
use outbe_primitives::asset_type::{currency_address, COEN_ASSET};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::aggregator::AggregatedPrice;
use crate::config::FeederConfig;
use crate::provider::dex::math::scale_fp18;
use crate::provider::evm_rpc::Rpc;
use crate::provider::onchain_feed::{AggregatorV3, MAX_FEED_AGE_SECS};

/// Blocks of fee history in one gas price when the entry gives none.
const DEFAULT_FEE_HISTORY_BLOCKS: u16 = 20;
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

/// The gas votes of this vote. A chain that fails gets no vote, and the
/// Oracle keeps its last price.
pub async fn gas_votes(config: &FeederConfig, prices: &[AggregatedPrice]) -> Vec<(u32, U256)> {
    if config.gas_providers.is_empty() {
        return Vec::new();
    }
    let usd = currency_address(840);
    let Some(coen_usd) = prices
        .iter()
        .find(|price| price.base == COEN_ASSET && price.quote == usd)
        .map(|price| price.price)
    else {
        warn!("no COEN/840 price in this vote; no gas votes");
        return Vec::new();
    };
    let mut votes = Vec::with_capacity(config.gas_providers.len());
    for entry in &config.gas_providers {
        match gas_vote(config, entry, coen_usd).await {
            Ok(price) => votes.push((entry.chain_id, price)),
            Err(error) => warn!(chain_id = entry.chain_id, %error, "no gas vote for chain"),
        }
    }
    votes
}

async fn gas_vote(
    config: &FeederConfig,
    entry: &GasProviderConfig,
    coen_usd: U256,
) -> Result<U256> {
    let endpoint = config
        .rpc_endpoints
        .get(&entry.chain_id.to_string())
        .ok_or_else(|| eyre!("no rpc_endpoints entry for chain {}", entry.chain_id))?;
    let rpc = Rpc::new(endpoint)?;
    let history = rpc
        .request(
            "eth_feeHistory",
            json!([
                format!("{:#x}", entry.fee_history_blocks),
                "latest",
                [PRIORITY_PERCENTILE]
            ]),
        )
        .await?;
    let gas_wei = fee_history_gas_price(&history)?;
    let native_usd = match entry.price_feed {
        PriceFeed::Chainlink(feed) => chainlink_price(&rpc, feed).await?,
    };
    gas_price_coen(gas_wei, native_usd, coen_usd)
}

/// The answer of a Chainlink feed at 18 decimals. Rejects a non-positive or
/// stale round.
async fn chainlink_price(rpc: &Rpc, feed: Address) -> Result<U256> {
    let decimals = rpc.call_latest(feed, AggregatorV3::decimalsCall {}).await?;
    let round = rpc
        .call_latest(feed, AggregatorV3::latestRoundDataCall {})
        .await?;
    ensure!(
        round.answer.is_positive(),
        "price feed answer is not positive"
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    ensure!(
        U256::from(now).saturating_sub(round.updatedAt) <= U256::from(MAX_FEED_AGE_SECS),
        "price feed round is stale"
    );
    Ok(scale_fp18(round.answer.into_raw(), decimals)?.raw())
}

/// The larger of `median(baseFeePerGas) + median(reward)` and the latest
/// base fee plus the latest reward of an `eth_feeHistory` result. The median
/// makes a fall slow. The latest block makes a rise count at once, so a
/// short spike does not leave the relayer to pay the difference.
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
    let latest = match (base_fees.last(), rewards.last()) {
        (Some(base_fee), Some(reward)) => *base_fee + *reward,
        _ => return Err(eyre!("eth_feeHistory returned no blocks")),
    };
    Ok((median(base_fees)? + median(rewards)?).max(latest))
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
    fn a_gas_entry_names_its_price_feed_and_defaults_to_20_blocks() {
        let entry: GasProviderConfig = toml::from_str(
            r#"
            chain_id = 56
            price_feed = { chainlink = "0x0567F2323251f0Aab15c8dFb1967E4e8A7D42aeE" }
            "#,
        )
        .unwrap();
        assert!(matches!(entry.price_feed, PriceFeed::Chainlink(_)));
        assert_eq!(entry.fee_history_blocks, 20);
    }

    #[test]
    fn a_rise_in_the_latest_block_counts_at_once() {
        // Medians 1 + 1 gwei, latest block 9 + 1 gwei.
        let history = json!({
            "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00", "0x218711a00"],
            "reward": [["0x3b9aca00"], ["0x3b9aca00"]],
        });
        assert_eq!(
            fee_history_gas_price(&history).unwrap(),
            U256::from(10_000_000_000u64)
        );
    }
}
