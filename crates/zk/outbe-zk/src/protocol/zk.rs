//! ZK proof backend seams, bound to a [`Circuit`].
//!
//! A [`Circuit`] is the type-level structure of a statement: it binds the
//! private *witness* to its *public inputs* (the claim) — the field
//! elements committed into the proof. The concrete
//! proving artifacts — the compiled circuit and the verifying key, plus
//! its hash — live with the backend in the zk crates (e.g.
//! `outbe-circuits-canonical`); a circuit type here is the contract those
//! backends and the protocol agree on.
//!
//! [`ProofGenerator`]/[`ProofVerifier`] are the backend seams,
//! parameterized by the circuit so a prover and a verifier for the *same*
//! `C` necessarily agree on the witness and public-input types.

use crate::error::Error;
use crate::Fr;

/// A circuit / statement: the binding between a *witness* (the private
/// inputs the circuit constrains) and the *public inputs* (the claim the
/// proof attests). The two are separate types — the public inputs are not
/// derived from the witness here; the prover supplies both.
pub trait Circuit {
    /// The private witness the circuit constrains.
    type Witness;
    /// The public inputs (the claim) the circuit exposes.
    type PublicInputs;

    /// Flatten the public inputs into field elements, in circuit order —
    /// the values (owner / hashes / binding …) bound into the proof. The
    /// *verify-side* mapping (what the on-chain verifier checks the proof
    /// against).
    fn public_inputs(public: &Self::PublicInputs) -> Vec<Fr>;

    /// Flatten the full witness — every ABI parameter (private and public, in
    /// `main()` declaration order; structs, arrays and byte elements expanded) —
    /// into field elements in ACIR witness-index order: index `i` in the
    /// returned vec is exactly ACIR `Witness(i)`. The *prove-side* mapping a noir
    /// backend lowers into a `WitnessMap`.
    ///
    /// Sits next to [`public_inputs`](Circuit::public_inputs) so the
    /// prover and verifier consume one build-derived source of truth and cannot
    /// drift from the ABI.
    fn witness_inputs(witness: &Self::Witness, public: &Self::PublicInputs) -> Vec<Fr>;
}

/// Content-derived proving artifacts and descriptive version metadata.
/// Implemented (build-generated) by canonical circuit markers; verifier routing
/// chooses the appropriate identity for its registry.
pub trait CircuitId {
    /// Canonical dotted label, e.g. `outbe.ownership` / `demo.tribute`.
    const LABEL: &'static str;
    /// Semver-style version string ("1.0.0"). Not authoritative.
    const VERSION: &'static str;
    /// `keccak256(base64_decode(bytecode))` — compiled circuit identity.
    /// L2 chain registrations pin [`CircuitId::VK_HASH`] instead.
    const CIRCUIT_HASH: [u8; 32];
    /// Base64-encoded ACIR bytecode (the preimage of [`CircuitId::CIRCUIT_HASH`]).
    const BYTECODE_B64: &'static str;
    /// Canonical UltraHonkKeccak verification key bytes, derived from the bytecode.
    const VK_BYTES: &'static [u8];
    /// Pre-computed `keccak256(VK_BYTES)`.
    const VK_HASH: [u8; 32];
}

/// A proof-generation backend for a specific [`Circuit`] — implemented by
/// the proof crate (e.g. `outbe-circuits` for noir).
pub trait ProofGenerator<C: Circuit> {
    /// The proof representation this backend produces (bytes, a noir
    /// proof, …).
    type Proof;
    /// Generate a proof: `witness` is the private inputs, `public` the
    /// claim (public inputs) the proof must attest.
    fn generate(
        &self,
        witness: &C::Witness,
        public: &C::PublicInputs,
    ) -> Result<Self::Proof, Error>;
}

/// A proof-verification backend for a specific [`Circuit`] — implemented
/// next to the verifier.
pub trait ProofVerifier<C: Circuit> {
    /// The proof representation this backend verifies.
    type Proof;
    /// Verify `proof` against `public`.
    fn verify(&self, public: &C::PublicInputs, proof: &Self::Proof) -> Result<bool, Error>;
}
