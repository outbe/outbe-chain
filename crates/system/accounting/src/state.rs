//! Local storage helpers around [`crate::schema::Accounting`].
//!
//! These helpers are the sanctioned mutation surface for slot 0 (INV4).
//! They stay crate-private. The crate root exposes writes only through
//! [`crate::runtime`]. The schema field is public.
//! Another crate can write the slot through that field.
//! These helpers do not enforce the single-writer rule.

use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;

use crate::schema::Accounting;

/// Reads `last_accounted_block_number` from EVM storage.
///
/// Returns `0` on a fresh chain that has not yet committed any V2 Phase 1
/// (the underlying EVM slot defaults to `U256::ZERO`).
pub(crate) fn last_accounted_block_number(ctx: &BlockRuntimeContext) -> Result<u64> {
    let accounting: Accounting<'_> = ctx.storage.contract::<Accounting<'_>>();
    accounting.last_accounted_block_number.read()
}

/// Writes `last_accounted_block_number` to EVM storage. The V2 executor
/// Phase 1 path is the only intended caller.
///
/// The function takes a `BlockRuntimeContext`, not a raw `StorageHandle`.
/// Thus the storage scope stays bound to the same block whose Phase 1 is
/// committing. This prevents accidental cross-block writes.
pub(crate) fn set_last_accounted_block_number(
    ctx: &BlockRuntimeContext,
    block_number: u64,
) -> Result<()> {
    let accounting: Accounting<'_> = ctx.storage.contract::<Accounting<'_>>();
    accounting.last_accounted_block_number.write(block_number)
}
