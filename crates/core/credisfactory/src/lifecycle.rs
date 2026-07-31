//! Begin-block credis expiry sweep (spec §3.6).
//!
//! Walks the credis dense position index with a persisted, bounded cursor and, for
//! every position past its 10-month term with an unpaid balance, burns the still-
//! locked pledged collateral, drops the pledger's fidelity cohort, and deposits the
//! equivalent value into the Promis Reserve (see [`crate::runtime::expire_position`]).

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
        scan_and_expire(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Max positions visited per begin-block expiry scan; the cursor resumes the rest
/// next block so the sweep never scales unboundedly with the position population.
// ponytail: full-index rescan per sweep (O(total_positions) reads amortized across
// blocks). Add an active/expiry-ordered index or prune closed positions if the
// position count grows large.
pub(crate) const MAX_CREDIS_EXPIRY_SCANS_PER_BLOCK: u64 = 256;

/// Burns the collateral of expired-and-unpaid positions in the current cursor window.
/// Returns the number of positions expired this block.
pub fn scan_and_expire(ctx: &BlockRuntimeContext) -> Result<u32> {
    let now = ctx.block.timestamp;
    let credis = CredisContract::new(ctx.storage.clone());
    let total = credis.total_positions()?;
    if total == 0 {
        return Ok(0);
    }

    let factory = CredisFactoryContract::new(ctx.storage.clone());
    let mut cursor = factory.expiry_scan_cursor.read()?;
    if cursor >= total {
        cursor = 0;
    }

    let mut expired: u32 = 0;
    let mut visited: u64 = 0;
    while visited < MAX_CREDIS_EXPIRY_SCANS_PER_BLOCK && visited < total {
        if cursor >= total {
            cursor = 0;
        }
        let position_id = credis.position_id_at(cursor)?;
        let position = credis.get_position(position_id)?;
        if now >= CredisContract::expires_at(&position)
            && !position.outstanding_anadosis_amount.is_zero()
        {
            runtime::expire_position(ctx.storage.clone(), position_id)?;
            expired = expired.saturating_add(1);
        }
        cursor += 1;
        visited += 1;
    }

    if cursor >= total {
        cursor = 0;
    }
    factory.expiry_scan_cursor.write(cursor)?;
    Ok(expired)
}
