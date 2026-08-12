//! Outbound sub-call ABI surfaces.
//!
//! External contract interfaces the nodfactory runtime invokes via
//! `StorageHandle::call`. NOT the precompile's own inbound ABI (which lives
//! in `precompile.rs::INodFactory`).

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");
