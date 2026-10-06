//! PayNote — shielded ERC20 note pool (`0x…1019`).
//!
//! A **deposit** does these steps:
//! 1. It pulls an ERC20 from the caller.
//! 2. It routes the ERC20 into the asset's reserve vault through VaultRouter.
//! 3. It appends a note commitment to a depth-32 incremental Merkle tree.
//!
//! The runtime derives the commitment from the transfer it actually performed.
//! The circuit binds the asset in the *commitment* rather than the serial
//! precisely so the pool can do this. The binding also exists so a depositor
//! cannot fund a note in a cheap token and spend it as an expensive one.
//!
//! A **spend** consumes the frozen `outbe.paynote@1.3.0` UltraHonkKeccak proof.
//! The proof:
//! - proves membership under an accepted root.
//! - publishes a nullifier.
//! - for a partial spend, publishes the deterministic change commitment.
//!
//! Notes are bearer instruments: spend authority is knowledge of the note spend
//! key, not an address. The proof's `context` word is an opaque settlement
//! statement. The caller recomputes it. The crate exposes spending only as
//! [`api::consume`], an in-process Rust entry point for other precompile
//! modules. That entry point is not on the Solidity ABI and it moves no tokens.
//! It returns the validated claim for the caller to settle.
//!
//! The hash/tree formulas mirror the frozen noir circuit's `paynote.nr`. The
//! runtime operates over persistent EVM storage.
//!
//! Layout: [`hash`] (formula mirror), [`schema`] (frozen V1 storage table),
//! [`runtime`] (transition core), [`api`] (cross-module surface),
//! [`precompile`] (ABI dispatch, value policy, selector-sensitive gas),
//! [`errors`], [`client`] (off-chain membership witnesses).

pub mod api;
pub mod client;
pub mod context;
pub mod errors;
pub use outbe_zk_canonical::paynote::{hash, Field};
pub mod precompile;
pub mod runtime;
pub mod schema;
mod sol_ext;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use api::PayNoteClaim;
pub use schema::PayNoteContract;

/// In-memory commitment tree for PayNote clients.
pub use outbe_zk_canonical::paynote::Tree as PayNoteTree;

#[cfg(test)]
mod tests;
