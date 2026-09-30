//! Outbe `DispatchFn` adapter for the PayNote precompile, the payable-selector
//! policy, and the selector-sensitive base gas.
//!
//! Deposits, proof-authorized merges and views are public. Settlement spending
//! remains the Rust-only [`crate::api::consume`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::PayNoteContract;

/// Selectors on the PayNote precompile that accept native value: none.
/// `deposit` moves ERC20, not native COEN, so the route table binds this
/// address to `ValuePolicy::Reject`.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Base gas for `deposit`: chain-ID absorption, the in-memory depth-32 zero
/// ladder, commitment derivation, one depth-32 append, and the ERC20 +
/// VaultRouter sub-calls (which meter their own execution on top).
pub const PAYNOTE_DEPOSIT_BASE_GAS: u64 = 850_000;

/// One merge verification plus the existing conservative tree-work allowance.
/// Calldata and persistent storage operations are charged separately.
pub const PAYNOTE_MERGE_BASE_GAS: u64 =
    PAYNOTE_DEPOSIT_BASE_GAS + outbe_primitives::storage::gas::ZK_VERIFY_GAS;

/// Base gas for the read-only views: a handful of storage reads, plus the
/// root-window scan for `isKnownRoot`.
pub const PAYNOTE_VIEW_BASE_GAS: u64 = 30_000;

sol! {
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IPayNote.sol"
}

/// Dispatches an ABI-encoded call to the PayNote precompile.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(data, IPayNote::IPayNoteCalls::abi_decode, |call| {
        use IPayNote::IPayNoteCalls::*;
        match call {
            deposit(c) => mutate_void(c, caller, |caller, c| {
                runtime::deposit(storage, caller, c.asset, c.amount, c.noteSn)
            }),
            mergePayNotes(c) => mutate_void(c, caller, |_, c| {
                runtime::merge_pay_notes(&storage, &c.proof)
            }),
            maxMergeInputs(c) => view(c, |_| runtime::max_merge_inputs()),
            currentRoot(c) => view(c, |_| {
                let paynote: PayNoteContract<'_> = storage.contract();
                paynote.current_root.read()
            }),
            leafCount(c) => view(c, |_| {
                let paynote: PayNoteContract<'_> = storage.contract();
                paynote.leaf_count.read()
            }),
            isKnownRoot(c) => view(c, |c| {
                let paynote: PayNoteContract<'_> = storage.contract();
                Ok(paynote.recent_roots.read_all()?.contains(&c.root))
            }),
            isSpent(c) => view(c, |c| {
                let paynote: PayNoteContract<'_> = storage.contract();
                paynote.spent_nullifiers.read(&c.nullifier)
            }),
            hasCommitment(c) => view(c, |c| {
                let paynote: PayNoteContract<'_> = storage.contract();
                paynote.commitments.read(&c.commitment)
            }),
        }
    })
}

/// Base gas charged by the registry before invoking [`dispatch`]:
/// selector-sensitive, mirroring the methods' fixed costs.
pub fn base_gas(input: &[u8]) -> u64 {
    match input.first_chunk::<4>() {
        Some(&IPayNote::depositCall::SELECTOR) => PAYNOTE_DEPOSIT_BASE_GAS,
        Some(&IPayNote::mergePayNotesCall::SELECTOR) => PAYNOTE_MERGE_BASE_GAS,
        Some(&IPayNote::currentRootCall::SELECTOR)
        | Some(&IPayNote::maxMergeInputsCall::SELECTOR)
        | Some(&IPayNote::leafCountCall::SELECTOR)
        | Some(&IPayNote::isKnownRootCall::SELECTOR)
        | Some(&IPayNote::isSpentCall::SELECTOR)
        | Some(&IPayNote::hasCommitmentCall::SELECTOR) => PAYNOTE_VIEW_BASE_GAS,
        _ => u64::MAX, // unknown selector: fail the call with out-of-gas
    }
}
