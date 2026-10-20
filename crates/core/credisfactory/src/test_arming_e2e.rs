//! E2E-only hooks. A Credis is stamped live, so only a throwaway build can move its
//! issuance behind the closed days a scenario seeds for the call sweep. The forfeit
//! sweep opens an hour only once it has closed, so `closeCallNoticeForTest` moves a
//! lapsed deadline into an hour the sweep opens on the next block.

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
    credis.push_called(group, deadline)
}
