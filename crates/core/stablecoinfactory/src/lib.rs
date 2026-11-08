//! Governed Stablecoin Factory V1 state and typed internal API.

pub mod api;
pub mod errors;
pub mod precompile;
pub mod schema;
pub mod vote_target;

mod abi;
mod state;

pub use api::{FactoryReservation, StablecoinFactoryApi, ValidatedStablecoinCreate};
pub use schema::{ReservationRecord, StablecoinFactoryContract};
pub use vote_target::{StablecoinFactoryVoteTarget, StablecoinVoteTargetError};

#[cfg(test)]
mod tests;
