//! Production Metadosis exposes typed reads and purpose-bound commands, but
//! keeps raw schema/state/runtime mutation surfaces crate-private.
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

pub mod api;
#[cfg(feature = "bench-utils")]
pub mod bench_support;
pub mod commands;
mod commit;
pub mod constants;
mod emission_sink;
pub mod errors;
#[cfg(feature = "test-utils")]
pub use outbe_tributefactory::worldwideday::genesis;
mod lifecycle;
mod limits;
mod lysis;
pub mod precompile;
mod runtime;
mod schema;
mod settlement;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use api::EmissionDayProjection;
pub use outbe_tributefactory::worldwideday::WwdProjection;
pub use schema::{emission_day_state, EmissionDay};

#[cfg(test)]
mod tests;
