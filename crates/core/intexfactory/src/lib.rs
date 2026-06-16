//! IntexFactory: Intex issuance, settlement (settle / minePromis), and the
//! autonomous Issued → Qualified → Called lifecycle. Series state is written to
//! Intex; this module owns the settlement bookkeeping and candidate index.

pub mod api;
pub mod called;
pub mod constants;
pub mod errors;
pub mod precompile;
pub mod qualified;
pub(crate) mod runtime;
pub mod schema;
pub(crate) mod sol_ext;
pub(crate) mod state;

pub use api::issue;
pub use errors::IntexFactoryError;
pub use qualified::IntexLifecycle;
pub use schema::{IntexFactoryContract, IssuanceParams};

#[cfg(test)]
mod tests;
