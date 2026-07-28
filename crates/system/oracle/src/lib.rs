pub mod api;
pub mod contract;
pub mod hooks;
pub mod logic;
mod openings;
pub mod precompile;
pub mod scurve;
pub mod tally;

pub use openings::{
    evaluate_oracle_opening_v1, oracle_count_slot_plan_v1, oracle_opening_slot_plan_v1,
    OracleCountSlotPlanV1, OracleOpeningEvaluationError, OracleOpeningEvaluationV1,
    OracleOpeningPlanError, OracleOpeningSlotPlanV1, MAX_OCOMP_ACTIVE_SCURVE_ENTRIES,
    MAX_OCOMP_SETTLEMENT_CURRENCIES, MAX_OCOMP_WWD_PAIR_ENTRIES,
};

#[cfg(test)]
mod tests;
