//! Block-lifecycle entrypoints for the gratisfactory precompile.
//!
//! This does NOT run as a begin-block hook. Returning assets to the vault is an
//! `IVaultV2.deposit` sub-call, and the block-hook provider
//! (`DirectStorageProvider`) does not implement `StorageProvider::sub_call` — the
//! trait default rejects with `NotAvailable`. It runs from the Cycle trigger
//! instead, which dispatches inside the `CycleTick` system transaction and so has a
//! real EVM frame.

use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::runtime::{sweep_expired, MAX_PLEDGE_EXPIRY_SWEEPS};

/// Cycle trigger entry point. Total by construction — a handler that returns `Err`
/// propagates out of `dispatch_triggers` and fails the block.
pub fn run_sweep(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_expired(&ctx.storage, MAX_PLEDGE_EXPIRY_SWEEPS)?;
    Ok(())
}
