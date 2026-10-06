//! Outbound sub-call ABI surfaces.
//!
//! The Hyperlane contract interfaces that the controller calls through
//! `StorageHandle::call` / `StorageHandle::staticcall`. This is NOT the
//! precompile's own inbound ABI (`precompile::IHyperlaneController`).

use alloy_sol_types::sol;

sol!("../../../contracts/crosschain/src/interfaces/IHyperlaneCore.sol");
