//! Production Metadosis exposes typed reads and purpose-bound commands, but
//! keeps raw schema/state/runtime mutation surfaces crate-private.
//!
//! ```compile_fail
//! use outbe_metadosis::schema::MetadosisContract;
//! ```
//!
//! ```compile_fail
//! use outbe_metadosis::runtime::process_emission_day;
//! ```
//!
//! ```compile_fail
//! use outbe_metadosis::limits::set_metadosis_limit;
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

pub use api::EmissionDayProjection;
pub use outbe_worldwideday::WwdProjection;
pub use schema::EmissionDay;

#[cfg(test)]
mod tests;
