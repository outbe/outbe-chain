//! Storage-independent Lysis V1 semantic program.
//!
//! The program consumes logical observations and emits typed actions.
//! The program owns no storage, node, process, or worker dependency.

mod execute;
mod types;

#[cfg(test)]
pub(crate) use execute::{compute_fraction_hash_map, validate_required_gratis};
pub use execute::{execute, execute_with_amounts};
pub use types::{
    FidelityPhaseV1, LeagueFractionV1, LysisArithmeticErrorV1, NodActionV1, ObservationValueV1,
    ObservedTributeV1, ProgramErrorV1, ProgramInputV1, ProgramResultV1, SemanticObservationV1,
    TributeInputV1,
};
