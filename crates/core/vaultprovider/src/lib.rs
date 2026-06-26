//! VaultProvider precompile (`VAULT_PROVIDER_ADDRESS`). Reserve liquidity
//! router rewritten from the Solidity `VaultProvider` contract.
//!
//! Registers ERC-4626 vaults per asset, tracks authorized liquidity
//! **sources** (NodCostPrice, IntexStrikePrice, CredisAnadosis, IntexBidPrice,
//! GemSettle) and **targets** (Credis), and moves funds in and out of the
//! configured vaults on their behalf:
//!
//! - `addVault` / `removeVault` (owner-only) register the vault for its
//!   underlying asset and manage the provider's ERC-20 allowance to it.
//! - `addLiquiditySource|Target` / `removeLiquiditySource|Target` (owner-only)
//!   maintain the source/target authorization sets.
//! - `depositLiquidity` (source-gated) pulls assets from the caller and
//!   deposits them into the asset's vault.
//! - `withdrawLiquidity` (target-gated) redeems shares and tops the assets up
//!   into the receiver token-bundle.
//!
//! Ownership replaces the Solidity `OwnableUpgradeable`: the owner is a single
//! storage slot seeded at genesis (see `scripts/seed_genesis.py`).

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

pub use schema::VaultProviderContract;

#[cfg(test)]
mod tests;
