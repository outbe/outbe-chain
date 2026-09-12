//! Separate setup/prove/verify processes so wallet RAM includes PK loading,
//! but does not accidentally include parameter generation or other proofs.
use ark_bn254::{Bn254, Fr};
use ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use outbe_p_link_source_measurements::{fixture, LinkCircuit};
use rand::rngs::OsRng;
use serde_json::json;
use std::{path::Path, time::Instant};

fn write<T: CanonicalSerialize>(path: &Path, data: &T) {
    let file = std::fs::File::create(path).unwrap();
    data.serialize_compressed(std::io::BufWriter::new(file))
        .unwrap();
}
fn read<T: CanonicalDeserialize>(path: &Path) -> T {
    let bytes = std::fs::read(path).unwrap();
    let mut remaining = bytes.as_slice();
    let value = T::deserialize_compressed(&mut remaining).unwrap();
    assert!(remaining.is_empty());
    value
}
fn constraints(c: LinkCircuit, valid: bool) -> (usize, usize) {
    let cs = ConstraintSystem::<Fr>::new_ref();
    c.generate_constraints(cs.clone()).unwrap();
    assert_eq!(cs.is_satisfied().unwrap(), valid);
    (cs.num_constraints(), cs.num_instance_variables() - 1)
}
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args.first().expect("setup|prove|verify|check DIR");
    let dir = Path::new(args.get(1).expect("artifact directory"));
    let c = fixture();
    let inputs = c.public.inputs().unwrap();
    if mode == "check" {
        let (count, public) = constraints(c.clone(), true);
        let mut bad = c.clone();
        bad.private.nominal += 1u32;
        bad.public.nominal_digest = outbe_p_link_source_measurements::bridge_digest(
            &bad.private.nominal,
            &bad.private.bridge_salt,
        );
        constraints(bad, false);
        let mut bad = c.clone();
        bad.private.bridge_salt[0] ^= 1;
        constraints(bad, false);
        let mut bad = c;
        bad.private.base += 1;
        bad.private.nominal = outbe_p_link_source_measurements::nominal(
            &bad.public,
            bad.private.base,
            bad.private.atto,
        )
        .unwrap();
        bad.public.nominal_digest = outbe_p_link_source_measurements::bridge_digest(
            &bad.private.nominal,
            &bad.private.bridge_salt,
        );
        constraints(bad, false);
        println!(
            "{}",
            json!({"constraints": count, "public_inputs": public,
            "wrong_nominal_matching_digest_rejected": true,
            "wrong_salt_rejected": true, "wrong_draft_matching_economics_rejected": true})
        );
        return;
    }
    std::fs::create_dir_all(dir).unwrap();
    if mode == "setup" {
        let (count, public) = constraints(c.clone(), true);
        let start = Instant::now();
        let pk =
            Groth16::<Bn254>::generate_random_parameters_with_reduction(c, &mut OsRng).unwrap();
        let setup_ms = start.elapsed().as_secs_f64() * 1000.;
        write(&dir.join("pk.bin"), &pk);
        write(&dir.join("vk.bin"), &pk.vk);
        write(&dir.join("public_inputs.bin"), &inputs);
        let result = json!({"constraints": count, "public_inputs": public, "setup_ms": setup_ms,
            "pk_compressed_bytes": pk.compressed_size(), "vk_compressed_bytes": pk.vk.compressed_size(),
            "setup_security": "OsRng single-party research setup; not production ceremony"});
        std::fs::write(
            dir.join("setup.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    } else if mode == "prove" {
        let load = Instant::now();
        let pk: ProvingKey<Bn254> = read(&dir.join("pk.bin"));
        let load_pk_ms = load.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let proof =
            Groth16::<Bn254>::create_random_proof_with_reduction(c, &pk, &mut OsRng).unwrap();
        let prove_ms = start.elapsed().as_secs_f64() * 1000.;
        assert!(
            Groth16::<Bn254>::verify_proof(&prepare_verifying_key(&pk.vk), &proof, &inputs)
                .unwrap()
        );
        write(&dir.join("proof.bin"), &proof);
        let result = json!({"load_pk_ms": load_pk_ms, "prove_ms": prove_ms,
            "proof_compressed_bytes": proof.compressed_size(), "public_field_payload_bytes": inputs.len()*32,
            "bridge_digest_hex": hex::encode(fixture().public.nominal_digest),
            "scope": "P_source only; requires P_commit with same digest plus external P_L2/admission"});
        std::fs::write(
            dir.join("prove.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    } else if mode == "verify" {
        let vk: VerifyingKey<Bn254> = read(&dir.join("vk.bin"));
        let proof: Proof<Bn254> = read(&dir.join("proof.bin"));
        let saved: Vec<Fr> = read(&dir.join("public_inputs.bin"));
        assert_eq!(saved, inputs, "exact fixture context");
        let pvk = prepare_verifying_key(&vk);
        let mut samples = Vec::new();
        for i in 0..6 {
            let start = Instant::now();
            assert!(Groth16::<Bn254>::verify_proof(&pvk, &proof, &inputs).unwrap());
            if i > 0 {
                samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
        }
        samples.sort_by(f64::total_cmp);
        // Each individual public field is actually bound by the real saved proof.
        for i in 0..inputs.len() {
            let mut bad = inputs.clone();
            bad[i] += Fr::from(1u32);
            assert!(!Groth16::<Bn254>::verify_proof(&pvk, &proof, &bad).unwrap());
        }
        let result = json!({"valid_proof": true, "verify_ms_median_of_5": samples[2],
            "public_field_substitutions_rejected": inputs.len()});
        std::fs::write(
            dir.join("verify.json"),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!("{result}");
    } else {
        panic!("unknown mode");
    }
}
