use alloy_primitives::U256;
use outbe_oracle::api::coen_rate_for;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
};

use crate::constants::QUALIFIER_REFERENCE_ISO;
use crate::schema::GemContract;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // TODO refactor this. Oracle is called here for each block
        scan_and_qualify(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    // No pair or no published rate: skip this block's scan rather than halt it.
    let Some(rate) = coen_rate_for(ctx.storage.clone(), QUALIFIER_REFERENCE_ISO)? else {
        return Ok(0);
    };
    if rate.is_zero() {
        return Ok(0);
    }

    let now = ctx.block.timestamp;
    let r_bin = GemContract::price_to_bin(rate)?;
    let mut gem = GemContract::new(ctx.storage.clone());

    let mut promoted: u32 = 0;
    let mut cursor: u32 = 0;
    loop {
        let next = match tree_math::find_first_left_inclusive(&gem, cursor)? {
            Some(b) if b <= r_bin => b,
            _ => break,
        };

        // Snapshot the bin's gem_ids before mutating; qualify() calls
        // remove_unqualified() on success which shifts entries in storage.
        let count = gem.unqualified_bin_count.read(&next)?;
        let mut bin_gems: Vec<U256> = Vec::with_capacity(count as usize);
        for i in 0..count {
            let id = gem
                .unqualified_bin_gems
                .read(&GemContract::bin_index_key(next, i))?;
            if !id.is_zero() {
                bin_gems.push(id);
            }
        }

        for gem_id in bin_gems {
            if gem.qualify(gem_id, now, rate)? {
                promoted = promoted.saturating_add(1);
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => break,
        };
    }
    Ok(promoted)
}
