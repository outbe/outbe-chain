//! ERC20 sub-calls for the tokens that the pool keeps. For the token,
//! `msg.sender` is the pool at `PAYNOTE_ADDRESS`.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::PAYNOTE_ADDRESS;

use crate::errors::PayNoteError;
use crate::sol_ext::IERC20;

/// Pull exactly `amount` of `asset` from `from` into the pool. The note
/// commitment uses `amount`. If the pool receives a different value, the note
/// does not have full backing. A fee-on-transfer token or a reentrant nested
/// deposit can cause this.
pub(crate) fn pull_exact(
    storage: &StorageHandle<'_>,
    asset: Address,
    from: Address,
    amount: U256,
) -> Result<()> {
    let before = balance_of(storage, asset, PAYNOTE_ADDRESS)?;
    let to = PAYNOTE_ADDRESS;
    call(
        storage,
        asset,
        IERC20::transferFromCall { from, to, amount },
    )?;
    let after = balance_of(storage, asset, PAYNOTE_ADDRESS)?;
    if after.checked_sub(before) != Some(amount) {
        return Err(PayNoteError::TransferAmountMismatch.into());
    }
    Ok(())
}

pub(crate) fn approve(
    storage: &StorageHandle<'_>,
    asset: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    call(storage, asset, IERC20::approveCall { spender, amount })
}

/// Pay `amount` of `asset` from the pool to `to`.
pub(crate) fn transfer(
    storage: &StorageHandle<'_>,
    asset: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    call(storage, asset, IERC20::transferCall { to, amount })
}

/// The call succeeds if it returns empty data or `true`. Some tokens return no
/// data.
fn call<C: SolCall>(storage: &StorageHandle<'_>, asset: Address, call: C) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(PayNoteError::TokenCallFailed.into());
    }
    Ok(())
}

fn balance_of(storage: &StorageHandle<'_>, asset: Address, account: Address) -> Result<U256> {
    let ret = storage.staticcall(asset, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PayNoteError::TokenCallFailed.into())
}
