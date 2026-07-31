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

/// Rejects a selected ABI call when one dynamic `bytes` argument does not have
/// the protocol-required fixed length.
///
/// This inspects only the ABI head and length word. It is intended for fixed
/// protocol identities that must be rejected before the general ABI decoder
/// allocates their dynamic payload.
pub fn preflight_dynamic_bytes_len(
    calldata: &[u8],
    selector: [u8; 4],
    argument_index: usize,
    head_words: usize,
    expected_len: usize,
) -> Result<()> {
    if calldata.get(..4) != Some(selector.as_slice()) {
        return Ok(());
    }

    let args = calldata
        .get(4..)
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    let head_len = head_words
        .checked_mul(32)
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    let offset_start = argument_index
        .checked_mul(32)
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    let offset_word = args
        .get(offset_start..offset_start.saturating_add(32))
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    let offset = abi_usize(offset_word)
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    if offset < head_len || offset % 32 != 0 {
        return Err(PrecompileError::Revert("invalid ABI bytes argument".into()));
    }

    let length_word = args
        .get(offset..offset.saturating_add(32))
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    if abi_usize(length_word) != Some(expected_len) {
        return Err(PrecompileError::Revert(format!(
            "invalid bytes length: expected {expected_len}"
        )));
    }

    let padded_len = expected_len
        .checked_add(31)
        .map(|len| len / 32 * 32)
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    let end = offset
        .checked_add(32)
        .and_then(|start| start.checked_add(padded_len))
        .ok_or_else(|| PrecompileError::Revert("invalid ABI bytes argument".into()))?;
    if end > args.len() {
        return Err(PrecompileError::Revert("invalid ABI bytes argument".into()));
    }
    Ok(())
}

fn abi_usize(word: &[u8]) -> Option<usize> {
    let width = core::mem::size_of::<usize>();
    if word.len() != 32 || word[..32 - width].iter().any(|byte| *byte != 0) {
        return None;
    }
    let mut value = [0_u8; core::mem::size_of::<usize>()];
    value.copy_from_slice(&word[32 - width..]);
    Some(usize::from_be_bytes(value))
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

#[cfg(test)]
mod tests {
    use super::preflight_dynamic_bytes_len;

    const SELECTOR: [u8; 4] = [0x12, 0x34, 0x56, 0x78];

    fn one_bytes_arg(length: usize) -> Vec<u8> {
        let padded = length.div_ceil(32) * 32;
        let mut calldata = vec![0_u8; 4 + 32 + 32 + padded];
        calldata[..4].copy_from_slice(&SELECTOR);
        calldata[4 + 31] = 32;
        calldata[4 + 32 + 31] = u8::try_from(length).unwrap();
        calldata
    }

    #[test]
    fn fixed_dynamic_bytes_preflight_accepts_only_the_required_length() {
        assert!(preflight_dynamic_bytes_len(&one_bytes_arg(36), SELECTOR, 0, 1, 36).is_ok());
        assert!(preflight_dynamic_bytes_len(&one_bytes_arg(35), SELECTOR, 0, 1, 36).is_err());
        assert!(preflight_dynamic_bytes_len(&one_bytes_arg(37), SELECTOR, 0, 1, 36).is_err());
    }

    #[test]
    fn fixed_dynamic_bytes_preflight_rejects_malformed_head_without_decoding_payload() {
        let mut points_into_head = one_bytes_arg(36);
        points_into_head[4 + 31] = 0;
        assert!(preflight_dynamic_bytes_len(&points_into_head, SELECTOR, 0, 1, 36).is_err());

        let truncated = &one_bytes_arg(36)[..4 + 32 + 32 + 35];
        assert!(preflight_dynamic_bytes_len(truncated, SELECTOR, 0, 1, 36).is_err());

        let unrelated = one_bytes_arg(35);
        assert!(preflight_dynamic_bytes_len(&unrelated, [0, 0, 0, 0], 0, 1, 36).is_ok());
    }
}
