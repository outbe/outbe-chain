use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::contract::Staking;

/// Called from pre-execution: processes matured unbonding entries.
///
/// The function zeroes all entries whose complete_time <= timestamp and
/// compacts the queue via swap-remove.
pub fn process_unbonding(storage: StorageHandle, timestamp: u64) -> Result<()> {
    let mut staking = Staking::new(storage);
    staking.process_unbonding(timestamp)
}
