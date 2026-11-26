//! IntexFactory: Intex issuance, settlement (settle / minePromis), the auction proceeds
//! burn, the target-chain notices, and the call and expiry sweeps over the data Intex
//! keeps.

pub mod api;
pub mod call;
pub mod call_notice;
pub(crate) mod call_state;
pub(crate) mod close;
pub mod constants;
pub mod errors;
pub(crate) mod forfeit;
pub mod hooks;
pub mod outbox;
pub mod precompile;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod sol_ext;
pub(crate) mod state;
pub mod targets;
pub mod transport;

pub use api::open_intex_series;
pub use errors::IntexFactoryError;
pub use hooks::IntexFactoryModule;
pub use outbe_intex::SeriesId;
pub use outbox::IntexFactoryOutboxModule;
pub use runtime::{marked_up, to_wire_price};
pub use schema::{IntexFactoryContract, IssuanceParams};

/// Narrow benchmark-only access to the already-public issuance hand-off type.
#[cfg(feature = "bench-utils")]
pub mod bench_support {
    pub use crate::runtime::IssuanceLeg;
}

#[cfg(test)]
mod tests;
