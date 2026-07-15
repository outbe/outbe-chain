use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    errors::NodError,
    precompile::INod,
    schema::{NodBucketState, NodContract, NodItemState},
};

impl NodContract<'_> {
    pub fn get_nod_data(&self, nod_id: U256) -> Result<(NodItemState, NodBucketState)> {
        let item = self.get_item(nod_id)?.ok_or(NodError::NodNotFound)?;
        let bucket_key = Self::bucket_key(item.worldwide_day, item.floor_price_minor);
        let bucket = self
            .nod_buckets
            .get(bucket_key)?
            .ok_or(NodError::NodNotFound)?;
        Ok((item, bucket))
    }

    /// Set `is_qualified = true` on the bucket and emit `NodBucketQualified`.
    /// Called from `NodLifecycle::begin_block` once the COEN/0xUSD oracle rate
    /// reaches `bucket.floor_price_minor`. Does NOT remove the bucket from
    /// `unqualified_heap` — the hook pops the root as part of its scan.
    pub fn qualify_bucket(
        &mut self,
        worldwide_day: WorldwideDay,
        floor_price_minor: U256,
    ) -> Result<()> {
        let bucket_key = Self::bucket_key(worldwide_day, floor_price_minor);
        let mut bucket = self
            .nod_buckets
            .get(bucket_key)?
            .ok_or_else(|| PrecompileError::Revert("qualify_bucket: bucket missing".into()))?;
        if bucket.is_qualified {
            return Ok(());
        }
        bucket.is_qualified = true;
        self.nod_buckets.update(&bucket)?;
        self.emit(INod::NodBucketBodyStored {
            bucketKey: bucket.bucket_key,
            worldwideDay: bucket.worldwide_day.into(),
            floorPriceMinor: bucket.floor_price_minor,
            isQualified: bucket.is_qualified,
            totalNods: bucket.total_nods,
            entryPriceMinor: bucket.entry_price_minor,
        })?;
        self.emit(INod::NodBucketQualified {
            bucketKey: bucket_key,
            worldwideDay: U256::from(u32::from(worldwide_day)),
            floorPriceMinor: floor_price_minor,
            isQualified: true,
        })?;
        Ok(())
    }

    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }
}
