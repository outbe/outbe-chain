//! Emit private-note pool precompile (`0x…EE12`).
//!
//! One native precompile exposing exactly two ABI methods, `burn` and `mint`
//! (see `contracts/precompiles/src/IEmit.sol`). Burn is a runtime-only
//! native-COEN transition that derives an amount-bound note commitment from a
//! caller-supplied serial and the credited value; mint consumes the frozen
//! `outbe.emit.mint@1.0.0` UltraHonkKeccak proof to nullify a note, credit a
//! payout, and append the circuit-derived deterministic change commitment.
//!
//! The hash/tree formulas mirror the PoC and the vendored noir circuit core
//! (`outbe-emit-circuit-core`); the runtime is reimplemented over persistent
//! EVM storage rather than porting the PoC's in-memory clear-witness ledger.
//!
//! Layout: [`hash`] (formula mirror), [`schema`] (frozen V1 storage table),
//! [`runtime`] (transition core shared by dispatch and tests), [`precompile`]
//! (ABI dispatch, payable policy, selector-sensitive gas), [`errors`].

pub mod errors;
pub mod hash;
pub mod precompile;
pub mod runtime;
pub mod schema;

#[cfg(test)]
mod tests;
