pub mod api;
pub mod math;
mod openings;
pub mod precompile;
pub mod runtime;
pub mod schema;

pub use openings::{
    fidelity_count_slot_plan_v1, fidelity_opening_slot_plan_v1, FidelityCountSlotPlanV1,
    FidelityOpeningPlanError, FidelityOpeningSlotPlanV1, MAX_OCOMP_COHORTS_PER_OWNER,
};
pub use runtime::{FidelityOcompProjection, OCOMP_POC_MAX_COHORTS_PER_OWNER};
pub use schema::FidelityContract;

#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod tests;
