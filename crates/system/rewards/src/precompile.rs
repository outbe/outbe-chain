use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::dispatch::reject_value;
use outbe_primitives::error::{PrecompileError, Result};

/// Dispatches an ABI-encoded call to the Rewards precompile.
///
/// The Rewards precompile exposes **no** callable external methods. Validator
/// daily emission is delivered as gems by [`crate::api::add_topup_for_voters`]
/// (validator emission is paid in gems, not a claimable native balance), and
/// per-block fees settle internally via the `LateFinalizeCredits` begin-zone
/// phase. The contract's state is accessed only in-process through the
/// [`crate::schema::Rewards`] facade (api / lifecycle / hooks), never through
/// this inbound ABI. `REWARDS_ADDRESS` stays a preserved system account holding
/// the fee escrow, so any external call deterministically reverts.
pub fn dispatch(
    _storage: outbe_primitives::storage::StorageHandle,
    _data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    Err(PrecompileError::Revert(
        "Rewards precompile exposes no callable methods".into(),
    ))
}
