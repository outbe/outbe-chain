pub mod config;
pub mod constants;
pub mod errors;
mod metadata;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub(crate) mod state;
#[cfg(feature = "e2e-test")]
mod test_arming;

pub use runtime::{
    calc_call_price, outcome, settlement_deadline, Forfeit, IssueCredisParams, Outcome, Settlement,
};
pub use schema::{Credis, CredisContract, CredisState};
pub use state::{CallBins, ExpiryHours};

#[cfg(test)]
mod tests;
