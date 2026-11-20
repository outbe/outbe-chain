//! GemFactory: Gem issuance, settlement and mining, the Gem call and forfeit sweeps, and
//! the GemFactory positions. Gem data lives in the Gem crate.

pub mod api;
pub(crate) mod call_progress;
pub mod called;
pub(crate) mod cleanup;
pub mod errors;
pub mod forfeited;
pub mod hooks;
pub(crate) mod position_expiry;
pub mod precompile;
pub mod schema;

pub(crate) mod runtime;
pub(crate) mod sol_ext;
pub(crate) mod state;

pub use hooks::{GemCleanupModule, GemFactoryModule};
pub use schema::{GemFactoryContract, GemIssueParams};

#[cfg(test)]
mod tests;
