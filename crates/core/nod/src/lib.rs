//! Nod: the terms, profile and data of a Nod. Bodies, buckets, the call bins and the
//! expiry queue live here, with the read-time state and settlement cost.
//!
//! NodFactory applies every rule and runs the call and forfeit sweeps through [`api`].
//! The precompile surface is the Nod NFT and its views.

pub mod api;
pub mod config;
pub mod constants;
mod enclave_client;
pub mod errors;
pub mod identity;
pub mod index_keys;
mod issuance;
mod metadata;
pub mod openings;
pub mod partitioning;
pub mod precompile;
pub mod pricing;
pub mod projection;
mod repository;
pub mod runtime;
pub mod schema;
pub mod state;
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_support;

pub use repository::{
    canonical_bucket, canonical_bucket_id, canonical_item, clear_owner_day, from_canonical_bucket,
    from_canonical_item, nod_reader, nod_writer, NodPage, NodPageRequest, NodRepositoryError,
    NodRepositoryReader, NodRepositoryWriter,
};
pub use schema::{
    NodBucketState, NodCertifiedGenerationProjection, NodContract, NodIssueParams, NodItemState,
    NodOcompTargetProjection,
};

#[cfg(test)]
mod tests;
