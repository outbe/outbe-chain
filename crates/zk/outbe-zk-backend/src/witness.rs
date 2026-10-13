//! ACVM witness core: lower a circuit's typed witness into the noir
//! [`WitnessMap`] and ACVM-solve the committed bytecode into the full witness a
//! prover consumes. Shared by every backend in this crate.
//!
//! This is the acir/acvm half of the "exact mapping". The witness-index layout
//! comes from [`Circuit::witness_inputs`] (fixed by the ABI, impl'd in the
//! canonical crate); here we turn it into a real [`WitnessMap`] and run the
//! solver. The toolchain's `acir_field` is itself
//! `GenericFieldElement<ark_bn254::Fr>` on ark 0.6 — the same field this
//! workspace uses — so the only coupling is that layout.

use acir::circuit::Program;
use acir::native_types::{Witness, WitnessMap, WitnessStack};
use acir::FieldElement;
use acvm::pwg::{ACVMStatus, ACVM};
use ark_ff::{BigInteger, PrimeField};
use base64::Engine;
use bn254_blackbox_solver::Bn254BlackBoxSolver;
use flate2::read::GzDecoder;
use std::io::Read;

use outbe_zk::error::Error;
use outbe_zk::protocol::zk::{Circuit, CircuitId};
use outbe_zk::Fr;

/// Convert the protocol's BN254 field into the toolchain's field representation.
fn to_field(f: &Fr) -> FieldElement {
    FieldElement::from_repr(*f)
}

/// Lower a circuit's typed `(witness, public)` into the ACVM initial
/// [`WitnessMap`]: every ABI input flattened in witness-index order
/// ([`Circuit::witness_inputs`]), each at its `Witness(i)` slot.
pub fn witness_map<C>(witness: &C::Witness, public: &C::PublicInputs) -> WitnessMap<FieldElement>
where
    C: Circuit,
{
    let mut map = WitnessMap::new();
    for (i, f) in C::witness_inputs(witness, public).iter().enumerate() {
        map.insert(Witness(i as u32), to_field(f));
    }
    map
}

/// Decode a circuit's committed ACIR bytecode into a runnable [`Program`].
fn program<C: CircuitId>() -> Result<Program<FieldElement>, Error> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(C::BYTECODE_B64)
        .map_err(|e| Error::Proof(format!("acir bytecode base64: {e}")))?;
    Program::deserialize_program(&bytes).map_err(|e| Error::Proof(format!("acir deserialize: {e}")))
}

/// Build the initial witness, ACVM-solve the committed bytecode, and return the
/// fully-solved witness serialized in the toolchain's [`WitnessStack`] format —
/// the bytes a prover backend takes. Errors via [`Error::Proof`] if the circuit
/// is unsatisfiable for these inputs (e.g. a bad signature or a mismatched
/// owner), so this doubles as an in-protocol witness check.
pub fn solved_witness<C>(witness: &C::Witness, public: &C::PublicInputs) -> Result<Vec<u8>, Error>
where
    C: Circuit + CircuitId,
{
    let program = program::<C>()?;
    let circuit = program
        .functions
        .first()
        .ok_or_else(|| Error::Proof("acir program has no functions".into()))?;

    let solver = Bn254BlackBoxSolver;
    let mut acvm = ACVM::new(
        &solver,
        &circuit.opcodes,
        witness_map::<C>(witness, public),
        &program.unconstrained_functions,
        &circuit.assert_messages,
    );
    match acvm.solve() {
        ACVMStatus::Solved => {}
        other => return Err(Error::Proof(format!("acvm solve: {other}"))),
    }

    let mut stack = WitnessStack::default();
    stack.push(0, acvm.finalize());
    // `WitnessStack::serialize()` gzip-compresses; barretenberg's FFI wants the
    // raw (uncompressed) msgpack witness, so decompress before returning.
    let compressed = stack
        .serialize()
        .map_err(|e| Error::Proof(format!("witness serialize: {e}")))?;
    let mut raw = Vec::new();
    GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut raw)
        .map_err(|e| Error::Proof(format!("witness decompress: {e}")))?;
    Ok(raw)
}

/// The public inputs as canonical 32-byte big-endian buffers, in circuit order
/// — the verify-side values ([`Circuit::public_inputs`]) a prover backend
/// checks the proof against, in the `Vec<Vec<u8>>` shape barretenberg consumes.
///
/// One materialization per field (field → padded 32-byte `Vec`); `to_bytes_be`
/// is minimal-length, so each is left-padded to 32.
pub fn public_inputs<C>(public: &C::PublicInputs) -> Vec<Vec<u8>>
where
    C: Circuit,
{
    C::public_inputs(public)
        .iter()
        .map(|f| {
            let be = f.into_bigint().to_bytes_be();
            let mut out = vec![0u8; 32];
            out[32 - be.len()..].copy_from_slice(&be);
            out
        })
        .collect()
}
