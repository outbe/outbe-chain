//! Production Metadosis exposes typed reads and purpose-bound commands, but
//! keeps raw schema/state/runtime mutation surfaces crate-private.
//!
//! The `test-utils` feature exposes the opaque semantic scenarios in
//! [`test_support`] and the narrowly typed, pre-launch-only [`genesis`]
//! builder. Raw modules remain inaccessible to external crates in both
//! default and `test-utils` builds.
//!
//! ```compile_fail
//! use outbe_metadosis::schema::MetadosisContract;
//! ```
//!
//! ```compile_fail
//! use outbe_metadosis::runtime::advance_active_worldwide_days;
//! ```
//!
//! ```compile_fail
//! use outbe_metadosis::emission_sink::apply;
//! ```
//!

mod aggregate;
pub mod api;
#[cfg(feature = "bench-utils")]
pub mod bench_support;
pub mod commands;
mod commit;
pub mod constants;
mod emission_sink;
pub mod errors;
#[cfg(any(test, feature = "test-utils"))]
mod fixture_kernel;
#[cfg(feature = "test-utils")]
pub mod genesis;
mod lifecycle;
mod limits;
mod lysis;
pub mod precompile;
mod reducer;
mod runtime;
mod schema;
mod settlement;
mod state;
mod terminal;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use aggregate::{WwdMembership, WwdProjection, WwdStatus};
pub use state::{DayLimitFormation, DayLimitFormationReceipt};

#[cfg(test)]
mod tests;
