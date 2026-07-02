//! Public Solidity ABI of the vaultprovider precompile.
//!
//! Other precompiles reach the vault provider via EVM sub-calls to
//! `VAULT_PROVIDER_ADDRESS`, constructing their calldata from these generated
//! call types (e.g. `IVaultProvider::depositLiquidityCall`). The Solidity
//! interface file is the single source of truth; [`crate::precompile`]
//! dispatches the same generated types on the inbound side.

use alloy_sol_types::{sol, SolCall};

sol!("../../../contracts/precompiles/src/IVaultProvider.sol");
