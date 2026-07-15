use alloy_primitives::U256;
use outbe_compressed_entities::{
    body_commitment, encode_nod_bucket_v1, CommitmentState, EntityId36, ACTIVE_COMMITMENT_SCHEME,
    BODY_SCHEMA_V1,
};
use outbe_primitives::error::{PrecompileError, Result};

use crate::{
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    precompile::INod,
    schema::{NodBucketState, NodContract},
    NodRepositoryReader,
};

impl NodContract<'_> {
    /// Emits the qualified bucket body while keeping the full body off-chain.
    pub fn qualify_bucket_with_reader(
        &mut self,
        reader: &NodRepositoryReader,
        bucket_key: alloy_primitives::B256,
    ) -> Result<()> {
        let worldwide_day = self.bucket_worldwide_day.read(&bucket_key)?;
        let bucket_id = EntityId36::new(worldwide_day, bucket_key.0);
        let bucket = self
            .get_bucket_verified(reader, bucket_id)?
            .ok_or_else(|| PrecompileError::Revert("qualify_bucket: bucket missing".into()))?;
        self.qualify_bucket_body(bucket)
    }

    pub(crate) fn qualify_bucket_body(&mut self, mut bucket: NodBucketState) -> Result<()> {
        if bucket.is_qualified {
            return Ok(());
        }
        let bucket_id = EntityId36::new(bucket.worldwide_day, bucket.bucket_key.0);
        let commitments = CommitmentState::new(self.storage_handle());
        let previous = commitments.nod_bucket(bucket_id)?.ok_or_else(|| {
            PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_id} became canonically absent during qualification"
            ))
        })?;
        bucket.is_qualified = true;
        let payload = encode_nod_bucket_v1(&crate::repository::canonical_bucket(&bucket))
            .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
        let new_commitment = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            BODY_SCHEMA_V1,
            bucket_id,
            &payload,
        )
        .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
        commitments.set_nod_bucket(bucket_id, new_commitment)?;
        self.emit_bucket_body_stored(bucket_id, Some(previous), new_commitment, payload)?;
        self.emit(INod::NodBucketQualified {
            bucketKey: bucket.bucket_key,
            worldwideDay: U256::from(u32::from(bucket.worldwide_day)),
            floorPriceMinor: bucket.floor_price_minor,
            isQualified: true,
        })
    }

    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }
}
