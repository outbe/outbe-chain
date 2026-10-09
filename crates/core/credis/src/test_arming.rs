//! E2E-only hooks. A Credis is stamped live, so only a throwaway build can move its
//! issuance behind the closed days a scenario seeds for the call sweep. The forfeit
//! sweep opens an hour only once it has closed, so `closeCallNoticeForTest` moves a
//! lapsed deadline into an hour the sweep opens on the next block.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_primitives::dispatch::ensure_mutation_allowed;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::CredisError;
use crate::schema::CredisContract;

sol! {
    interface ICredisTestArming {
        function backdateCredisForTest(uint256 credisId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 credisId, uint64 deadline) external;
    }
}

/// Runs a test-arming call, or returns `None` for any other calldata.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    if let Ok(call) = ICredisTestArming::backdateCredisForTestCall::abi_decode(data) {
        return Some(backdate(storage, call.credisId, call.issuedAt));
    }
    if let Ok(call) = ICredisTestArming::closeCallNoticeForTestCall::abi_decode(data) {
        return Some(requeue(storage, call.credisId, call.deadline));
    }
    None
}

/// Moves issuance and the interest anchor together, so no interest is skipped.
fn backdate(storage: &StorageHandle<'_>, credis_id: U256, issued_at: u64) -> Result<Bytes> {
    ensure_mutation_allowed(storage)?;
    let mut credis = CredisContract::new(storage.clone());
    let mut record = credis.get_credis(credis_id)?;
    record.issued_at = issued_at;
    record.last_settled_at = issued_at;
    credis.update_credis_record(&record)?;
    Ok(Bytes::new())
}

fn requeue(storage: &StorageHandle<'_>, credis_id: U256, deadline: u64) -> Result<Bytes> {
    ensure_mutation_allowed(storage)?;
    let mut credis = CredisContract::new(storage.clone());
    if credis.called_deadline.read(&credis_id)? == 0 {
        return Err(CredisError::NotCalled.into());
    }
    credis.unqueue_called(credis_id)?;
    credis.queue_called(credis_id, deadline)?;
    Ok(Bytes::new())
}
