use alloy_primitives::{
    aliases::{I24, U24},
    keccak256, B256, U256, U512,
};
use alloy_sol_types::{SolEvent, SolValue};
use eyre::Result;

use super::{
    abi::*,
    config::{DexMarketConfig, PoolConfig},
    math,
};
use crate::errors::DexError;
use crate::fixed::FixedValue;
use crate::provider::evm_rpc::{Block, Log, Rpc};

#[derive(Clone, Copy)]
pub(super) struct Decimals {
    pub base: u8,
    pub quote: u8,
}

impl PoolConfig {
    pub(super) fn pool_id(&self, market: &DexMarketConfig) -> Result<Option<B256>> {
        let (currency0, currency1) = market.tokens();
        Ok(match *self {
            Self::UniswapV4 {
                fee,
                tick_spacing,
                hooks,
                ..
            } => Some(keccak256(
                UniPoolKey {
                    currency0,
                    currency1,
                    fee: U24::try_from(fee)?,
                    tickSpacing: I24::try_from(tick_spacing)?,
                    hooks,
                }
                .abi_encode(),
            )),
            Self::InfinityCl {
                manager,
                fee,
                hooks,
                parameters,
            }
            | Self::InfinityBin {
                manager,
                fee,
                hooks,
                parameters,
            } => Some(keccak256(
                InfinityPoolKey {
                    currency0,
                    currency1,
                    hooks,
                    poolManager: manager,
                    fee: U24::try_from(fee)?,
                    parameters,
                }
                .abi_encode(),
            )),
            _ => None,
        })
    }

    pub(super) fn swap_topic(&self) -> B256 {
        match self {
            Self::UniswapV2 { .. } | Self::PancakeswapV2 { .. } => Pair::Swap::SIGNATURE_HASH,
            Self::UniswapV3 { .. } => UniV3::Swap::SIGNATURE_HASH,
            Self::PancakeswapV3 { .. } => PancakeV3::Swap::SIGNATURE_HASH,
            Self::UniswapV4 { .. } => UniV4::Swap::SIGNATURE_HASH,
            Self::InfinityCl { .. } => InfinityCl::Swap::SIGNATURE_HASH,
            Self::InfinityBin { .. } => InfinityBin::Swap::SIGNATURE_HASH,
        }
    }
}

impl DexMarketConfig {
    pub(super) async fn read_decimals(&self, rpc: &Rpc, block: &Block) -> Result<Decimals> {
        match self.pool {
            PoolConfig::UniswapV4 {
                manager,
                state_view,
                ..
            } => {
                if rpc
                    .call(state_view, StateView::poolManagerCall {}, block)
                    .await?
                    != manager
                {
                    return Err(DexError::StateViewManagerMismatch.into());
                }
            }
            PoolConfig::InfinityCl { .. } | PoolConfig::InfinityBin { .. } => {}
            _ => {
                let address = self.pool.address();
                let token0 = rpc.call(address, Pair::token0Call {}, block).await?;
                let token1 = rpc.call(address, Pair::token1Call {}, block).await?;
                if (token0, token1) != self.tokens() {
                    return Err(DexError::PoolTokenMismatch.into());
                }
            }
        }
        let base = rpc
            .call(self.base_token, Token::decimalsCall {}, block)
            .await?;
        let quote = rpc
            .call(self.quote_token, Token::decimalsCall {}, block)
            .await?;
        if base > 77 || quote > 77 {
            return Err(DexError::UnsupportedDexTokenDecimals.into());
        }
        Ok(Decimals { base, quote })
    }

    pub(super) async fn read_rate(
        &self,
        rpc: &Rpc,
        block: &Block,
        decimals: Decimals,
    ) -> Result<FixedValue> {
        let id = self.pool.pool_id(self)?.unwrap_or_default();
        let (numerator, denominator) = match self.pool {
            PoolConfig::UniswapV2 { address } | PoolConfig::PancakeswapV2 { address } => {
                let state = rpc.call(address, Pair::getReservesCall {}, block).await?;
                (U512::from(state.reserve1), U512::from(state.reserve0))
            }
            PoolConfig::UniswapV3 { address } => {
                let state = rpc.call(address, UniV3::slot0Call {}, block).await?;
                sqrt_ratio(U256::from(state.sqrtPriceX96))
            }
            PoolConfig::PancakeswapV3 { address } => {
                let state = rpc.call(address, PancakeV3::slot0Call {}, block).await?;
                sqrt_ratio(U256::from(state.sqrtPriceX96))
            }
            PoolConfig::UniswapV4 { state_view, .. } => {
                let state = rpc
                    .call(state_view, StateView::getSlot0Call { poolId: id }, block)
                    .await?;
                sqrt_ratio(U256::from(state.sqrtPriceX96))
            }
            PoolConfig::InfinityCl { manager, .. } => {
                let state = rpc
                    .call(manager, InfinityCl::getSlot0Call { poolId: id }, block)
                    .await?;
                sqrt_ratio(U256::from(state.sqrtPriceX96))
            }
            PoolConfig::InfinityBin { manager, .. } => {
                let state = rpc
                    .call(manager, InfinityBin::getSlot0Call { poolId: id }, block)
                    .await?;
                // activeId is uint24, so conversion to u32 is lossless.
                (
                    U512::from(math::bin_price(
                        state.activeId.to::<u32>(),
                        self.pool.bin_step(),
                    )?),
                    U512::ONE << 128,
                )
            }
        };
        let (n, d) = if self.base_is_token0() {
            (numerator, denominator)
        } else {
            (denominator, numerator)
        };
        math::rate(n, d, decimals.base, decimals.quote)
    }

    pub(super) fn log_topics(&self) -> Result<Vec<B256>> {
        let mut topics = vec![self.pool.swap_topic()];
        if let Some(id) = self.pool.pool_id(self)? {
            topics.push(id);
        }
        Ok(topics)
    }

    /// Absolute base-token balance delta, once for each core Swap event. A V2 flash
    /// swap can have both input and output. Use its net delta, not double turnover.
    pub(super) fn swap_volume(&self, log: &Log) -> Result<U256> {
        if log.removed || log.address != self.pool.address() {
            return Err(DexError::InvalidLogAddressOrRemoved.into());
        }
        if log.topics.len() != 3 || log.topics[0] != self.pool.swap_topic() {
            return Err(DexError::InvalidSwapTopics.into());
        }
        if let Some(id) = self.pool.pool_id(self)? {
            if log.topics[1] != id {
                return Err(DexError::SwapPoolIdMismatch.into());
            }
        }
        self.base_delta(log)
    }

    /// Decodes one Swap event for the pool's protocol and returns the
    /// absolute base-token amount it moved.
    fn base_delta(&self, log: &Log) -> Result<U256> {
        let topics = || log.topics.iter().copied();
        // The base token's side of the event: amount0 when base is token0.
        let pick = |amount0: U256, amount1: U256| {
            if self.base_is_token0() {
                amount0
            } else {
                amount1
            }
        };
        Ok(match self.pool {
            PoolConfig::UniswapV2 { .. } | PoolConfig::PancakeswapV2 { .. } => {
                let e = Pair::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(e.amount0In, e.amount1In).abs_diff(pick(e.amount0Out, e.amount1Out))
            }
            PoolConfig::UniswapV3 { .. } => {
                let e = UniV3::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(e.amount0.unsigned_abs(), e.amount1.unsigned_abs())
            }
            PoolConfig::PancakeswapV3 { .. } => {
                let e = PancakeV3::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(e.amount0.unsigned_abs(), e.amount1.unsigned_abs())
            }
            PoolConfig::UniswapV4 { .. } => {
                let e = UniV4::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(
                    U256::from(e.amount0.unsigned_abs()),
                    U256::from(e.amount1.unsigned_abs()),
                )
            }
            PoolConfig::InfinityCl { .. } => {
                let e = InfinityCl::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(
                    U256::from(e.amount0.unsigned_abs()),
                    U256::from(e.amount1.unsigned_abs()),
                )
            }
            PoolConfig::InfinityBin { .. } => {
                let e = InfinityBin::Swap::decode_raw_log_validate(topics(), &log.data)?;
                pick(
                    U256::from(e.amount0.unsigned_abs()),
                    U256::from(e.amount1.unsigned_abs()),
                )
            }
        })
    }
}

fn sqrt_ratio(sqrt: U256) -> (U512, U512) {
    // sqrt is decoded from uint160; its square fits in 320 bits.
    (U512::from(sqrt) * U512::from(sqrt), U512::ONE << 192)
}
