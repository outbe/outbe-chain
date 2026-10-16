//! Hooks only a throwaway e2e build answers. Genesis cannot seed a Gem body, so a scenario
//! that needs a paid Gem asks for one. A gem is stamped live, so only such a build can move
//! its issuance back behind the closed days a scenario seeds for the call sweep. The forfeit
//! sweep opens a bucket only once its hour has closed, so `closeCallNoticeForTest` moves a
//! deadline that already lapsed into one the sweep opens on the next block.

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
    None
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
            gem_type: crate::schema::GemTypes::Wallet as u8,
            promis_load_minor: call.promisLoadMinor,
            entry_price_minor: unit,
            issuance_currency: call.currency,
            reference_currency: call.currency,
            issued_at: storage.timestamp()?.to::<u64>(),
            call_terms: None,
        },
    )?;
    let (item, bucket) =
        gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    gem_api::record_settled(storage, item, bucket)?;
    Ok(gem_id)
}

/// Re-issues the gem on `issued_at`: it leaves its bucket and joins the bucket of the
/// new day, which may open a new bucket body on the live terms.
fn backdate(storage: &StorageHandle<'_>, gem_id: U256, issued_at: u64) -> Result<()> {
    let (item, bucket) =
        gem_api::load_pair(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.body().is_settled() {
        return Err(GemFactoryError::InvalidState.into());
    }
    let mut reissued = item.body().clone();
    let entry_price_minor = bucket.body().entry_price_minor;
    reissued.issued_at = issued_at;
    reissued.bucket_key = outbe_gem::identity::bucket_key(
        reissued.worldwide_day(),
        entry_price_minor,
        reissued.reference_currency,
    );
    let terms = crate::runtime::derived_call_terms(
        entry_price_minor,
        reissued.reference_currency,
        outbe_gem::config::read(storage)?,
    )?;
    storage.clone().with_checkpoint(|| {
        gem_api::remove_gem(storage, item, bucket)?;
        gem_api::record_issued(storage, &reissued, entry_price_minor, terms)
    })
}

/// Moves the queue entry of a called gem's bucket onto `deadline`.
fn requeue_called(storage: &StorageHandle<'_>, gem_id: U256, deadline: u64) -> Result<()> {
    let item = gem_api::get_item(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    let mut gem = GemContract::new(storage.clone());
    let bucket = item.bucket_key;
    if gem.called_deadline.read(&bucket)? == 0 {
        return Err(GemFactoryError::InvalidState.into());
    }
    gem.remove_called(bucket)?;
    gem.push_called(bucket, deadline)
}
