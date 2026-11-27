//! Native precompile for the PSO EnclaveRegistry (`0x5200000000000000000000000000000000000002`).

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod snp;

pub use errors::EnclaveRegistryError;
pub use precompile::IEnclaveRegistry;
pub use runtime::{verify_ed25519_strict, DEFAULT_RETIRING_GRACE, MIN_SCHEMA};
pub use schema::{DeploymentRecord, EnclaveRegistryContract, ProviderPolicyRecord};

#[cfg(test)]
mod tests;
