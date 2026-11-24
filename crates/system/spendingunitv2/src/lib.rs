//! Native precompile for the PSO SpendingUnitV2 (`0x5200000000000000000000000000000000000008`).

pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;
pub mod worldwide_day;

pub use errors::SpendingUnitV2Error;
pub use precompile::ISpendingUnitV2;
pub use runtime::{
    seal_id, settlement_id, settlement_message, BN254_FR_MODULUS, MAX_SEALS, POOL_RETENTION_DAYS,
    VERSION,
};
pub use schema::{SpendingUnitV2Contract, SpendingUnitV2Record};
pub use worldwide_day::{day_number, today_utc_plus_14};

#[cfg(test)]
mod tests;
