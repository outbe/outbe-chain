pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use runtime::{BurnSource, LimitSource, LimitTarget, DEAD_ADDRESS};
pub use schema::PromisLimitContract;

/// Counts the base and blob fees that the EVM burned in a block. The daily step adds them.
pub fn settle_block_burned_fees(storage: &mut StorageHandle<'_>, burned_fees: U256) -> Result<()> {
    PromisLimitContract::new(storage.clone()).count_base_fees(burned_fees)
}

#[cfg(test)]
mod tests;
