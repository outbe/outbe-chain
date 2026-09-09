use alloy_primitives::U256;
use outbe_compressed_entities::{update, BodyInput, ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    api::LoadedNodBucket,
    constants::{
        CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_WINDOW, TOKEN_NAME, TOKEN_SYMBOL,
    },
    precompile::INod,
    schema::{CallTerms, NodContract},
};

impl NodContract<'_> {
    /// Loads and qualifies one bucket through the generic overlay lifecycle.
    pub fn qualify_bucket(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        bucket_key: alloy_primitives::B256,
    ) -> Result<()> {
        let worldwide_day = self.bucket_worldwide_day.read(&bucket_key)?;
        let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
        let current = crate::api::load_bucket(&self.storage_handle(), scope, parent, bucket_id)?
            .ok_or_else(|| PrecompileError::Revert("qualify_bucket: bucket missing".into()))?;
        self.qualify_bucket_loaded(scope, current)
    }

    pub(crate) fn qualify_bucket_loaded(
        &mut self,
        scope: &ExecutionScope,
        current: LoadedNodBucket,
    ) -> Result<()> {
        let (mut bucket, capability) = current.into_parts();
        if bucket.is_qualified {
            return Ok(());
        }
        bucket.is_qualified = true;
        let canonical = crate::repository::canonical_bucket(&bucket);
        update(
            self.storage_handle(),
            scope,
            capability,
            BodyInput::NodBucket(&canonical),
        )?;
        // Arm the bucket for the daily call scan. A zero entry price would yield
        // a zero call price that every published VWAP exceeds, calling the bucket
        // on its first scan; leave such a bucket unarmed instead. Lysis rejects a
        // zero nominal price, but the certified-materialization path takes the
        // value from a batch.
        if !bucket.entry_price_minor.is_zero() {
            let call_price = bucket
                .entry_price_minor
                .checked_mul(U256::from(100 + CALL_RATE_PCT))
                .ok_or_else(|| {
                    PrecompileError::Fatal(format!(
                        "Nod bucket {} call price overflow",
                        bucket.bucket_key
                    ))
                })?
                / U256::from(100u64);
            // The constants are read exactly here, once. Every later check reads
            // the bucket's sealed copy, so a retune cannot re-term it.
            self.insert_callable_bucket(
                bucket.bucket_key,
                CallTerms {
                    call_price,
                    reference_currency: bucket.reference_currency,
                    call_rate: CALL_RATE_PCT,
                    call_window: CALL_WINDOW,
                    call_threshold: CALL_THRESHOLD,
                    call_notice_period: CALL_NOTICE_PERIOD,
                },
            )?;
        }
        self.emit(INod::NodBucketQualified {
            bucketKey: bucket.bucket_key,
            worldwideDay: U256::from(u32::from(bucket.worldwide_day)),
            floorPriceMinor: bucket.floor_price_minor,
            referenceCurrency: bucket.reference_currency,
        })
    }

    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }
}
