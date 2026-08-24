//! Outbe `DispatchFn` adapter for the Emit precompile, the payable-selector
//! policy, the mint proof preflight, and the selector-sensitive base gas.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate_void, mutate_void_payable, reject_value_unless_payable,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_zkproof::EMIT_MINT_MAX_COMBINED_LEN;

use crate::runtime::{self, MintStatement};

/// Selectors on the Emit precompile (`0x…EE12`) that accept native value: only
/// `burn`. The route table binds this list to the address's `ValuePolicy` at
/// compile time; `mint` refuses any credited value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IEmit::burnCall::SELECTOR];

/// Base gas for `burn`: chain-pool derivation, the in-memory zero ladder,
/// commitment derivation, and one depth-20 append.
pub const EMIT_BURN_BASE_GAS: u64 = 530_000;

/// Base gas for `mint`: one UltraHonkKeccak verification plus chain-pool
/// derivation, the zero ladder, and a worst-case change append.
pub const EMIT_MINT_BASE_GAS: u64 = outbe_zkproof::constants::ZK_VERIFY_GAS + 517_500;

sol! {
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IEmit.sol"
}

/// Mint calldata head length in bytes: eight 32-byte words (seven static
/// arguments plus the dynamic `proof` offset word).
const MINT_HEAD_LEN: usize = 8 * 32;
/// The canonical ABI offset of the dynamic `proof` argument.
const MINT_PROOF_OFFSET: usize = MINT_HEAD_LEN;

/// Dispatches an ABI-encoded call to the Emit precompile.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // Emit is a payable route; every selector the module has not published
    // refuses value here, so only `burn` can carry it.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    preflight_mint_proof(data)?;
    dispatch_call(data, IEmit::IEmitCalls::abi_decode, |call| {
        use IEmit::IEmitCalls::*;
        match call {
            burn(c) => {
                mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |caller, c, val| {
                    runtime::burn(storage, caller, val, c.noteSn)
                })
            }
            mint(c) => mutate_void(c, caller, |caller, c| {
                runtime::mint(
                    storage,
                    caller,
                    c.payoutRecipient,
                    MintStatement {
                        pool_id: c.poolId,
                        root: c.root,
                        nullifier: c.nullifier,
                        note_owner: c.noteOwner,
                        mint_units: c.mintUnits,
                        change_commitment: c.changeCommitment,
                    },
                    c.proof.as_ref(),
                )
            }),
        }
    })
}

/// Allocation-free preflight of the dynamic `proof` argument on the mint
/// selector, before Alloy decodes (and copies) the payload.
///
/// Requires the canonical eight-word head (`offset == 256`), reads the length
/// word without allocation, and rejects lengths over
/// [`EMIT_MINT_MAX_COMBINED_LEN`] or lengths whose padded tail would reach
/// outside the calldata. Alloy performs full padding validation afterwards.
fn preflight_mint_proof(calldata: &[u8]) -> Result<()> {
    if calldata.get(..4) != Some(&IEmit::mintCall::SELECTOR[..]) {
        return Ok(());
    }
    let malformed = |reason: &'static str| {
        PrecompileError::Revert(format!("Emit mint proof is malformed: {reason}"))
    };
    let args = calldata
        .get(4..)
        .ok_or_else(|| malformed("missing proof argument"))?;
    let offset_word = args
        .get(7 * 32..8 * 32)
        .ok_or_else(|| malformed("missing proof offset word"))?;
    let offset = read_right_aligned_usize(offset_word)
        .ok_or_else(|| malformed("proof offset is not a right-aligned word"))?;
    if offset != MINT_PROOF_OFFSET {
        return Err(malformed("non-canonical proof offset"));
    }
    let length_word = args
        .get(offset..offset + 32)
        .ok_or_else(|| malformed("missing proof length word"))?;
    let length = read_right_aligned_usize(length_word)
        .ok_or_else(|| malformed("proof length is not a right-aligned word"))?;
    if length == 0 || length > EMIT_MINT_MAX_COMBINED_LEN {
        return Err(malformed("proof length outside the accepted range"));
    }
    let padded_len = length.div_ceil(32) * 32;
    let tail_end = offset + 32 + padded_len;
    if tail_end > args.len() {
        return Err(malformed("proof section is truncated"));
    }
    Ok(())
}

/// Reads a right-aligned `usize` from a 32-byte big-endian word; `None` when
/// the upper bytes are non-zero (the value could not index this calldata).
fn read_right_aligned_usize(word: &[u8]) -> Option<usize> {
    debug_assert_eq!(word.len(), 32);
    if word[..24].iter().any(|&byte| byte != 0) {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..32]);
    usize::try_from(u64::from_be_bytes(buf)).ok()
}

/// Base gas charged by the registry before invoking [`dispatch`]:
/// selector-sensitive, mirroring the two methods' fixed costs.
pub fn base_gas(input: &[u8]) -> u64 {
    if input.get(..4) == Some(&IEmit::mintCall::SELECTOR[..]) {
        EMIT_MINT_BASE_GAS
    } else {
        EMIT_BURN_BASE_GAS
    }
}
