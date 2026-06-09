//! Precompile ABI dispatch helpers.
//!
//! Provides ergonomic helpers for routing ABI-encoded calldata to contract methods.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;

use crate::error::{PrecompileError, Result};

/// Precompile call output (matches revm::precompile::PrecompileOutput shape).
pub struct PrecompileOutput {
    pub bytes: Bytes,
    pub gas_used: u64,
}

/// Dispatches ABI-encoded calldata through a decoder and handler.
///
/// 1. Validates calldata length (>= 4 bytes for selector)
/// 2. Decodes via `decode_fn` into an enum
/// 3. Passes decoded call to `handler_fn`
pub fn dispatch_call<T, E: core::fmt::Display>(
    calldata: &[u8],
    decode: impl FnOnce(&[u8]) -> core::result::Result<T, E>,
    handler: impl FnOnce(T) -> Result<Bytes>,
) -> Result<Bytes> {
    if calldata.len() < 4 {
        return Err(PrecompileError::Revert(
            "invalid input: missing function selector".into(),
        ));
    }
    let call =
        decode(calldata).map_err(|e| PrecompileError::Revert(format!("decode error: {e}")))?;
    handler(call)
}

/// View helper: calls a read-only function and ABI-encodes the return value.
///
/// Usage: `view(decoded_call, |c| contract.balance_of(c.account))`
#[inline]
pub fn view<T: SolCall>(call: T, f: impl FnOnce(T) -> Result<T::Return>) -> Result<Bytes> {
    let ret = f(call)?;
    Ok(Bytes::from(T::abi_encode_returns(&ret)))
}

/// Metadata helper: calls a no-arg function and ABI-encodes the return value.
///
/// Usage: `metadata::<nameCall>(|| Ok(contract.name().to_string()))`
#[inline]
pub fn metadata<T: SolCall>(f: impl FnOnce() -> Result<T::Return>) -> Result<Bytes> {
    let ret = f()?;
    Ok(Bytes::from(T::abi_encode_returns(&ret)))
}

/// Mutate helper: calls a state-changing function with caller address, ABI-encodes return value.
///
/// Usage: `mutate(decoded_call, caller, |sender, c| contract.mine_coen(sender, c.amount))`
#[inline]
pub fn mutate<T: SolCall>(
    call: T,
    sender: Address,
    f: impl FnOnce(Address, T) -> Result<T::Return>,
) -> Result<Bytes> {
    let ret = f(sender, call)?;
    Ok(Bytes::from(T::abi_encode_returns(&ret)))
}

/// Mutate-void helper: calls a state-changing function that returns no value.
///
/// Usage: `mutate_void(decoded_call, caller, |sender, c| contract.set_qualified(...))`
#[inline]
pub fn mutate_void<T: SolCall>(
    call: T,
    sender: Address,
    f: impl FnOnce(Address, T) -> Result<()>,
) -> Result<Bytes> {
    f(sender, call)?;
    Ok(Bytes::new())
}

/// Mutate-void payable helper: calls a state-changing function that accepts msg.value.
///
/// Similar to [`mutate_void`] but also passes `value` (msg.value) to the handler.
/// Usage: `mutate_void_payable(decoded_call, caller, value, |sender, c, val| contract.stake(sender, c.validator, val))`
#[inline]
pub fn mutate_void_payable<T: SolCall>(
    call: T,
    sender: Address,
    value: U256,
    f: impl FnOnce(Address, T, U256) -> Result<()>,
) -> Result<Bytes> {
    f(sender, call, value)?;
    Ok(Bytes::new())
}

/// Rejects calls with non-zero msg.value for non-payable functions.
#[inline]
pub fn reject_value(value: &U256) -> Result<()> {
    if !value.is_zero() {
        return Err(PrecompileError::Revert(
            "non-payable function called with value".into(),
        ));
    }
    Ok(())
}
