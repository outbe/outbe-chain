pub mod constants;
pub mod emission_sink;
pub mod errors;
pub mod ocomp;
pub(crate) mod ocomp_budget;
mod pre_admission;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod state;

pub use pre_admission::{initialize_fresh_ocomp_profile, MetadosisPreAdmissionProjection};

#[cfg(test)]
mod tests;
