use alloy_primitives::{Address, B256};
use eyre::Result;
use serde::Deserialize;
use std::collections::BTreeSet;

use crate::config::FeederConfig;
use crate::errors::{check, DexError};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DexProviderConfig {
    pub name: String,
    pub chain_id: u64,
    /// Empty takes the `rpc_endpoints` URL for `chain_id`.
    #[serde(default)]
    pub rpc_endpoint: String,
    #[serde(default = "poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "log_chunk")]
    pub log_chunk_blocks: u64,
    /// Blocks behind `latest` to read from. Zero reads the head. A few blocks
    /// keep ordinary short reorgs from reaching the price and volume reads; a
    /// deeper reorg is still detected by hash and rebuilds the volume window.
    #[serde(default = "confirmations")]
    pub confirmations: u64,
    /// Maximum wall-clock age of the block read, including `confirmations`.
    #[serde(default = "block_age", alias = "max_finalized_age_secs")]
    pub max_block_age_secs: u64,
    /// The pair base's and the quote's ERC20 contracts on this network.
    pub(super) base_token: Address,
    pub(super) quote_token: Address,
    pub(super) pool: PoolConfig,
    /// Built from the source that names this entry; see [`collect_markets`].
    #[serde(skip)]
    pub(super) markets: Vec<DexMarketConfig>,
}

fn poll_interval() -> u64 {
    2
}
fn log_chunk() -> u64 {
    2_000
}
fn confirmations() -> u64 {
    3
}
fn block_age() -> u64 {
    1_800
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DexMarketConfig {
    pub base: String,
    pub quote: String,
    pub(super) base_token: Address,
    pub(super) quote_token: Address,
    pub(super) pool: PoolConfig,
}

/// Each variant has its own ABI. Manager pool keys include both token addresses
/// from the enclosing market, sorted in the protocol's currency0/currency1 order.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "protocol", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PoolConfig {
    UniswapV2 {
        address: Address,
    },
    PancakeswapV2 {
        address: Address,
    },
    UniswapV3 {
        address: Address,
    },
    PancakeswapV3 {
        address: Address,
    },
    UniswapV4 {
        manager: Address,
        state_view: Address,
        fee: u32,
        tick_spacing: i32,
        hooks: Address,
    },
    InfinityCl {
        manager: Address,
        fee: u32,
        hooks: Address,
        parameters: B256,
    },
    InfinityBin {
        manager: Address,
        fee: u32,
        hooks: Address,
        parameters: B256,
    },
}

impl DexMarketConfig {
    pub fn key(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }

    pub fn base_is_token0(&self) -> bool {
        self.base_token < self.quote_token
    }

    pub fn tokens(&self) -> (Address, Address) {
        if self.base_is_token0() {
            (self.base_token, self.quote_token)
        } else {
            (self.quote_token, self.base_token)
        }
    }
}

impl DexProviderConfig {
    pub fn validate(&self) -> Result<()> {
        check(
            matches!(
                (self.name.as_str(), self.chain_id),
                ("uniswap", 1) | ("pancakeswap", 56)
            ),
            DexError::UnsupportedChainOrName,
        )?;
        let url = reqwest::Url::parse(&self.rpc_endpoint).map_err(|_| DexError::InvalidRpcUrl)?;
        check(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            DexError::NonHttpRpcUrl,
        )?;
        check(
            (1..=10).contains(&self.poll_interval_secs),
            DexError::InvalidPollInterval,
        )?;
        check(
            (1..=10_000).contains(&self.log_chunk_blocks),
            DexError::InvalidLogChunkBlocks,
        )?;
        check(self.confirmations <= 1_000, DexError::InvalidConfirmations)?;
        check(
            self.max_block_age_secs > 0,
            DexError::NonPositiveMaxBlockAge,
        )?;
        check(
            self.markets.len() == 1,
            DexError::UnattachedPool(self.name.clone()),
        )?;
        let mut markets = BTreeSet::new();
        let mut pools = BTreeSet::new();
        for market in &self.markets {
            check(
                !market.base_token.is_zero()
                    && !market.quote_token.is_zero()
                    && market.base_token != market.quote_token,
                DexError::InvalidTokenAddresses,
            )?;
            check(
                markets.insert(market.key()),
                DexError::DuplicateMarket(market.key()),
            )?;
            market.pool.validate(&self.name)?;
            let id = market.pool.pool_id(market)?;
            check(
                pools.insert((market.pool.address(), id)),
                DexError::DuplicatePool,
            )?;
        }
        Ok(())
    }
}

impl PoolConfig {
    pub fn address(&self) -> Address {
        match *self {
            Self::UniswapV2 { address }
            | Self::PancakeswapV2 { address }
            | Self::UniswapV3 { address }
            | Self::PancakeswapV3 { address } => address,
            Self::UniswapV4 { manager, .. }
            | Self::InfinityCl { manager, .. }
            | Self::InfinityBin { manager, .. } => manager,
        }
    }

    fn validate(&self, provider: &str) -> Result<()> {
        check(!self.address().is_zero(), DexError::ZeroPoolAddress)?;
        let uniswap = matches!(
            self,
            Self::UniswapV2 { .. } | Self::UniswapV3 { .. } | Self::UniswapV4 { .. }
        );
        check(
            uniswap == (provider == "uniswap"),
            DexError::ProtocolMismatch,
        )?;
        match *self {
            Self::UniswapV4 {
                state_view,
                fee,
                tick_spacing,
                ..
            } => {
                check(!state_view.is_zero(), DexError::MissingStateView)?;
                check(
                    (1..=32_767).contains(&tick_spacing),
                    DexError::InvalidV4TickSpacing,
                )?;
                validate_fee(fee)?;
            }
            Self::InfinityCl {
                fee, parameters, ..
            } => {
                validate_fee(fee)?;
                let value = alloy_primitives::U256::from_be_bytes(parameters.0);
                let spacing = (value >> 16) & alloy_primitives::U256::from(0xff_ffffu32);
                check(
                    value >> 40 == alloy_primitives::U256::ZERO
                        && spacing > alloy_primitives::U256::ZERO
                        && spacing <= alloy_primitives::U256::from(32_767u32),
                    DexError::InvalidInfinityClParameters,
                )?;
            }
            Self::InfinityBin {
                fee, parameters, ..
            } => {
                validate_fee(fee)?;
                let value = alloy_primitives::U256::from_be_bytes(parameters.0);
                check(
                    value >> 32 == alloy_primitives::U256::ZERO && self.bin_step() > 0,
                    DexError::InvalidInfinityBinParameters,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn bin_step(&self) -> u16 {
        match self {
            Self::InfinityBin { parameters, .. } => {
                u16::from_be_bytes([parameters[28], parameters[29]])
            }
            _ => 0,
        }
    }
}

fn validate_fee(fee: u32) -> Result<()> {
    check(
        fee <= 1_000_000 || fee == 0x80_0000,
        DexError::InvalidPoolFee,
    )
}

/// Rebuilds every DEX entry's market from the source that names it.
pub(crate) fn collect_markets(config: &mut FeederConfig) {
    for dex in &mut config.dex_providers {
        dex.markets = config
            .currency_pairs
            .iter()
            .flat_map(|pair| &pair.sources)
            .filter(|source| source.provider == dex.name)
            .map(|source| DexMarketConfig {
                base: source.base.clone(),
                quote: source.quote.clone(),
                base_token: dex.base_token,
                quote_token: dex.quote_token,
                pool: dex.pool.clone(),
            })
            .collect();
    }
}

pub(crate) fn validate_config(config: &FeederConfig) -> Result<()> {
    let mut names = BTreeSet::new();
    for dex in &config.dex_providers {
        dex.validate()?;
        check(
            names.insert(dex.name.as_str()),
            DexError::DuplicateDexProvider(dex.name.clone()),
        )?;
        check(
            !config.api_providers.iter().any(|e| e.name == dex.name),
            DexError::ConfiguredInApiProviders,
        )?;
    }
    for pair in &config.currency_pairs {
        for source in &pair.sources {
            if !matches!(source.provider.as_str(), "uniswap" | "pancakeswap") {
                continue;
            }
            check(
                config
                    .dex_providers
                    .iter()
                    .any(|d| d.name == source.provider),
                DexError::MissingDexConfig(source.provider.clone()),
            )?;
        }
    }
    Ok(())
}
