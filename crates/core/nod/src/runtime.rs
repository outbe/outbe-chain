use alloy_primitives::U256;
use outbe_compressed_entities::{update, BodyInput, ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    api::LoadedNodBucket,
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    precompile::INod,
    schema::NodContract,
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
        // Arm the bucket for the daily call scan from the terms sealed at
        // issuance. A zero entry price was left unsealed then, because a zero
        // call price would fire on the first scan; leave such a bucket unarmed.
        // A non-zero entry with a zero sealed price means issuance failed to
        // snapshot - do not fall back to live constants.
        if !bucket.entry_price_minor.is_zero() {
            let terms = self.read_call_terms(bucket.bucket_key)?;
            if terms.call_price.is_zero() {
                return Err(PrecompileError::Fatal(format!(
                    "Nod bucket {} is missing issuance-sealed call terms",
                    bucket.bucket_key
                )));
            }
            self.insert_callable_bucket(bucket.bucket_key)?;
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
