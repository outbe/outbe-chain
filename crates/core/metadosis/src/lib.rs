//! Production Metadosis exposes typed reads and purpose-bound commands, but
//! keeps raw schema/state/runtime/commit mutation surfaces crate-private and
//! relies on sealed `outbe_primitives::storage` mutation leases/purposes.
//!
//! ```compile_fail,E0603
//! use outbe_metadosis::schema::MetadosisContract;
//! ```
//!
//! ```compile_fail,E0603
//! use outbe_metadosis::runtime::process_emission_day;
//! ```
//!
//! ```compile_fail,E0603
//! use outbe_metadosis::limits::set_metadosis_limit;
//! ```
//!
//! ```compile_fail,E0603
//! use outbe_metadosis::commit::commit_transition;
//! ```
//!
//! ```compile_fail,E0432
//! use outbe_primitives::storage::ScopedMutationLease;
//! ```
//!
//! ```compile_fail,E0277
//! use outbe_primitives::storage::{MutationPurposeTag, ScopedMutationPurpose};
//!
//! struct ForgedPurpose;
//!
//! impl ScopedMutationPurpose for ForgedPurpose {
//!     const TAG: MutationPurposeTag = MutationPurposeTag::CycleLifecycle;
//! }
//! ```

pub mod api;
#[cfg(feature = "bench-utils")]
pub mod bench_support;
pub mod commands;
mod commit;
pub mod constants;
pub mod errors;
mod lifecycle;
mod limits;
mod loads;
pub mod precompile;
mod runtime;
mod schema;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

#[cfg(test)]
mod tests;
