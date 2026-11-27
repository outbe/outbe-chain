//! Test-only entry points of Desis for other crates.

use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::EmissionDate;

/// Takes `day` into the schedule as the run at its auction start does. The record of the day
/// must have left `Pending`.
pub fn take_day(storage: StorageHandle<'_>, day: EmissionDate) -> Result<()> {
    let anchor = u64::from(crate::runtime::auction_anchor(day)?);
    crate::runtime::take_due_days(&storage, day.value(), anchor)
}
