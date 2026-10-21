//! These hooks serve only E2E tests. A Credis receives its issuance timestamp when issued.
//! Only a temporary build can move that timestamp before the closed days that a scenario initializes for the call sweep.
//! The forfeit sweep opens a day only after that day closes.
//! Thus, `closeCallNoticeForTest` moves a lapsed deadline into a day that the sweep opens on the next block.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_primitives::dispatch::ensure_mutation_allowed;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_credis::CredisContract;

use crate::errors::CredisFactoryError;

sol! {
    interface ICredisFactoryTestArming {
        function backdateCredisForTest(uint256 credisId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 credisId, uint64 deadline) external;
        function rescanClosedDayForTest() external;
    }
}

/// Runs a test-arming call, or returns `None` for any other calldata.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use ICredisFactoryTestArming::*;
    if let Ok(call) = backdateCredisForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            backdate(storage, call.credisId, call.issuedAt)
        }));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.credisId, call.deadline)
        }));
    }
    if rescanClosedDayForTestCall::abi_decode(data).is_ok() {
        return Some(guarded(storage, || {
            let day = outbe_primitives::time::previous_date_key(
                outbe_primitives::time::timestamp_to_date_key(storage.now_secs()?),
            );
            reopen_day(storage, day)
        }));
    }
    None
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<()>) -> Result<Bytes> {
    ensure_mutation_allowed(storage)?;
    hook().map(|()| Bytes::new())
}

/// Moves issuance and the interest anchor together, so no interest is skipped. The
/// Credis moves to the group of its new issuance day.
fn backdate(storage: &StorageHandle<'_>, credis_id: U256, issued_at: u64) -> Result<()> {
    let mut credis = CredisContract::new(storage.clone());
    let mut record = credis.get_credis(credis_id)?;
    credis.leave_group(record.group_key(), credis_id)?;
    credis.delete_credis_record(credis_id)?;
    record.issued_at = issued_at;
    record.last_settled_at = issued_at;
    credis.create_credis_record(&record)
}

/// Moves the deadline of the Credis's called group.
fn requeue_called(storage: &StorageHandle<'_>, credis_id: U256, deadline: u64) -> Result<()> {
    let mut credis = CredisContract::new(storage.clone());
    let group = credis.get_credis(credis_id)?.group_key();
    if credis.called_deadline.read(&group)? == 0 {
        return Err(CredisFactoryError::NotCalled.into());
    }
    // Members read the deadline from the call, so the call moves with the queue.
    let notice = u64::from(credis.group_call_notice_period_seconds.read(&group)?);
    credis
        .group_called_at
        .write(&group, deadline.saturating_sub(notice).max(1))?;
    credis.remove_called(group)?;
    credis.push_called(group, deadline)?;
    let day = outbe_primitives::time::timestamp_to_date_key(deadline);
    reopen_day(storage, day)
}

fn reopen_day(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    // The fixture may target a closed day before this temporary chain's genesis.
    outbe_oracle::reopen_daily_call_sweep!(CredisContract::new(storage.clone()), storage, day)
}
