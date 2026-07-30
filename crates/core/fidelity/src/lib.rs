pub mod api;
pub mod math;
pub mod precompile;
pub mod runtime;
pub mod schema;

pub use math::{MAX_LEAGUE, MIN_LEAGUE};
pub use runtime::{FidelityOcompProjection, OCOMP_POC_MAX_COHORTS_PER_OWNER};
pub use schema::FidelityContract;

#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod tests;
