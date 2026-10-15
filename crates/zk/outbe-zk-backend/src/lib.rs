//! Noir proving backend for the canonical Outbe circuits.
//!
//! Layering: the circuit *seams* live in `outbe-zk`
//! ([`Circuit`](outbe_zk::protocol::zk::Circuit) with its prove-side
//! `witness_inputs` and verify-side `public_inputs`, plus
//! [`CircuitId`](outbe_zk::protocol::zk::CircuitId)). The proving
//! core is **generic over any circuit** implementing those seams.
//! Noir-specific coupling stays here: a shared ACVM witness-solving core
//! ([`witness`]) and the [`barretenberg`] UltraHonkKeccak prover/verifier (FFI,
//! on-device). Concrete circuit layouts and public-input decoders live in
//! `outbe-zk`.
//!
//! Both consume [`witness::solved_witness`] (the ACVM-solved witness) and
//! [`witness::public_inputs`], and implement the core
//! [`ProofGenerator`](outbe_zk::protocol::zk::ProofGenerator) /
//! [`ProofVerifier`](outbe_zk::protocol::zk::ProofVerifier) seams.

pub mod barretenberg;
pub mod errors;
pub mod witness;
