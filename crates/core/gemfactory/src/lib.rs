//! GemFactory: Gem issuance, settlement and mining, the Gem call and close sweeps, and
//! the GemFactory positions. Gem data lives in the Gem crate.

pub mod api;
pub mod call;
pub(crate) mod call_state;
pub mod close;
pub mod errors;
pub(crate) mod forfeit;
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
