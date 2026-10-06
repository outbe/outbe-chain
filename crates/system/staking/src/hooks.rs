use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::contract::Staking;

/// Called from pre-execution.
///
/// Does not zero mature entries. `claim_unbonded` zeroes them.
/// Compaction is a capped tail trim, not a swap-remove.
/// The call also moves the stake of an UNBONDING validator into the queue.
pub fn process_unbonding(storage: StorageHandle, timestamp: u64) -> Result<()> {
    let mut staking = Staking::new(storage);
    staking.process_unbonding(timestamp)
}
