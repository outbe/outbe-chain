//! Hooks only a throwaway e2e build answers. Lysis issues a Nod over a certified
//! generation, so a scenario that needs one asks for it straight. The forfeit sweep opens a
//! bucket only once its hour has closed, so `closeCallNoticeForTest` moves a lapsed deadline
//! into an hour the sweep opens on the next block.

use alloy_primitives::{Bytes, B256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_nod::schema::NodContract;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::runtime;

alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface INodFactoryTestArming {
        function issueForTest(
            address owner,
            bytes32 creatorPublicKey,
            uint32 worldwideDay,
            uint256 gratisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint64 issuedAt
        ) external;
        function closeCallNoticeForTest(bytes32 bucketKey, uint64 deadline) external;
    }
}

/// Answers a test hook, or `None` when `data` is no test hook.
pub(crate) fn dispatch(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    data: &[u8],
) -> Option<Result<Bytes>> {
    use INodFactoryTestArming::*;
    if let Ok(call) = issueForTestCall::abi_decode(data) {
        return Some(guarded(storage, || issue(storage, scope, parent, call)));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.bucketKey, call.deadline)
        }));
    }
    None
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<()>) -> Result<Bytes> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    hook().map(|()| Bytes::new())
}

/// Issue one Nod straight to its owner. A non-zero `issuedAt` restamps the bucket,
/// since calls and qualification count only days after it.
fn issue(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    call: INodFactoryTestArming::issueForTestCall,
) -> Result<()> {
    let day = outbe_primitives::time::WorldwideDay::new(call.worldwideDay);
    let encrypted = outbe_tee::nod_mine::create_nod_for_test(
        outbe_primitives::nod_encryption::NodTermsV2 {
            chain_id: storage.chain_id()?,
            nod_id: outbe_nod::identity::generate_nod_id(call.owner, day)?,
            owner: call.owner,
            worldwide_day: day,
            league_id: 1,
            entry_price_minor: call.entryPriceMinor,
            issuance_currency: call.issuanceCurrency,
            reference_currency: call.referenceCurrency,
        },
        call.creatorPublicKey.0,
        call.gratisLoadMinor,
    )
    .map_err(|e| PrecompileError::Revert(e.to_string()))?;
    runtime::issue_nod(storage, scope, parent, &encrypted)?;
    if call.issuedAt != 0 {
        let bucket_key =
            outbe_nod::identity::bucket_key(day, call.entryPriceMinor, call.referenceCurrency);
        NodContract::new(storage.clone())
            .bucket_issued_at
            .write(&bucket_key, call.issuedAt)?;
    }
    Ok(())
}

/// Moves a called bucket's queue entry onto `deadline`.
fn requeue_called(
    storage: &StorageHandle<'_>,
    bucket_key: B256,
    deadline: u64,
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    if nod.called_deadline.read(&bucket_key)? == 0 {
        return Err(PrecompileError::Revert(
            "closeCallNoticeForTest: bucket not queued".into(),
        ));
    }
    nod.remove_called(bucket_key)?;
    nod.push_called(bucket_key, deadline)
}
