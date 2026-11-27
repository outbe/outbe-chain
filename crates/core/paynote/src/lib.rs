//! PayNote — shielded ERC20 note pool (`0x…1019`).
//!
//! A **deposit** does these steps:
//! 1. The pool pulls the ERC20 from the caller and keeps it.
//! 2. It appends a locked transfer note to a depth-32 incremental Merkle tree.
//!
//! The depositor names the recipient in the lock, as in a transfer. They name
//! themselves to deposit for themselves. `IPayNote.resolveTransfer` turns the
//! locked note into an ordinary note that can be spent.
//!
//! The runtime derives the commitment from the transfer it actually performed.
//! The circuit binds the asset in the *commitment* rather than the serial
//! precisely so the pool can do this. The binding also exists so a depositor
//! cannot fund a note in a cheap token and spend it as an expensive one.
//!
//! A **spend** consumes the frozen `outbe.paynote@1.4.0` UltraHonkKeccak proof.
//! The proof:
//! - proves membership under an accepted root.
//! - publishes a nullifier.
//! - for a partial spend, publishes the deterministic change commitment.
//!
//! Notes are bearer instruments: spend authority is knowledge of the note spend
//! key, not an address. The proof's `context` word is an opaque settlement
//! statement. The caller recomputes it. Only the settlement factories may
//! spend, through `IPayNote.consume`. They reach it with [`api::consume`] or
//! [`api::settle`]. A spend routes the spent amount from the pool through
//! VaultRouter into the reserve vault of the asset. Then it returns the
//! validated claim. The caller settles the claim.
//!
//! A **withdrawal** is the public spend, `IPayNote.withdraw`. The proof binds a
//! recipient through [`api::withdraw_context`]. The pool pays the spent amount
//! to that recipient, as the ERC20 of the note or as native COEN.
//!
//! A spend or a withdrawal also takes an `outbe.paynote.claim_spend` proof,
//! which spends a locked transfer note directly. Thus the holder of a deposit
//! or a transfer pays with one proof and no `IPayNote.resolveTransfer` first.
//! The rest of the locked note becomes an ordinary change note under a serial
//! the prover picks.
//!
//! A **transfer** pays another party without sharing a key. `IPayNote.transfer`
//! turns part of a note into a locked note for the recipient's public key,
//! which is the note serial of a key the recipient holds. The recipient
//! resolves it with `IPayNote.resolveTransfer`: into a note of their own, or,
//! to decline, into the sender's refund serial. After the 30-day timelock the
//! sender may resolve it too. The sender gives the recipient the note details
//! off-chain. They do not let anyone spend.
//!
//! # Address privacy
//!
//! The pool hides who owns a note. It cannot hide who sends a transaction, so
//! the two parties of a payment can still learn each other's address:
//! - The recipient knows the locked commitment. They see the sender of the
//!   `deposit` or the `transfer` that created it.
//! - The sender chose `rho`, so they know the nullifier of the locked note.
//!   They see the sender of the transaction that resolves it. With an
//!   `outbe.paynote.claim_spend` proof they also see the recipient of the
//!   withdrawal, or the caller of the settlement.
//!
//! A wallet that hides both addresses follows these rules:
//! - The recipient publishes a [`client::ReceiverBundle`], never an address.
//! - The sender deposits for themselves, then pays with `IPayNote.transfer`.
//! - A one-time key with no balance sends `transfer`, `resolveTransfer`,
//!   `mergePayNotes` and `withdraw`. The zero-fee path sponsors these calls for
//!   an account that delegates to ZeroFee.
//! - The recipient resolves the locked note first and spends the ordinary note
//!   later. The sender cannot compute the nullifier of that note.
//! - The recipient withdraws to a fresh address. Deposit and withdrawal amounts
//!   are public, so equal amounts close in time link the two ends.
//!
//! The hash/tree formulas mirror the frozen noir circuit's `paynote.nr`. The
//! runtime operates over persistent EVM storage.
//!
//! Layout: [`hash`] (formula mirror), [`schema`] (frozen V1 storage table),
//! [`runtime`] (transition core), [`api`] (cross-module surface),
//! [`precompile`] (ABI dispatch, value policy, selector-sensitive gas),
//! [`errors`], [`context`] (settlement statement), [`client`] (off-chain membership witnesses).

pub mod api;
pub mod client;
pub mod context;
pub mod errors;
pub use outbe_zk::paynote::{hash, Field};
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;
mod token;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use api::PayNoteClaim;
pub use schema::PayNoteContract;

/// In-memory commitment tree for PayNote clients.
pub use outbe_zk::paynote::Tree as PayNoteTree;

#[cfg(test)]
mod tests;
