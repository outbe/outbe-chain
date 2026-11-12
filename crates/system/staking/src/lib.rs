pub mod contract;
pub mod errors;
pub mod genesis;
pub mod hooks;
pub mod logic;
pub mod precompile;
mod unbonding;

pub use errors::GenesisValidationError;
pub use genesis::{validate_genesis_state, validate_genesis_state_for_regime};
pub use hooks::StakingModule;

#[cfg(test)]
mod native_sink_tests;
#[cfg(test)]
mod proptests;
#[cfg(test)]
mod tests;
