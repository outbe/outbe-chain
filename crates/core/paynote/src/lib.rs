//! PayNote — shielded ERC20 note pool (`0x…1019`).
//!
//! A **deposit** pulls an ERC20 from the caller, routes it into the asset's
//! reserve vault through VaultRouter, and appends a note commitment to a
//! depth-32 incremental Merkle tree. The commitment is derived by the runtime
//! from the transfer it actually performed — the circuit binds the asset in the
//! *commitment* rather than the serial precisely so the pool can do this, and
//! so a depositor cannot fund a note in a cheap token and spend it as an
//! expensive one.
//!
//! A **spend** consumes the frozen `outbe.paynote@1.3.0` UltraHonkKeccak proof:
//! it proves membership under an accepted root, publishes a nullifier, and — for
//! a partial spend — the deterministic change commitment. Notes are bearer
//! instruments: spend authority is knowledge of the note spend key, not an
//! address. The proof's `context` word is an opaque settlement statement; the
//! caller recomputes it. Spending is exposed only as [`api::consume`], an
//! in-process Rust entry point for other precompile modules; it is not on the
//! Solidity ABI and it moves no tokens, returning the validated claim for the
//! caller to settle.
//!
//! The hash/tree formulas mirror the frozen noir circuit's `paynote.nr`; the
//! runtime is implemented over persistent EVM storage.
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

/// Canonical big-endian BN254 words for PayNote proofs.
pub struct PayNoteSuit;

impl PayNoteSuit {
    pub fn field_to_b256(value: &Field) -> Result<alloy_primitives::B256, outbe_protocol::Error> {
        outbe_protocol::codec::field_to_b256(value)
    }

    pub fn field_from_b256(value: &alloy_primitives::B256) -> Result<Field, outbe_protocol::Error> {
        outbe_protocol::codec::field_from_b256(value)
    }
}

/// In-memory commitment tree for PayNote clients.
pub use outbe_zk_canonical::paynote::Tree as PayNoteTree;

#[cfg(test)]
mod tests;
