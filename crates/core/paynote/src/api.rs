//! Cross-module API for the PayNote pool.
//!
//! In-process Rust surface for other precompile modules (gem, nod, …). This is
//! deliberately **not** a Solidity ABI. Spending a note is a privileged
//! in-runtime transition, not something an EOA calls directly. Thus `consume`
//! never appears in `IPayNote.sol` and never routes through dispatch.
//!
//! Callers depend on this module, not on [`crate::runtime`] or
//! [`crate::state`]-level internals.

use alloy_primitives::B256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::PayNoteContract;

pub use crate::runtime::PayNoteClaim;

pub use crate::context::{intex_holding_target, settlement_context, SettlementDomain};

/// Verify a `outbe.paynote@1.3.0` spend proof, nullify the note, append any
/// change commitment, and return the validated claim.
///
/// **Moves no tokens.** PayNote owns the tree, the nullifier set and the root
/// window. The caller decides what `claim.spend_amount` of `claim.asset` buys
/// and must require `claim.context` to equal the settlement statement it
/// recomputes with [`settlement_context`].
///
/// The claim comes from the proof itself, so the caller must check asset,
/// amount, and context before acting on them. A valid proof for a different
/// statement is still a valid proof.
///
/// Reverts if one of these conditions is true:
/// - the tree is uninitialized.
/// - the chain ID does not match.
/// - the root is outside the acceptance window.
/// - the nullifier is already spent.
/// - the proof fails verification.
///
/// The nullifier write and the change append are one rollback unit with the
/// caller's own effects.
pub fn consume(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<PayNoteClaim> {
    runtime::consume(storage, proof)
}

/// Whether `root` is inside the acceptance window. Useful for pre-flighting a
/// spend before committing to the gas of full verification.
pub fn is_known_root(storage: &StorageHandle<'_>, root: B256) -> Result<bool> {
    let paynote: PayNoteContract<'_> = storage.contract();
    Ok(paynote.recent_roots.read_all()?.contains(&root))
}

/// Whether `nullifier` has already been spent.
pub fn is_spent(storage: &StorageHandle<'_>, nullifier: B256) -> Result<bool> {
    let paynote: PayNoteContract<'_> = storage.contract();
    paynote.spent_nullifiers.read(&nullifier)
}
