//! Cross-module API for the PayNote pool.
//!
//! Rust surface for other precompile modules (gem, nod, …). Spending a note is
//! a privileged transition, not something an EOA calls directly. `consume`
//! reaches `IPayNote.consume` by an EVM sub-call, so the pool is `msg.sender`
//! for the token calls that route the spent value. Only the settlement
//! factories may make that call.
//!
//! Callers depend on this module, not on [`crate::runtime`] or
//! [`crate::state`]-level internals.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::PAYNOTE_ADDRESS;
use outbe_vaultrouter::settlement::Quote;

use crate::errors::PayNoteError;
use crate::precompile::IPayNote;
use crate::schema::PayNoteContract;

pub use crate::runtime::PayNoteClaim;

pub use crate::context::{intex_holding_target, settlement_context, SettlementDomain};

/// Verify a `outbe.paynote@1.3.0` spend proof and return the validated claim.
///
/// The function nullifies the note and appends the change commitment, if there
/// is one. Then it routes `claim.spend_amount` of `claim.asset` from the pool
/// into the reserve vault of the asset.
///
/// PayNote owns the tree, the nullifier set, the root window and the tokens in
/// the pool. The caller decides what `claim.spend_amount` of `claim.asset` buys
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
/// - the vault routing fails.
///
/// - the caller is not a settlement factory.
///
/// The nullifier write, the change append and the vault routing roll back
/// together with the effects of the caller.
pub fn consume(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<PayNoteClaim> {
    let ret = storage.call(
        PAYNOTE_ADDRESS,
        U256::ZERO,
        IPayNote::consumeCall {
            proof: proof.to_vec().into(),
        }
        .abi_encode()
        .into(),
    )?;
    let ret = IPayNote::consumeCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Fatal("PayNote consume returned undecodable data".into()))?;
    Ok(PayNoteClaim {
        asset: ret.asset,
        context: ret.context,
        spend_amount: ret.spendAmount,
        nullifier: ret.nullifier,
    })
}

/// Pay one settlement with a note: [`consume`] the proof, price the settlement
/// in the asset of the note with `price`, and require the proof to bind this
/// settlement and to spend exactly its cost.
///
/// `target` and `units` are the arguments of [`settlement_context`]. The
/// snapshot comes from the quote. Run it inside the checkpoint of the caller,
/// after the state transition, so a failure here rolls the transition back.
pub fn settle(
    storage: &StorageHandle<'_>,
    proof: &[u8],
    (domain, target, units): (SettlementDomain, B256, U256),
    price: impl FnOnce(Address) -> Result<Quote>,
) -> Result<(PayNoteClaim, Quote)> {
    let claim = consume(storage, proof)?;
    let quote = price(claim.asset)?;
    let expected = settlement_context(domain, target, units, quote.snapshot_id())?;
    if claim.context != expected {
        return Err(PayNoteError::ContextMismatch {
            expected,
            actual: claim.context,
        }
        .into());
    }
    // Exact: the pool routes the whole spend into the vault.
    if claim.spend_amount != quote.payment_minor {
        return Err(PayNoteError::CostMismatch {
            covered: claim.spend_amount,
            required: quote.payment_minor,
        }
        .into());
    }
    Ok((claim, quote))
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
