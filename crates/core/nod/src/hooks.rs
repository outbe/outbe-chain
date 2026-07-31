//! NOD price-qualifier block hook.
//!
//! Mirrors the Cosmos reference (`x/nod/abci.go::EndBlocker` +
//! `x/nod/keeper/qualification.go::QualifyBucketsByOracleRate`): every
//! block, read the current COEN/0xUSD exchange rate from the oracle and
//! promote any unqualified bucket whose `floor_price_minor < rate`. The
//! comparison is strict — a bucket priced exactly at the rate stays
//! unqualified until the rate moves strictly above its floor.
//! Qualification is a monotonic latch — once a bucket is qualified it stays
//! that way, so `mine_gratis` only has to read the cached `is_qualified` bit.
//!
//! Implementation (PancakeSwap-Liquidity-Book bin index):
//! - `floor_price_minor` is mapped to a 24-bit `bin_id` on a log-spaced
//!   ladder (`BIN_STEP_BP = 25` ⇒ 0.25% per bin) via `state::price_to_bin`.
//! - Unqualified buckets are stored in `unqualified_bin_count` /
//!   `unqualified_bin_buckets`, and a 3-level radix-256 bitmap trie
//!   (`bin_tree_root`/`bin_tree_mid`/`bin_tree_leaf`) marks non-empty bins.
//! - Each block: walk set bins in ascending `bin_id` order via
//!   `bin_tree::find_first_left_inclusive`. Bins strictly below `r_bin` hold
//!   only floors `< rate` (any floor equal to the rate maps into `r_bin`), so
//!   they drain wholesale; the tail bin (`bin_id == r_bin`) checks each
//!   bucket's exact `floor_price_minor < rate` so a coarse bin neither
//!   qualifies a bucket above the rate nor one priced exactly at it.

use alloy_primitives::U256;
use outbe_compressed_entities::{
    EntityId36, ExecutionScope, ParentBodySource, ParentBodySourceRef,
};
use outbe_oracle::api::get_exchange_rate;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
};

use crate::{api, constants::MAX_BUCKET_QUALIFICATIONS_PER_BLOCK, schema::NodContract};

/// Oracle pair that gates bucket qualification: COEN against the stablecoin.
const QUALIFIER_BASE: &str = "COEN";
const QUALIFIER_QUOTE: &str = "0xUSD";

pub struct NodLifecycle;

/// Explicit body authorities required by receipt-visible Nod qualification.
pub struct NodLifecycleContext<'a, 'storage> {
    pub runtime: BlockRuntimeContext<'storage>,
    pub scope: &'a ExecutionScope,
    parent: ParentBodySourceRef<'a>,
}

impl<'a, 'storage> NodLifecycleContext<'a, 'storage> {
    #[must_use]
    pub fn new(
        runtime: BlockRuntimeContext<'storage>,
        scope: &'a ExecutionScope,
        parent: &'a dyn ParentBodySource,
    ) -> Self {
        Self {
            runtime,
            scope,
            parent: ParentBodySourceRef::new(parent),
        }
    }
}

impl BlockLifecycle for NodLifecycle {
    type Context<'a, 'storage> = NodLifecycleContext<'a, 'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &Self::Context<'_, '_>) -> Result<()> {
        qualify_nods(&ctx.runtime, ctx.scope, &ctx.parent)
    }

    fn end_block(_ctx: &Self::Context<'_, '_>) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Qualifies Nod buckets using the same block scope and parent source as transactions.
pub fn qualify_nods(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    let rate = get_exchange_rate(ctx.storage.clone(), QUALIFIER_BASE, QUALIFIER_QUOTE)?;
    qualify_buckets_with_rate(ctx, scope, parent, rate)
}

/// Qualification entry point used by the block executor and behavioral tests.
pub fn qualify_buckets_with_rate(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    rate: U256,
) -> Result<()> {
    if rate.is_zero() {
        return Ok(());
    }
    let r_bin = NodContract::price_to_bin(rate)?;
    let mut nod = NodContract::new(ctx.storage.clone());
    let mut bin_cursor = 0_u32;
    let mut inspected = 0_u32;
    loop {
        if inspected == MAX_BUCKET_QUALIFICATIONS_PER_BLOCK {
            break;
        }
        let next = match tree_math::find_first_left_inclusive(&nod, bin_cursor)? {
            Some(bin) if bin <= r_bin => bin,
            _ => break,
        };
        let strict = next < r_bin;
        let mut count = nod.unqualified_bin_count.read(&next)?;
        if count == 0 {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin tree references empty bin {next}"
                )),
            );
        }
        let mut index = nod.unqualified_bin_scan_cursor.read(&next)?;
        if index >= count {
            index = 0;
        }
        while index < count && inspected < MAX_BUCKET_QUALIFICATIONS_PER_BLOCK {
            let key = NodContract::bin_index_key(next, index);
            let bucket_key = nod.unqualified_bin_buckets.read(&key)?;
            if bucket_key.is_zero() {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {next}:{index} has an empty bucket key"
                    )),
                );
            }
            let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
            let bucket_id = EntityId36::new(worldwide_day, bucket_key.0);
            let loaded =
                api::load_bucket(&ctx.storage, scope, parent, bucket_id)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod unqualified-bin entry {next}:{index} references missing bucket {bucket_id}"
                ))
                })?;
            let bucket = loaded.body();
            if bucket.is_qualified || NodContract::price_to_bin(bucket.floor_price_minor)? != next {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {next}:{index} mismatches bucket {bucket_key}"
                    )),
                );
            }
            if !strict && bucket.floor_price_minor >= rate {
                index += 1;
                inspected += 1;
                continue;
            }
            nod.qualify_bucket_loaded(scope, loaded)?;
            let last = count.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin {next} count underflow"
                ))
            })?;
            if index != last {
                let replacement = nod
                    .unqualified_bin_buckets
                    .read(&NodContract::bin_index_key(next, last))?;
                if replacement.is_zero() {
                    return Err(
                        outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                            "Nod unqualified-bin entry {next}:{last} has an empty bucket key"
                        )),
                    );
                }
                nod.unqualified_bin_buckets.write(&key, replacement)?;
            }
            nod.unqualified_bin_buckets.write(
                &NodContract::bin_index_key(next, last),
                alloy_primitives::B256::ZERO,
            )?;
            count = last;
            inspected += 1;
        }

        nod.unqualified_bin_count.write(&next, count)?;
        if count == 0 {
            nod.unqualified_bin_scan_cursor.write(&next, 0)?;
            nod.unqualified_bin_count.write(&next, 0)?;
            tree_math::remove(&nod, next)?;
        } else if index >= count {
            nod.unqualified_bin_scan_cursor.write(&next, 0)?;
        } else {
            nod.unqualified_bin_scan_cursor.write(&next, index)?;
            break;
        }

        bin_cursor = match next.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => break,
        };
    }
    Ok(())
}
