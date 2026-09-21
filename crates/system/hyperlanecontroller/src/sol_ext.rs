//! Outbound sub-call ABI surfaces.
//!
//! The Hyperlane contract interfaces the controller invokes via
//! `StorageHandle::call` / `StorageHandle::staticcall`. NOT the precompile's
//! own inbound ABI (`precompile::IHyperlaneController`).

use alloy_sol_types::sol;

sol!("../../../contracts/crosschain/src/interfaces/IHyperlaneCore.sol");
