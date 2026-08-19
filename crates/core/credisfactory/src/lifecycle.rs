//! Begin-block void sweep.
//!
//! Walks the credis dense position index with a persisted, bounded cursor and, for
//! every called position whose settlement window has lapsed with principal still
//! outstanding, burns the unpaid share of the pledged collateral, drops the pledger's
//! fidelity cohort, and deposits the equivalent value into the Promis Reserve (see
//! [`crate::runtime::void_position`]).

use outbe_credis::CredisContract;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

use crate::runtime;
use crate::schema::CredisFactoryContract;

pub struct CredisLifecycle;

impl BlockLifecycle for CredisLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        scan_and_void(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Max positions visited per begin-block void scan; the cursor resumes the rest
/// next block so the sweep never scales unboundedly with the position population.
// ponytail: full-index rescan per sweep (O(total_positions) reads amortized across
// blocks). The called-position bin index replaces this once the daily call scan
// lands — a called-and-lapsed position is rare enough that a dense called-only
// index visits a handful of entries instead of the whole book.
pub(crate) const MAX_CREDIS_VOID_SCANS_PER_BLOCK: u64 = 256;

/// Voids the remainder of called-and-lapsed positions in the current cursor window.
/// Returns the number of positions voided this block.
pub fn scan_and_void(ctx: &BlockRuntimeContext) -> Result<u32> {
    let now = ctx.block.timestamp;
    let credis = CredisContract::new(ctx.storage.clone());
    let total = credis.total_positions()?;
    if total == 0 {
        return Ok(0);
    }

    let factory = CredisFactoryContract::new(ctx.storage.clone());
    let mut cursor = factory.void_scan_cursor.read()?;
    if cursor >= total {
        cursor = 0;
    }

    let mut voided: u32 = 0;
    let mut visited: u64 = 0;
    while visited < MAX_CREDIS_VOID_SCANS_PER_BLOCK && visited < total {
        if cursor >= total {
            cursor = 0;
        }
        let position_id = credis.position_id_at(cursor)?;
        let position = credis.get_position(position_id)?;
        if runtime::is_voidable(&position, now)? {
            runtime::void_position(ctx.storage.clone(), position_id)?;
            voided = voided.saturating_add(1);
        }
        cursor += 1;
        visited += 1;
    }

    if cursor >= total {
        cursor = 0;
    }
    factory.void_scan_cursor.write(cursor)?;
    Ok(voided)
}
