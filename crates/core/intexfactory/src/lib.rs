//! IntexFactory: Intex issuance, settlement (settle / minePromis), the auction proceeds
//! burn, the target-chain notices, and the call and expiry sweeps over the data Intex
//! keeps.

pub mod api;
pub mod called;
pub(crate) mod cleanup;
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
#[cfg(feature = "e2e-test")]
mod test_arming_e2e;

pub use api::issue_intex;
pub use errors::IntexFactoryError;
pub use outbe_intex::SeriesId;
pub use runtime::{marked_up, to_wire_price};
pub use schema::{IntexFactoryContract, IssuanceParams};

/// Narrow benchmark-only access to the already-public issuance hand-off type.
#[cfg(feature = "bench-utils")]
pub mod bench_support {
    pub use crate::runtime::IssuanceLeg;
}

#[cfg(test)]
mod tests;
