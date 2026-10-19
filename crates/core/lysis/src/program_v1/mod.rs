//! Storage-independent Lysis V1 semantic program.
//!
//! The program consumes logical observations and emits typed actions.
//! The program owns no storage, node, process, or worker dependency.

mod execute;
mod types;

pub use execute::{
    calculate_gratis_load, compute_fraction_map_from_groups, execute, execute_with_amounts,
};
#[cfg(test)]
pub(crate) use execute::{compute_fraction_hash_map, validate_required_gratis};
pub use types::{
    FidelityPhaseV1, LeagueFractionV1, LysisArithmeticErrorV1, NodActionV1, ObservationValueV1,
    ObservedTributeV1, ProgramErrorV1, ProgramInputV1, ProgramResultV1, SemanticObservationV1,
    TributeInputV1,
};
