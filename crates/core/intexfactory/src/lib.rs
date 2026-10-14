//! IntexFactory: Intex issuance, settlement (settle / minePromis), and the
//! autonomous call lifecycle over the data Intex keeps. Qualification is derived from
//! daily VWAPs and is never stored.

pub mod api;
pub mod called;
pub mod constants;
pub mod errors;
pub(crate) mod expired;
pub mod hooks;
pub mod notify;
pub mod parked;
pub mod precompile;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod sol_ext;
pub(crate) mod state;
pub mod vwap_push;

pub use api::issue;
pub use errors::IntexFactoryError;
pub use outbe_intex::SeriesId;
pub use runtime::to_wire_price;
pub use schema::{IntexFactoryContract, IssuanceParams};

/// Narrow benchmark-only access to the already-public issuance hand-off type.
#[cfg(feature = "bench-utils")]
pub mod bench_support {
    pub use crate::runtime::IssuanceLeg;
}

#[cfg(test)]
mod tests;
