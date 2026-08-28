//! Outbe `DispatchFn` adapter for the Emit precompile, the payable-selector
//! policy, and the selector-sensitive base gas.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate_void, mutate_void_payable, reject_value_unless_payable,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::{self, MintStatement};

/// Selectors on the Emit precompile (`0x…EE13`) that accept native value: only
/// `burn`. The route table binds this list to the address's `ValuePolicy` at
/// compile time; `mint` refuses any credited value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[IEmit::burnCall::SELECTOR];

/// Base gas for `burn`: chain-ID absorption, the in-memory zero ladder,
/// commitment derivation, and one depth-20 append.
pub const EMIT_BURN_BASE_GAS: u64 = 530_000;

/// Base gas for `mint`: one UltraHonkKeccak verification plus chain-ID
/// absorption, the zero ladder, and a worst-case change append.
pub const EMIT_MINT_BASE_GAS: u64 = outbe_zkproof::constants::ZK_VERIFY_GAS + 517_500;

sol! {
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IEmit.sol"
}

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
    // Alloy performs all ABI framing validation; the frozen circuit's exact
    // proof length is enforced by the zkproof decoder (`EMIT_MINT_COMBINED_LEN`).
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
                        chain_id: c.chainId,
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

/// Base gas charged by the registry before invoking [`dispatch`]:
/// selector-sensitive, mirroring the two methods' fixed costs.
pub fn base_gas(input: &[u8]) -> u64 {
    match input.first_chunk::<4>() {
        Some(&IEmit::mintCall::SELECTOR) => EMIT_MINT_BASE_GAS,
        Some(&IEmit::burnCall::SELECTOR) => EMIT_BURN_BASE_GAS,
        _ => u64::MAX, // unknown selector: fail the call with out-of-gas
    }
}
