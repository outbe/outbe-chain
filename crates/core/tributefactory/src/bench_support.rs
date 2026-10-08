//! Narrow adapter used by the permanent Tribute creation benchmark.
//!
//! This module is absent from default builds. It lets the external Cargo
//! benchmark exercise the same crate-private processor-injection seam as unit
//! tests. That seam does not become part of TributeFactory's product API.

use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_tee::protocol::{EncryptedTributeOffer, TributeOfferResult};

use crate::schema::TributeFactoryContract;

pub use crate::runtime::OfferTributeInput as BenchOfferInput;

/// Execute one successful creation through the canonical TributeFactory
/// runtime with a caller-supplied benchmark processor. This function keeps the
/// production processor contract. The node-local transport stays outside the
/// standalone Cargo benchmark.
pub fn execute_offer_with_processor(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    input: BenchOfferInput,
    processor: impl FnOnce(
        &[EncryptedTributeOffer],
    ) -> core::result::Result<Vec<TributeOfferResult>, PrecompileError>,
) -> Result<WwdEntityId> {
    TributeFactoryContract::new(storage)
        .offer_tribute_with_processor(scope, parent, input, processor)
}
