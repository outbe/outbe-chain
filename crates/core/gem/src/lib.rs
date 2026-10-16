//! Gem: the terms, profile and data of a Gem. Bodies, buckets, the call bins and the
//! expiry queue live here, with the read-time state.
//!
//! GemFactory applies every rule and runs the call and forfeit sweeps through [`api`].
//! The precompile surface is the Gem NFT and its views.

pub mod api;
pub mod config;
pub mod constants;
pub mod errors;
pub mod identity;
pub mod partitioning;
pub mod precompile;
pub mod projection;
pub mod schema;

pub(crate) mod metadata;
mod repository;
pub(crate) mod state;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use api::{LoadedGem, LoadedGemBucket};
pub use config::GemParams;
pub use errors::{GemError, GemFatalError, GemRepositoryError};
pub use repository::{
    canonical_bucket, canonical_bucket_id, canonical_item, from_canonical_bucket,
    from_canonical_item, gem_reader, gem_writer, GemPage, GemPageRequest, GemRepositoryReader,
    GemRepositoryWriter,
};
pub use schema::{
    CallTerms, GemAddParams, GemBucketState, GemContract, GemItemState, GemState, GemView,
};
pub use state::{CallBins, ExpiryHours};

#[cfg(test)]
mod tests;
