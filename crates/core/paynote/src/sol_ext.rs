//! ABI interfaces for outbound sub-calls.
//!
//! The PayNote runtime calls these interfaces through `StorageHandle::call`.
//! The inbound ABI of the precompile is not here. It is in
//! `precompile.rs::IPayNote`.

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");
