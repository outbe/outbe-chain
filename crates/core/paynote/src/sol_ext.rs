//! Outbound sub-call ABI surfaces.
//!
//! The paynote runtime invokes these interfaces via `StorageHandle::call`. They
//! are not the precompile's own inbound ABI (which lives in
//! `precompile.rs::IPayNote`).

use alloy_sol_types::sol;

sol!("../../../contracts/tokens/src/interfaces/IERC20.sol");
