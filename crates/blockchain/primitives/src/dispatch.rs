//! Precompile ABI dispatch helpers.
//!
//! Provides helpers that route ABI-encoded calldata to contract methods.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;

use crate::error::{PrecompileError, Result};
use crate::storage::StorageHandle;

/// Check frame authority before any command validation, reads or child calls.
pub fn ensure_mutation_allowed(storage: &StorageHandle<'_>) -> Result<()> {
    if storage.is_static()? {
        return Err(PrecompileError::WriteProtection);
    }
    Ok(())
}

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
/// Usage: `mutate(&storage, decoded_call, caller, |sender, c| contract.mine_coen(sender, c.amount))`
#[inline]
pub fn mutate<T: SolCall>(
    storage: &StorageHandle<'_>,
    call: T,
    sender: Address,
    f: impl FnOnce(Address, T) -> Result<T::Return>,
) -> Result<Bytes> {
    typed::mutate(storage, call, sender, f)
}

/// Shared typed dispatch, adapted from Tempo's `dispatch::typed::mutate`.
pub mod typed {
    use super::*;

    /// Reject STATICCALL before entering the command, then encode its typed return.
    #[inline]
    pub fn mutate<T: SolCall, R: Into<T::Return>>(
        storage: &StorageHandle<'_>,
        call: T,
        sender: Address,
        f: impl FnOnce(Address, T) -> Result<R>,
    ) -> Result<Bytes> {
        ensure_mutation_allowed(storage)?;
        let ret = f(sender, call)?;
        Ok(T::abi_encode_returns(&ret.into()).into())
    }
}

/// Mutate-void helper: calls a state-changing function that returns no value.
///
/// Usage: `mutate_void(&storage, decoded_call, caller, |sender, c| contract.set_qualified(...))`
#[inline]
pub fn mutate_void<T: SolCall>(
    storage: &StorageHandle<'_>,
    call: T,
    sender: Address,
    f: impl FnOnce(Address, T) -> Result<()>,
) -> Result<Bytes>
where
    T::Return: From<()>,
{
    typed::mutate(storage, call, sender, f)
}

/// Mutate-void payable helper: calls a state-changing function that accepts msg.value.
///
/// Similar to [`mutate_void`] but also passes `value` (msg.value) to the handler.
///
/// `payable_selectors` is the calling module's `PAYABLE_SELECTORS`. This helper refuses a
/// funded call to a selector that is missing from that list and does not hand it the value.
/// The route table binds the list to the address's value policy. Such a selector would thus
/// consume value that the boundary never authorized for this address. A zero-value call still
/// dispatches, as in [`reject_value_unless_payable`]. The helper refuses the value of an
/// undeclared selector, but does not disable the selector outright.
#[inline]
pub fn mutate_void_payable<T: SolCall>(
    storage: &StorageHandle<'_>,
    call: T,
    payable_selectors: &[[u8; 4]],
    sender: Address,
    value: U256,
    f: impl FnOnce(Address, T, U256) -> Result<()>,
) -> Result<Bytes>
where
    T::Return: From<()>,
{
    typed::mutate(storage, call, sender, |sender, call| {
        if !value.is_zero() && !payable_selectors.contains(&T::SELECTOR) {
            return Err(PrecompileError::Revert(
                "payable selector is not declared in PAYABLE_SELECTORS".into(),
            ));
        }
        f(sender, call, value)
    })
}

/// Mutate payable helper: a state-changing function that accepts msg.value and
/// returns a value.
///
/// Same as [`mutate_void_payable`], but with a return value. The same
/// `payable_selectors` guard applies. The guard refuses the value of an undeclared
/// selector and does not hand the value to it.
#[inline]
pub fn mutate_payable<T: SolCall>(
    storage: &StorageHandle<'_>,
    call: T,
    payable_selectors: &[[u8; 4]],
    sender: Address,
    value: U256,
    f: impl FnOnce(Address, T, U256) -> Result<T::Return>,
) -> Result<Bytes> {
    typed::mutate(storage, call, sender, |sender, call| {
        if !value.is_zero() && !payable_selectors.contains(&T::SELECTOR) {
            return Err(PrecompileError::Revert(
                "payable selector is not declared in PAYABLE_SELECTORS".into(),
            ));
        }
        f(sender, call, value)
    })
}

/// Refuses native value for any selector the module has not published as
/// payable.
///
/// A module reaches this only because its address's route declares
/// `ValuePolicy::Payable`, which the route table binds to `payable_selectors` at
/// compile time. This function checks the raw selector against that same list.
/// This makes value default-denied for the whole module. A selector added later
/// takes no value until the module publishes it. Each non-payable arm thus does
/// not have to remember a [`reject_value`] call of its own.
#[inline]
pub fn reject_value_unless_payable(
    calldata: &[u8],
    payable_selectors: &[[u8; 4]],
    value: &U256,
) -> Result<()> {
    if value.is_zero() {
        return Ok(());
    }
    let published = calldata
        .get(..4)
        .is_some_and(|selector| payable_selectors.iter().any(|entry| entry == selector));
    if published {
        return Ok(());
    }
    reject_value(value)
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

#[cfg(test)]
mod payable_witness_tests {
    use alloy_primitives::{Address, U256};
    use alloy_sol_types::{sol, SolCall};

    use super::mutate_void_payable;
    use crate::error::PrecompileError;
    use crate::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    sol! {
        interface IWitness {
            function fund(uint256 amount) external payable;
            function note() external;
        }
    }

    /// The route table binds a module's `PAYABLE_SELECTORS` to its address's
    /// value policy at compile time. A selector that forwards value without
    /// appearing in that list would take value that the boundary never authorized
    /// for the address. Its own call site therefore refuses it.
    #[test]
    fn undeclared_payable_selector_is_refused() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let call = IWitness::fundCall {
            amount: U256::from(1u64),
        };
        let refused = mutate_void_payable(
            &storage,
            call,
            &[],
            Address::ZERO,
            U256::from(1u64),
            |_, _, _| panic!("handler must not run for an undeclared selector"),
        );
        assert!(matches!(refused, Err(PrecompileError::Revert(_))));
    }

    /// The module-wide default-deny: on a payable address, every selector that the
    /// module has not published refuses value. A new value-consuming arm thus
    /// takes nothing until the module declares it. There is no per-arm check to forget.
    #[test]
    fn unpublished_selector_refuses_value_on_a_payable_module() {
        use super::reject_value_unless_payable;

        let published = &[IWitness::fundCall::SELECTOR];
        let other = IWitness::noteCall {}.abi_encode();

        assert!(matches!(
            reject_value_unless_payable(&other, published, &U256::from(1u64)),
            Err(PrecompileError::Revert(_))
        ));
        assert!(reject_value_unless_payable(&other, published, &U256::ZERO).is_ok());

        let funded = IWitness::fundCall {
            amount: U256::from(1u64),
        }
        .abi_encode();
        assert!(reject_value_unless_payable(&funded, published, &U256::from(1u64)).is_ok());

        // Calldata too short to carry a selector is not published either.
        assert!(matches!(
            reject_value_unless_payable(&[0u8; 3], published, &U256::from(1u64)),
            Err(PrecompileError::Revert(_))
        ));
    }

    #[test]
    fn declared_payable_selector_reaches_the_handler() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let call = IWitness::fundCall {
            amount: U256::from(1u64),
        };
        let mut seen = U256::ZERO;
        mutate_void_payable(
            &storage,
            call,
            &[IWitness::fundCall::SELECTOR],
            Address::ZERO,
            U256::from(7u64),
            |_, _, value| {
                seen = value;
                Ok(())
            },
        )
        .expect("declared selector must dispatch");
        assert_eq!(seen, U256::from(7u64));
    }

    #[test]
    fn all_command_helpers_refuse_static_before_entering_the_closure() {
        use super::{mutate, mutate_payable, mutate_void, view};
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_static(true);
        let storage = StorageHandle::new(&mut provider);
        let a = Address::ZERO;
        let outcomes = [
            mutate(&storage, IWitness::noteCall {}, a, |_, _| {
                panic!("static handler entered")
            }),
            mutate_void(&storage, IWitness::noteCall {}, a, |_, _| {
                panic!("static handler entered")
            }),
            mutate_payable(
                &storage,
                IWitness::fundCall { amount: U256::ZERO },
                &[],
                a,
                U256::ZERO,
                |_, _, _| panic!("static handler entered"),
            ),
            mutate_void_payable(
                &storage,
                IWitness::fundCall { amount: U256::ZERO },
                &[],
                a,
                U256::ZERO,
                |_, _, _| panic!("static handler entered"),
            ),
        ];
        for outcome in outcomes {
            assert!(matches!(outcome, Err(PrecompileError::WriteProtection)));
        }
        assert!(view(IWitness::noteCall {}, |_| Ok(IWitness::noteReturn {})).is_ok());
    }
}
