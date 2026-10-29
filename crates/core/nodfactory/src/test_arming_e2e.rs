//! Only a temporary E2E build responds to these hooks. Lysis issues a Nod over a certified generation.
//! A scenario that needs an earlier group stamp moves that stamp in storage.
//! The forfeit sweep opens a group only after its day closes.
//! Thus, `closeCallNoticeForTest` moves a lapsed deadline into a day that the sweep opens on the next block.

use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_nod::schema::NodContract;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::NodFactoryError;

alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface INodFactoryTestArming {
        function backdateNodForTest(uint256 nodId, uint64 issuedAt, uint256 gratisLoadMinor) external;
        function closeCallNoticeForTest(bytes32 groupKey, uint64 deadline) external;
        function rescanClosedDayForTest() external;
    }
}

/// Answers a test hook, or `None` when `data` is no test hook.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use INodFactoryTestArming::*;
    if let Ok(call) = backdateNodForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            backdate_nod(storage, call.nodId, call.issuedAt, call.gratisLoadMinor)
        }));
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

/// Re-issues one unsettled Nod at `issued_at` without calling the enclave.
///
/// The encrypted body stays. The item leaves its old group, after that group's
/// unpaid load drops by `gratis_load_minor`, and joins the group of the new stamp.
fn backdate_nod(
    storage: &StorageHandle<'_>,
    nod_id: U256,
    issued_at: u64,
    gratis_load_minor: U256,
) -> Result<()> {
    let id = nod_id;
    let item = outbe_nod::api::load_item(storage, id)?.ok_or(NodFactoryError::NodNotFound)?;
    let group = outbe_nod::api::load_group(storage, item.body().group_key)?
        .ok_or(NodFactoryError::GroupNotFound)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    let terms = crate::runtime::call_terms(
        storage,
        item.body().entry_price_minor(),
        item.body().reference_currency,
    )?;
    let group_key = outbe_nod::identity::group_key(
        issued_at,
        terms.map_or(U256::ZERO, |terms| terms.call_price_minor),
        item.body().reference_currency,
    )
    .map_err(crate::errors::date_step_error)?;
    let old_group_key = item.body().group_key;
    let mut reissued = item.body().clone();
    reissued.issued_at = issued_at;
    reissued.group_key = group_key;
    storage.clone().with_checkpoint(|| {
        outbe_nod::api::take_unpaid_load(storage, old_group_key, gratis_load_minor)?;
        outbe_nod::api::remove_nod(storage, item, group)?;
        outbe_nod::api::add_nod(storage, &reissued, terms, gratis_load_minor)
    })
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
