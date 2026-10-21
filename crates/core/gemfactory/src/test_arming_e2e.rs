//! Hooks only a throwaway e2e build answers. Genesis cannot seed a Gem body, so a scenario
//! that needs a paid Gem asks for one. A gem is stamped live, so only such a build can move
//! its issuance back behind the closed days a scenario seeds for the call sweep. The forfeit
//! sweep opens a group only once its deadline day has closed, so `closeCallNoticeForTest` moves a
//! deadline that already lapsed into one the sweep opens on the next block, and
//! `closePositionHourForTest` does the same for an expired position's hour.

use alloy_primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_gem::{api as gem_api, GemAddParams, GemContract};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::errors::GemFactoryError;

alloy_sol_types::sol! {
    #[sol(alloy_sol_types = alloy_sol_types)]
    interface IGemFactoryTestArming {
        function issueSettledGemForTest(address owner, uint256 promisLoadMinor, uint16 currency) external returns (uint256 gemId);
        function backdateGemForTest(uint256 gemId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 gemId, uint64 deadline) external;
        function closePositionHourForTest(uint256 positionId, uint64 deadline) external;
    }
}

/// Answers a test hook, or `None` when `data` is no test hook.
pub(crate) fn dispatch(storage: &StorageHandle<'_>, data: &[u8]) -> Option<Result<Bytes>> {
    use IGemFactoryTestArming::*;
    if let Ok(call) = issueSettledGemForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            let gem_id = issue_settled(storage, call)?;
            Ok(issueSettledGemForTestCall::abi_encode_returns(&gem_id).into())
        }));
    }
    if let Ok(call) = backdateGemForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            backdate(storage, call.gemId, call.issuedAt).map(|()| Bytes::new())
        }));
    }
    if let Ok(call) = closeCallNoticeForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_called(storage, call.gemId, call.deadline).map(|()| Bytes::new())
        }));
    }
    if let Ok(call) = closePositionHourForTestCall::abi_decode(data) {
        return Some(guarded(storage, || {
            requeue_position_hour(storage, call.positionId, call.deadline).map(|()| Bytes::new())
        }));
    }
    None
}

/// Moves the queue entry of an expired position's hour onto `deadline`.
fn requeue_position_hour(
    storage: &StorageHandle<'_>,
    position_id: U256,
    deadline: u64,
) -> Result<()> {
    use outbe_primitives::expiry_queue;
    let factory = crate::GemFactoryContract::new(storage.clone());
    let position = factory
        .positions
        .get(position_id)?
        .ok_or(GemFactoryError::PositionNotFound)?;
    let now = storage.now_secs()?;
    if now < position.expires_at {
        return Err(GemFactoryError::PositionNotExpired.into());
    }
    let days = crate::state::PositionExpiryDays(&factory);
    let entry = crate::state::position_queue_entry(position.expiry_hour())?;
    expiry_queue::remove(&days, entry)?;
    expiry_queue::push(&days, entry, deadline)?;
    reopen_day(storage, deadline)
}

fn guarded(storage: &StorageHandle<'_>, hook: impl FnOnce() -> Result<Bytes>) -> Result<Bytes> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    hook()
}

/// Issues a gem and settles it at once, skipping the payment.
fn issue_settled(
    storage: &StorageHandle<'_>,
    call: IGemFactoryTestArming::issueSettledGemForTestCall,
) -> Result<U256> {
    // One six-decimal unit: a positive entry price.
    let unit = U256::from(1_000_000u64);
    let gem_id = crate::runtime::add_gem(
        storage,
        GemAddParams {
            owner: call.owner,
            promis_load_minor: call.promisLoadMinor,
            entry_price_minor: unit,
            issuance_currency: call.currency,
            reference_currency: call.currency,
            issued_at: storage.now_secs()?,
            call_terms: None,
        },
    )?;
    let (item, group) = gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    gem_api::record_settled(storage, item, group)?;
    Ok(gem_id)
}

/// Re-issues the gem on `issued_at`: it leaves its group and joins the group of its new
/// first full day, which may open a new group on the live terms.
fn backdate(storage: &StorageHandle<'_>, gem_id: U256, issued_at: u64) -> Result<()> {
    let (item, group) = gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.body().is_settled() {
        return Err(GemFactoryError::GemAlreadySettled.into());
    }
    let mut reissued = item.body().clone();
    let terms = crate::runtime::derived_call_terms(
        reissued.entry_price_minor,
        reissued.reference_currency,
        outbe_gem::config::read(storage)?,
    )?;
    reissued.issued_at = issued_at;
    reissued.group_key = outbe_primitives::call_breach::group_key(
        outbe_primitives::time::first_full_day(issued_at),
        terms.map_or(U256::ZERO, |terms| terms.call_price_minor),
        reissued.reference_currency,
    );
    storage.clone().with_checkpoint(|| {
        gem_api::remove_gem(storage, item, group)?;
        gem_api::record_issued(storage, &reissued, terms)
    })
}

/// Moves a called gem's group onto `deadline`: its call and its queue entry.
fn requeue_called(storage: &StorageHandle<'_>, gem_id: U256, deadline: u64) -> Result<()> {
    let item = gem_api::get_item(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    let mut gem = GemContract::new(storage.clone());
    let group = item.group_key;
    if gem.called_deadline.read(&group)? == 0 {
        return Err(GemFactoryError::GemNotCalled.into());
    }
    // Members read the deadline from the call, so the call moves with the queue.
    let notice = u64::from(gem.group_call_notice_period_seconds.read(&group)?);
    gem.group_called_at
        .write(&group, deadline.saturating_sub(notice).max(1))?;
    gem.remove_called(group)?;
    gem.push_called(group, deadline)?;
    reopen_day(storage, deadline)
}

fn reopen_day(storage: &StorageHandle<'_>, deadline: u64) -> Result<()> {
    let gem = GemContract::new(storage.clone());
    let day = outbe_primitives::time::timestamp_to_date_key(deadline);
    gem.call_sweep_day.write(day)?;
    gem.call_latest_day
        .write(gem.call_latest_day.read()?.max(day))?;
    gem.daily_genesis_day.write(day)?;
    gem.daily_phase.write(0)?;
    for currency in outbe_oracle::api::reference_currencies(storage.clone())? {
        gem.call_bin_cursor.clear(&currency)?;
    }
    gem.call_currency_cursor.clear()
}
