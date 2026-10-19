//! GemFactory: Gem issuance, settlement and mining, the Gem call and forfeit sweeps, and
//! the factory's own position NFT. Gem data lives in the Gem crate.

pub mod api;
pub mod called;
pub mod errors;
pub mod expired;
pub mod hooks;
pub mod positions_expired;
pub mod precompile;
pub mod schema;
#[cfg(feature = "e2e-test")]
mod test_arming_e2e;

pub mod constants;
pub(crate) mod runtime;
pub(crate) mod sol_ext;
pub(crate) mod state;

pub use schema::{GemFactoryContract, GemIssueParams};

#[cfg(test)]
mod tests;
