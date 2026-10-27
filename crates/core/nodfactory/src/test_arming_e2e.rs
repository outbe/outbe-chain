//! Only a temporary E2E build responds to these hooks. Lysis issues a Nod over a certified generation.
//! A scenario that needs a Nod requests it directly.
//! The forfeit sweep opens a group only after its day closes.
//! Thus, `closeCallNoticeForTest` moves a lapsed deadline into a day that the sweep opens on the next block.

use alloy_primitives::{Bytes, B256};
use alloy_sol_types::SolCall;
use outbe_nod::schema::NodContract;
use outbe_primitives::error::Result;
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
        function closeCallNoticeForTest(bytes32 groupKey, uint64 deadline) external;
        function rescanClosedDayForTest() external;
    }
}

/// Answers a test hook, or `None` when `data` is no test hook.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use INodFactoryTestArming::*;
    if let Ok(call) = issueForTestCall::abi_decode(data) {
        return Some(guarded(storage, || issue(storage, call)));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.groupKey, call.deadline)
        }));
    }
    if rescanClosedDayForTestCall::abi_decode(data).is_ok() {
        return Some(guarded(storage, || {
            let day = outbe_primitives::time::previous_date_key(
                outbe_primitives::time::timestamp_to_date_key(storage.now_secs()?),
            )
            .map_err(crate::errors::date_step_error)?;
            reopen_day(storage, day)
        }));
    }
    None
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<()>) -> Result<Bytes> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    hook().map(|()| Bytes::new())
}

/// Issue one Nod straight to its owner. A non-zero `issuedAt` restamps the group,
/// since calls count only days after it.
fn issue(storage: &StorageHandle<'_>, call: INodFactoryTestArming::issueForTestCall) -> Result<()> {
    let day = outbe_primitives::time::WorldwideDay::new(call.worldwideDay);
    let encrypted = outbe_tee::nod_mine::create_nod_for_test(
        outbe_primitives::nod_encryption::NodTermsV1 {
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
    .map_err(crate::errors::NodFactoryError::CreateNodForTest)?;
    let issued_at = match call.issuedAt {
        0 => storage.now_secs()?,
        at => at,
    };
    runtime::issue_nod_at(storage, &encrypted, issued_at)?;
    Ok(())
}

/// Moves a called group onto `deadline`: its call and its queue entry.
fn requeue_called(storage: &StorageHandle<'_>, group_key: B256, deadline: u64) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    if nod.called_deadline.read(&group_key)? == 0 {
        return Err(crate::errors::NodFactoryError::GroupNotQueuedForTest.into());
    }
    // Members read the deadline from the call, so the call moves with the queue.
    let notice = u64::from(nod.group_call_notice_period_seconds.read(&group_key)?);
    nod.group_called_at
        .write(&group_key, deadline.saturating_sub(notice).max(1))?;
    nod.remove_called(group_key)?;
    nod.push_called(group_key, deadline)?;
    let day = outbe_primitives::time::timestamp_to_date_key(deadline);
    reopen_day(storage, day)
}

fn reopen_day(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    // The fixture may target a closed day before this temporary chain's genesis.
    let nod = NodContract::new(storage.clone());
    let genesis_day = nod.daily_genesis_day.read()?;
    if genesis_day == 0 || day < genesis_day {
        nod.daily_genesis_day.write(day)?;
    }
    if day > nod.call_latest_day.read()? {
        nod.call_latest_day.write(day)?;
    }
    nod.call_sweep_day.write(day)?;
    nod.daily_phase.write(0)?;
    nod.call_currency_cursor.write(0)?;
    for currency in outbe_oracle::api::REFERENCE_CURRENCIES {
        nod.call_bin_cursor.write(&currency, 0)?;
        nod.call_scan_failed_day.clear(&currency)?;
    }
    Ok(())
}
