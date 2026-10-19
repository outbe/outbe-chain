//! Nod: the terms, profile and data of a Nod. Bodies, buckets, the call bins and the
//! expiry queue live here, with the read-time state and settlement cost.
//!
//! NodFactory applies every rule and runs the call and forfeit sweeps through [`api`].
//! The precompile surface is the Nod NFT and its views.

pub mod api;
pub mod codec;
pub mod config;
pub mod constants;
mod enclave_client;
pub mod errors;
pub mod identity;
pub mod index_keys;
mod issuance;
mod metadata;
pub mod openings;
pub mod precompile;
pub mod pricing;
pub mod runtime;
pub mod schema;
pub mod state;
pub mod state_api;
pub mod storage_records;
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_support;

pub use schema::{NodBucketState, NodContract, NodIssueParams, NodItemState};

#[cfg(test)]
mod tests;

pub use codec::{canonical_bucket, canonical_item, from_canonical_bucket, from_canonical_item};
