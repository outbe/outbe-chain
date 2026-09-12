//! Research driver: circuit-specific local setup, real randomized Groth16 proof.
use ark_bn254::{Bn254, Fr};
use ark_ff::Field;
use ark_groth16::{prepare_verifying_key, Groth16, Proof, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem, OptimizationGoal};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use num_bigint::BigUint;
use outbe_p_link_measurements::{
    circuit::{self, LinkCircuit, LinkPublic},
    integer::fr_integer,
    p384_gadget,
};
use rand::rngs::OsRng;
use serde_json::json;
use std::{path::PathBuf, time::Instant};

type PublicMutation = (&'static str, Box<dyn Fn(&mut LinkPublic)>);

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
fn synthesize(c: LinkCircuit, expect: bool) -> (usize, usize, usize, f64) {
    let start = Instant::now();
    let cs = ConstraintSystem::<Fr>::new_ref();
    cs.set_optimization_goal(OptimizationGoal::Constraints);
    c.generate_constraints(cs.clone()).expect("synthesis");
    assert_eq!(cs.is_satisfied().unwrap(), expect, "R1CS satisfaction");
    (
        cs.num_constraints(),
        cs.num_witness_variables(),
        cs.num_instance_variables() - 1,
        ms(start),
    )
}
fn encode<T: CanonicalSerialize>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.serialize_compressed(&mut bytes).unwrap();
    bytes
}
fn decode<T: CanonicalDeserialize>(bytes: &[u8]) -> T {
    let mut rest = bytes;
    let v = T::deserialize_compressed(&mut rest).expect("canonical serialized artifact");
    assert!(rest.is_empty(), "trailing bytes");
    v
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args.first().map(String::as_str).unwrap_or("constraints");
    let out = PathBuf::from(
        args.get(1)
            .map(String::as_str)
            .unwrap_or("/tmp/outbe-p-link"),
    );
    if mode == "verify" {
        let vk: VerifyingKey<Bn254> = decode(&std::fs::read(out.join("vk.bin")).unwrap());
        let proof: Proof<Bn254> = decode(&std::fs::read(out.join("proof.bin")).unwrap());
        let inputs: Vec<Fr> = decode(&std::fs::read(out.join("public_inputs.bin")).unwrap());
        assert_eq!(inputs.len(), 40);
        assert!(
            Groth16::<Bn254>::verify_proof(&prepare_verifying_key(&vk), &proof, &inputs).unwrap()
        );
        println!("P_link artifact verified (L2 FullProof/root gate is separate)");
        return;
    }
    let c = circuit::fixture();
    let inputs = c.public.inputs().unwrap();
    eprintln!("P_link: synthesizing complete draft/economics/P384 circuit");
    let (constraints, witness_vars, public_vars, synthesis_ms) = synthesize(c.clone(), true);
    assert_eq!(public_vars, inputs.len());
    eprintln!("P_link: satisfied; {constraints} constraints, {public_vars} public inputs; {synthesis_ms:.1} ms");
    if mode == "constraints" {
        println!(
            "{}",
            json!({"constraints":constraints,"witness_variables":witness_vars,"public_inputs":public_vars,"synthesis_and_satisfaction_ms":synthesis_ms})
        );
        return;
    }
    assert_eq!(
        mode, "bench",
        "usage: [constraints | bench DIR | verify DIR]"
    );
    eprintln!("P_link: local circuit-specific Groth16 setup (research only)");
    let start = Instant::now();
    let pk =
        Groth16::<Bn254>::generate_random_parameters_with_reduction(c.clone(), &mut OsRng).unwrap();
    let setup_ms = ms(start);
    eprintln!("P_link: setup finished in {setup_ms:.1} ms");
    let start = Instant::now();
    let pvk = prepare_verifying_key(&pk.vk);
    let prepare_vk_ms = ms(start);
    let start = Instant::now();
    let proof =
        Groth16::<Bn254>::create_random_proof_with_reduction(c.clone(), &pk, &mut OsRng).unwrap();
    let prove_ms = ms(start);
    eprintln!("P_link: proof generated in {prove_ms:.1} ms");
    let mut verifications = Vec::new();
    for i in 0..6 {
        let start = Instant::now();
        assert!(Groth16::<Bn254>::verify_proof(&pvk, &proof, &inputs).unwrap());
        if i > 0 {
            verifications.push(ms(start));
        }
    }
    verifications.sort_by(f64::total_cmp);
    let verify_ms = verifications[2];
    let proof_bytes = encode(&proof);
    let vk_bytes = encode(&pk.vk);
    let input_bytes = encode(&inputs);
    let roundtrip: Proof<Bn254> = decode(&proof_bytes);
    assert!(Groth16::<Bn254>::verify_proof(&pvk, &roundtrip, &inputs).unwrap());
    let mut rejected = Vec::new();
    let cases: Vec<PublicMutation> = vec![
        ("L2 draft hash", Box::new(|p| p.nft_hash += Fr::ONE)),
        ("L2 derived owner", Box::new(|p| p.derived_owner += Fr::ONE)),
        ("L2 binding hash", Box::new(|p| p.binding_hash += Fr::ONE)),
        ("L2 root context", Box::new(|p| p.merkle_root += Fr::ONE)),
        ("L1 sender", Box::new(|p| p.sender += BigUint::from(1u64))),
        ("chain", Box::new(|p| p.chain_id += 1)),
        ("day", Box::new(|p| p.day += 1)),
        ("currency", Box::new(|p| p.currency += 1)),
        (
            "reference currency",
            Box::new(|p| p.reference_currency += 1),
        ),
        (
            "exclusion flag",
            Box::new(|p| p.exclude_from_intex = !p.exclude_from_intex),
        ),
        (
            "oracle price",
            Box::new(|p| p.issuance_vwap += BigUint::from(1u64)),
        ),
        ("source marker", Box::new(|p| p.source_ids[0] += Fr::ONE)),
    ];
    for (label, mutate) in cases {
        let mut p = c.public.clone();
        mutate(&mut p);
        assert!(!Groth16::<Bn254>::verify_proof(&pvk, &proof, &p.inputs().unwrap()).unwrap());
        rejected.push(label.to_string());
    }
    let mut wrong = c.public.clone();
    wrong.nominal_commitment = p384_gadget::native_commit(
        &(&c.private.nominal + BigUint::from(1u64)),
        &c.private.nominal_blinding,
    )
    .unwrap();
    assert!(!Groth16::<Bn254>::verify_proof(&pvk, &proof, &wrong.inputs().unwrap()).unwrap());
    rejected.push("nominal commitment".into());
    let mut bad_proof = proof.clone();
    bad_proof.a = pk.vk.alpha_g1;
    assert!(!Groth16::<Bn254>::verify_proof(&pvk, &bad_proof, &inputs).unwrap());
    rejected.push("proof point".into());
    eprintln!(
        "P_link: valid proof verified; {} public/proof substitutions rejected",
        rejected.len()
    );
    // Malicious witnesses, including a new commitment consistent with the false nominal.
    let mut bad = c.clone();
    bad.private.nominal += BigUint::from(1u64);
    bad.public.nominal_commitment =
        p384_gadget::native_commit(&bad.private.nominal, &bad.private.nominal_blinding).unwrap();
    synthesize(bad, false);
    rejected.push("wrong arithmetic with matching new commitment (R1CS)".into());
    let mut bad = c.clone();
    bad.private.base += 1;
    bad.private.nominal =
        circuit::nominal(&bad.public, bad.private.base, bad.private.atto).unwrap();
    bad.public.nominal_commitment =
        p384_gadget::native_commit(&bad.private.nominal, &bad.private.nominal_blinding).unwrap();
    synthesize(bad, false);
    rejected.push("different private draft with matching economics (R1CS)".into());
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("proof.bin"), &proof_bytes).unwrap();
    std::fs::write(out.join("vk.bin"), &vk_bytes).unwrap();
    std::fs::write(out.join("public_inputs.bin"), &input_bytes).unwrap();
    let public_hex = inputs
        .iter()
        .map(|f| format!("0x{:0>64}", fr_integer(*f).to_str_radix(16)))
        .collect::<Vec<_>>();
    std::fs::write(
        out.join("public_inputs.json"),
        serde_json::to_vec_pretty(&public_hex).unwrap(),
    )
    .unwrap();
    let result = json!({
        "kind":"P_link Groth16 BN254 with canonical draft hash, exact integer nominal and P384 commitment",
        "constraints":constraints,"witness_variables":witness_vars,"public_inputs":public_vars,
        "max_source_markers":circuit::MAX_SOURCES,"fixture_source_markers":c.public.source_count,
        "synthesis_and_satisfaction_ms":synthesis_ms,"setup_ms":setup_ms,"prepare_vk_ms":prepare_vk_ms,
        "prove_ms_one_sample":prove_ms,"verify_ms_median_of_5_after_warmup":verify_ms,
        "proof_compressed_bytes":proof_bytes.len(),"vk_compressed_bytes":vk_bytes.len(),
        "pk_compressed_bytes":pk.compressed_size(),"public_field_payload_bytes":inputs.len()*32,
        "public_inputs_serialized_bytes":input_bytes.len(),"nominal_commitment_sec1_bytes":49,
        "negative_checks_passed":rejected,
        "randomness":"OsRng for Groth16 setup and proof; fixed PUBLIC witness fixture in source",
        "setup_security":"single-process research setup; no distributed ceremony; keys MUST NOT be deployed",
        "source_hash_crosschecked":"pinned outbe-protocol v0.14.0 canonical Entity derive in cargo integration test",
        "not_implemented":["L2 FullProof verification inside this executable","network admission/receipts/consensus","production setup/VK registration","private PayNote/Fidelity and exact claim","wallet witness delivery from L2"],
        "scope":"P_link only; no production code changed"
    });
    std::fs::write(
        out.join("results.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
