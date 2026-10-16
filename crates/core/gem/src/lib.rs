//! Gem: the terms, profile and data of a Gem. Bodies, buckets, the call bins and the
//! expiry queue live here, with the read-time state and qualification.
//!
//! GemFactory applies every rule and runs the call and forfeit sweeps through [`api`].
//! The precompile surface is the Gem NFT and its views.

pub mod api;
pub mod codec;
pub mod config;
pub mod constants;
pub mod errors;
pub mod identity;
pub mod precompile;
pub mod schema;
pub mod state_api;
pub mod storage_records;

pub(crate) mod metadata;
pub(crate) mod state;

pub use api::{LoadedGem, LoadedGemBucket};
pub use config::GemParams;
pub use errors::{GemError, GemFatalError};
pub use schema::{
    CallTerms, GemAddParams, GemBucketState, GemContract, GemItemState, GemState, GemView,
    GENESIS_GEM_TYPE,
};
pub use state::{CallBins, ExpiryHours};

#[cfg(test)]
mod tests;

pub use codec::{
    canonical_bucket, canonical_bucket_id, canonical_item, from_canonical_bucket,
    from_canonical_item,
};

pub use codec::GemCodecError;
