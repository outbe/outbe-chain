//! Native precompile for the PSO AttestersRegistry (`0x5200000000000000000000000000000000000001`).

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

pub use errors::AttestersRegistryError;
pub use precompile::IAttestersRegistry;
pub use schema::{AttesterRecord, AttestersRegistryContract};

#[cfg(test)]
mod tests;
