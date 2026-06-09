//! PancakeSwap Liquidity Book math port.
//!
//! File layout mirrors `pancakeswap/infinity-core@main` so the port stays
//! auditable line-for-line:
//! - [`constants`] — `src/pool-bin/libraries/Constants.sol` + `PriceHelper.sol`.
//! - [`bit_math`] — `src/pool-bin/libraries/math/BitMath.sol`.
//! - [`uint256x256_math`] — `src/pool-bin/libraries/math/Uint256x256Math.sol`.
//! - [`uint128x128_math`] — `src/pool-bin/libraries/math/Uint128x128Math.sol`.
//! - [`price_helper`] — `src/pool-bin/libraries/PriceHelper.sol`.
//! - [`tree_math`] — `src/pool-bin/libraries/math/TreeMath.sol` (decoupled
//!   from any specific contract via the [`tree_math::BinTreeStorage`] trait).

pub mod bit_math;
pub mod constants;
pub mod price_helper;
pub mod tree_math;
pub mod uint128x128_math;
pub mod uint256x256_math;
