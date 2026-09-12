//! Resource feasibility only. Public fixture secrets, single-party research CRS.
use ark_bw6_761::{Fr as Native, BW6_761};
use ark_crypto_primitives::crh::sha256::constraints::Sha256Gadget;
use ark_ec::{AffineRepr, CurveGroup, PrimeGroup};
use ark_ed_on_bw6_761::{constraints::EdwardsVar, EdwardsAffine, EdwardsProjective, Fr as Scalar};
use ark_ff::{AdditiveGroup, BigInteger, Field, PrimeField};
use ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey};
use ark_r1cs_std::{boolean::Boolean, fields::fp::FpVar, prelude::*, uint8::UInt8};
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, OptimizationGoal, SynthesisError,
};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use num_bigint::BigUint;
use rand::rngs::OsRng;
use serde_json::json;
use sha2::{Digest, Sha256, Sha512};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Write},
    path::Path,
    sync::OnceLock,
    time::Instant,
};

const DOMAIN: &[u8] = b"OUTBE-P-LINK-BRIDGE-v1";
const H_DOMAIN: &[u8] = b"OUTBE-RESEARCH-WIDE-EDWARDS-H-v1";

fn h() -> EdwardsProjective {
    static POINT: OnceLock<EdwardsProjective> = OnceLock::new();
    *POINT.get_or_init(|| {
        // Research-only public try-and-increment map, not an RFC 9380 suite.
        // No scalar-to-generator map: nobody learns log_G(H) from this recipe.
        for counter in 0u32.. {
            let mut hasher = Sha512::new();
            hasher.update(H_DOMAIN);
            hasher.update(counter.to_be_bytes());
            let digest = hasher.finalize();
            let y = Native::from_be_bytes_mod_order(&digest);
            if let Some(point) = EdwardsAffine::get_point_from_y_unchecked(y, digest[63] & 1 == 1) {
                assert!(point.is_on_curve());
                let cleared = point.mul_by_cofactor_to_group();
                let affine = cleared.into_affine();
                if !affine.is_zero()
                    && cleared != EdwardsProjective::generator()
                    && cleared != -EdwardsProjective::generator()
                {
                    assert!(affine.is_in_correct_subgroup_assuming_on_curve());
                    return cleared;
                }
            }
        }
        unreachable!()
    })
}

fn bridge(a: &[u8; 32], salt: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(a);
    hasher.update(salt);
    hasher.finalize().into()
}

#[derive(Clone)]
struct Circuit {
    a: [u8; 32],
    salt: [u8; 32],
    r: Scalar,
    commitment: EdwardsAffine,
    digest: [u8; 32],
}

impl Circuit {
    fn fixture() -> Self {
        // Must match the parent's canonical source fixture; decimal can be
        // supplied without changing the circuit shape or proving key.
        let nominal = std::env::var("OUTBE_WIDE_NOMINAL").unwrap_or_else(|_| "4086512338".into());
        let value = BigUint::parse_bytes(nominal.as_bytes(), 10).expect("nominal decimal");
        assert!(value.bits() <= 256);
        let mut a = [0u8; 32];
        let bytes = value.to_bytes_be();
        a[32 - bytes.len()..].copy_from_slice(&bytes);
        let salt: [u8; 32] = Sha256::digest(b"OUTBE-P-LINK-PUBLIC-FIXTURE-SALT-v1").into();
        let r = Scalar::from_be_bytes_mod_order(&Sha512::digest(
            b"OUTBE-WIDE-COMMIT-PUBLIC-FIXTURE-R-v1",
        ));
        let scalar = Scalar::from_be_bytes_mod_order(&a);
        let commitment = (EdwardsProjective::generator() * scalar + h() * r).into_affine();
        Self {
            a,
            salt,
            r,
            commitment,
            digest: bridge(&a, &salt),
        }
    }
    fn inputs(&self) -> Vec<Native> {
        assert!(self.commitment.is_on_curve());
        assert!(self.commitment.is_in_correct_subgroup_assuming_on_curve());
        vec![
            self.commitment.x,
            self.commitment.y,
            Native::from_be_bytes_mod_order(&self.digest[..16]),
            Native::from_be_bytes_mod_order(&self.digest[16..]),
        ]
    }
}

fn less_constant(
    bits: &[Boolean<Native>],
    limit: &BigUint,
) -> Result<Boolean<Native>, SynthesisError> {
    let mut result = Boolean::FALSE;
    for (i, bit) in bits.iter().enumerate() {
        let other = Boolean::constant(limit.bit(i as u64));
        result = (bit ^ &other).select(&((!bit) & other), &result)?;
    }
    Ok(result)
}

fn fixed_mul(
    acc: &mut EdwardsVar,
    bits: &[Boolean<Native>],
    base: EdwardsProjective,
) -> Result<(), SynthesisError> {
    let mut multiples = Vec::with_capacity(bits.len());
    let mut next = base;
    for _ in bits {
        multiples.push(next);
        next.double_in_place();
    }
    acc.precomputed_base_scalar_mul_le(bits.iter().zip(&multiples))
}

impl ConstraintSynthesizer<Native> for Circuit {
    fn generate_constraints(self, cs: ConstraintSystemRef<Native>) -> Result<(), SynthesisError> {
        let cx = FpVar::new_input(cs.clone(), || Ok(self.commitment.x))?;
        let cy = FpVar::new_input(cs.clone(), || Ok(self.commitment.y))?;
        let digest_inputs = self.inputs();
        let d0 = FpVar::new_input(cs.clone(), || Ok(digest_inputs[2]))?;
        let d1 = FpVar::new_input(cs.clone(), || Ok(digest_inputs[3]))?;
        let a = UInt8::new_witness_vec(cs.clone(), &self.a)?;
        let salt = UInt8::new_witness_vec(cs.clone(), &self.salt)?;
        let mut preimage = UInt8::constant_vec(DOMAIN);
        preimage.extend(a.clone());
        preimage.extend(salt);
        let digest = Sha256Gadget::digest(&preimage)?;
        for (chunk, expected) in digest.0.chunks(16).zip([d0, d1]) {
            let mut little_bits = Vec::new();
            for byte in chunk.iter().rev() {
                little_bits.extend(byte.to_bits_le()?);
            }
            // Exactly 128 bits in either field: no reduction of a 256-bit hash.
            Boolean::le_bits_to_fp(&little_bits)?.enforce_equal(&expected)?;
        }
        let mut a_bits = Vec::with_capacity(256);
        for byte in a.iter().rev() {
            a_bits.extend(byte.to_bits_le()?);
        }
        let r_integer = BigUint::from_bytes_le(&self.r.into_bigint().to_bytes_le());
        let r_bits = (0..Scalar::MODULUS_BIT_SIZE as usize)
            .map(|i| Boolean::new_witness(cs.clone(), || Ok(r_integer.bit(i as u64))))
            .collect::<Result<Vec<_>, _>>()?;
        let order = BigUint::from_bytes_le(&Scalar::MODULUS.to_bytes_le());
        less_constant(&r_bits, &order)?.enforce_equal(&Boolean::TRUE)?;
        let mut point = EdwardsVar::zero();
        fixed_mul(&mut point, &a_bits, EdwardsProjective::generator())?;
        fixed_mul(&mut point, &r_bits, h())?;
        point.x.enforce_equal(&cx)?;
        point.y.enforce_equal(&cy)?;
        Ok(())
    }
}

fn save<T: CanonicalSerialize>(path: impl AsRef<Path>, value: &T) {
    let mut writer = BufWriter::new(File::create(path).unwrap());
    value.serialize_compressed(&mut writer).unwrap();
    writer.flush().unwrap();
}
fn load<T: CanonicalDeserialize>(path: impl AsRef<Path>) -> T {
    T::deserialize_compressed(BufReader::new(File::open(path).unwrap())).unwrap()
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let mode = args.get(1).map(String::as_str).unwrap_or("synth");
    let out = Path::new(
        args.get(2)
            .map(String::as_str)
            .unwrap_or("/private/tmp/outbe-wide-commit-proof"),
    );
    std::fs::create_dir_all(out).unwrap();
    let c = Circuit::fixture();
    let inputs = c.inputs();
    let max =
        BigUint::from(1_000_000_000u64) * ((BigUint::from(1u64) << 256usize) - BigUint::from(1u64));
    assert!(BigUint::from_bytes_le(&Scalar::MODULUS.to_bytes_le()) > max);
    match mode {
        "synth" => {
            let start = Instant::now();
            let cs = ConstraintSystem::<Native>::new_ref();
            cs.set_optimization_goal(OptimizationGoal::Constraints);
            c.clone().generate_constraints(cs.clone()).unwrap();
            assert!(cs.is_satisfied().unwrap());
            let result = json!({"constraints":cs.num_constraints(),"public_inputs":cs.num_instance_variables()-1,
                "witness_variables":cs.num_witness_variables(),"synthesis_and_check_ms":ms(start),
                "coordinate_field_bits":Native::MODULUS_BIT_SIZE,"scalar_field_bits":Scalar::MODULUS_BIT_SIZE,
                "commitment_compressed_bytes":c.commitment.compressed_size(),"bridge_digest_hex":hex::encode(c.digest),
                "nominal_decimal":BigUint::from_bytes_be(&c.a).to_string(),"capacity_billion_uint256":true});
            println!("{result}");
            std::fs::write(out.join("synth.json"), result.to_string()).unwrap();
            save(out.join("h.bin"), &h().into_affine());
            save(out.join("commitment.bin"), &c.commitment);
            save(out.join("public_inputs.bin"), &inputs);
        }
        "setup" => {
            let start = Instant::now();
            let pk = Groth16::<BW6_761>::generate_random_parameters_with_reduction(c, &mut OsRng)
                .unwrap();
            let setup_ms = ms(start);
            save(out.join("vk.bin"), &pk.vk);
            save(out.join("pk.bin"), &pk);
            let result = json!({"setup_ms":setup_ms,"pk_bytes":pk.compressed_size(),"vk_bytes":pk.vk.compressed_size(),"single_party_research_setup":true});
            println!("{result}");
            std::fs::write(out.join("setup.json"), result.to_string()).unwrap();
        }
        "prove" => {
            let overall = Instant::now();
            let load_start = Instant::now();
            let pk: ProvingKey<BW6_761> = load(out.join("pk.bin"));
            let pk_load_ms = ms(load_start);
            let start = Instant::now();
            let proof =
                Groth16::<BW6_761>::create_random_proof_with_reduction(c, &pk, &mut OsRng).unwrap();
            let prove_ms = ms(start);
            save(out.join("proof.bin"), &proof);
            save(out.join("public_inputs.bin"), &inputs);
            let result = json!({"pk_load_ms":pk_load_ms,"prove_ms":prove_ms,"wallet_process_work_ms":ms(overall),
                "proof_bytes":proof.compressed_size(),"public_inputs":inputs.len(),"pk_loaded_with_validation":true});
            println!("{result}");
            std::fs::write(out.join("prove.json"), result.to_string()).unwrap();
        }
        "verify" => {
            let vk: VerifyingKey<BW6_761> = load(out.join("vk.bin"));
            let proof: Proof<BW6_761> = load(out.join("proof.bin"));
            let saved: Vec<Native> = load(out.join("public_inputs.bin"));
            assert_eq!(saved, inputs);
            let pvk = prepare_verifying_key(&vk);
            let mut times = Vec::new();
            for i in 0..6 {
                let start = Instant::now();
                assert!(Groth16::<BW6_761>::verify_proof(&pvk, &proof, &inputs).unwrap());
                if i > 0 {
                    times.push(ms(start));
                }
            }
            times.sort_by(f64::total_cmp);
            for i in 0..4 {
                let mut wrong = inputs.clone();
                wrong[i] += Native::ONE;
                assert!(!Groth16::<BW6_761>::verify_proof(&pvk, &proof, &wrong).unwrap());
            }
            let result = json!({"verify_median_ms":times[2],"valid":true,"substituted_public_inputs_rejected":4});
            println!("{result}");
            std::fs::write(out.join("verify.json"), result.to_string()).unwrap();
        }
        "negative" => {
            let cases = [
                ("different amount same digest", 1usize),
                ("different salt same digest", 2),
                ("wrong commitment", 3),
            ];
            for (label, kind) in cases {
                let mut bad = c.clone();
                match kind {
                    1 => bad.a[31] ^= 1,
                    2 => bad.salt[0] ^= 1,
                    _ => {
                        bad.commitment = (bad.commitment.into_group()
                            + EdwardsProjective::generator())
                        .into_affine()
                    }
                };
                let cs = ConstraintSystem::<Native>::new_ref();
                bad.generate_constraints(cs.clone()).unwrap();
                assert!(!cs.is_satisfied().unwrap(), "{label}");
            }
            println!("{}", json!({"malicious_witness_cases_rejected":3}));
        }
        _ => panic!("synth | setup | prove | verify | negative, then output directory"),
    }
}
