//! VaultRouter precompile (`VAULT_ROUTER_ADDRESS`) aka Reserve liquidity router.
//!
//! Registers ERC-4626 vaults per asset and moves funds in and out.

pub mod api;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod settlement;
mod sol_ext;
pub(crate) mod state;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;
pub mod vote_target;

pub use schema::{LiquidityReservation, VaultRouterContract};
pub use vote_target::VaultRouterVoteTarget;

#[cfg(test)]
mod tests;

mod reservation;
