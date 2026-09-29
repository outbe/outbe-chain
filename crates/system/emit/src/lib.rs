//! Emit private-note tree precompile (`0x…EE13`).
//!
//! One native precompile exposing `burn`, `mint`, and four tree-state views
//! (see `contracts/precompiles/src/IEmit.sol`). Burn is a runtime-only
//! native-COEN transition that derives a chain-ID- and amount-bound note
//! commitment from a caller-supplied serial and the credited value; mint
//! consumes the frozen `outbe.emit.mint@1.5.0` UltraHonkKeccak proof to
//! nullify a note, credit a payout, and append the circuit-derived
//! deterministic change commitment.
//!
//! The hash/tree formulas mirror the frozen noir circuit's `emit.nr`; the
//! runtime is reimplemented over persistent EVM storage rather than porting
//! the PoC's in-memory clear-witness ledger.
//!
//! Layout: [`hash`] (formula mirror), [`schema`] (frozen V1 storage table),
//! [`runtime`] (transition core shared by dispatch and tests), [`precompile`]
//! (ABI dispatch, payable policy, selector-sensitive gas), [`errors`].

pub mod errors;
pub use outbe_zk_canonical::emit_mint::{hash, Field};
pub mod precompile;
pub mod runtime;
pub mod schema;

/// Canonical big-endian BN254 words for Emit proofs.
pub struct EmitSuite;

impl EmitSuite {
    pub fn field_to_b256(value: &Field) -> Result<alloy_primitives::B256, outbe_protocol::Error> {
        outbe_protocol::codec::field_to_b256(value)
    }

    pub fn field_from_b256(value: &alloy_primitives::B256) -> Result<Field, outbe_protocol::Error> {
        outbe_protocol::codec::field_from_b256(value)
    }
}

/// In-memory commitment tree for Emit clients.
pub use outbe_zk_canonical::emit_mint::Tree as EmitTree;

#[cfg(test)]
mod tests;
