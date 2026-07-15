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
use outbe_oracle::api::get_exchange_rate;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
};

use crate::{schema::NodContract, NodRepositoryReader};

/// Oracle pair that gates bucket qualification: COEN against the stablecoin.
const QUALIFIER_BASE: &str = "COEN";
const QUALIFIER_QUOTE: &str = "0xUSD";

pub struct NodLifecycle;

impl BlockLifecycle for NodLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        #[cfg(not(any(test, feature = "test-utils")))]
        {
            let _ = ctx;
            Err(outbe_primitives::error::PrecompileError::Fatal(
                "Nod lifecycle read authority was not supplied".into(),
            ))
        }

        #[cfg(any(test, feature = "test-utils"))]
        // TODO refactor this. Oracle is called here for each block,
        //  but we only need to receive the hook if the price is changed
        qualify_nods(ctx)
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub fn qualify_nods(ctx: &BlockRuntimeContext) -> Result<()> {
    let rate = get_exchange_rate(ctx.storage.clone(), QUALIFIER_BASE, QUALIFIER_QUOTE)?;
    qualify_buckets_with_rate(ctx, rate)
}

#[cfg(any(test, feature = "test-utils"))]
pub fn qualify_buckets_with_rate(ctx: &BlockRuntimeContext, rate: U256) -> Result<()> {
    if rate.is_zero() {
        return Ok(());
    }
    let r_bin = NodContract::price_to_bin(rate)?;

    let mut nod = NodContract::new(ctx.storage.clone());
    let mut cursor: u32 = 0;
    loop {
        let next = match tree_math::find_first_left_inclusive(&nod, cursor)? {
            Some(b) if b <= r_bin => b,
            _ => break,
        };
        let strict = next < r_bin;
        let count = nod.unqualified_bin_count.read(&next)?;

        // Read the bin's bucket_keys and partition into (qualified, survivors).
        let mut survivors: Vec<alloy_primitives::B256> = Vec::new();
        for i in 0..count {
            let key = NodContract::bin_index_key(next, i);
            let bucket_key = nod.unqualified_bin_buckets.read(&key)?;
            if bucket_key.is_zero() {
                continue;
            }
            // Skip stale entries: the bucket may have been deleted (last
            // NOD mined) or admin-flipped to qualified out-of-band.
            let bucket = match nod.nod_buckets.get(bucket_key)? {
                Some(b) if !b.is_qualified => b,
                _ => continue,
            };
            // Tail bin: exact-check `floor_price < rate` (strict) so a coarse
            // bin neither qualifies a bucket above the rate nor one priced
            // exactly at it — equality stays unqualified.
            if !strict && bucket.floor_price_minor >= rate {
                survivors.push(bucket_key);
                continue;
            }
            nod.qualify_bucket(bucket.worldwide_day, bucket.floor_price_minor)?;
        }

        if survivors.is_empty() {
            // Drain entire bin: zero index slots, reset count, clear bit.
            for i in 0..count {
                nod.unqualified_bin_buckets.write(
                    &NodContract::bin_index_key(next, i),
                    alloy_primitives::B256::ZERO,
                )?;
            }
            nod.unqualified_bin_count.write(&next, 0)?;
            tree_math::remove(&nod, next)?;
        } else {
            // Compact survivors into [0..len), zero the tail. Bit stays set.
            for (i, k) in survivors.iter().enumerate() {
                nod.unqualified_bin_buckets
                    .write(&NodContract::bin_index_key(next, i as u32), *k)?;
            }
            for i in (survivors.len() as u32)..count {
                nod.unqualified_bin_buckets.write(
                    &NodContract::bin_index_key(next, i),
                    alloy_primitives::B256::ZERO,
                )?;
            }
            nod.unqualified_bin_count
                .write(&next, survivors.len() as u32)?;
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => break,
        };
    }
    Ok(())
}

/// Qualifies Nod buckets using only compact EVM worklists and the off-chain body reader.
pub fn qualify_nods_with_reader(
    ctx: &BlockRuntimeContext,
    reader: &NodRepositoryReader,
) -> Result<()> {
    let rate = get_exchange_rate(ctx.storage.clone(), QUALIFIER_BASE, QUALIFIER_QUOTE)?;
    qualify_buckets_with_rate_and_reader(ctx, reader, rate)
}

/// Reader-aware qualification entry point used by the block executor.
pub fn qualify_buckets_with_rate_and_reader(
    ctx: &BlockRuntimeContext,
    reader: &NodRepositoryReader,
    rate: U256,
) -> Result<()> {
    if rate.is_zero() {
        return Ok(());
    }
    let r_bin = NodContract::price_to_bin(rate)?;
    let mut nod = NodContract::new(ctx.storage.clone());
    let mut cursor = 0_u32;
    loop {
        let next = match tree_math::find_first_left_inclusive(&nod, cursor)? {
            Some(bin) if bin <= r_bin => bin,
            _ => break,
        };
        let strict = next < r_bin;
        let count = nod.unqualified_bin_count.read(&next)?;
        let mut survivors = Vec::new();
        for index in 0..count {
            let key = NodContract::bin_index_key(next, index);
            let bucket_key = nod.unqualified_bin_buckets.read(&key)?;
            if bucket_key.is_zero() {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {next}:{index} has an empty bucket key"
                    )),
                );
            }
            let bucket = reader.get_bucket(bucket_key)?.ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod unqualified-bin entry {next}:{index} references missing bucket {bucket_key}"
                ))
            })?;
            if bucket.is_qualified || NodContract::price_to_bin(bucket.floor_price_minor)? != next {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {next}:{index} mismatches bucket {bucket_key}"
                    )),
                );
            }
            if !strict && bucket.floor_price_minor >= rate {
                survivors.push(bucket_key);
                continue;
            }
            nod.qualify_bucket_with_reader(reader, bucket_key)?;
        }

        if survivors.is_empty() {
            for index in 0..count {
                nod.unqualified_bin_buckets.write(
                    &NodContract::bin_index_key(next, index),
                    alloy_primitives::B256::ZERO,
                )?;
            }
            nod.unqualified_bin_count.write(&next, 0)?;
            tree_math::remove(&nod, next)?;
        } else {
            for (index, bucket_key) in survivors.iter().enumerate() {
                nod.unqualified_bin_buckets
                    .write(&NodContract::bin_index_key(next, index as u32), *bucket_key)?;
            }
            for index in (survivors.len() as u32)..count {
                nod.unqualified_bin_buckets.write(
                    &NodContract::bin_index_key(next, index),
                    alloy_primitives::B256::ZERO,
                )?;
            }
            nod.unqualified_bin_count
                .write(&next, survivors.len() as u32)?;
        }

        cursor = match next.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => break,
        };
    }
    Ok(())
}
