use ark_bn254::{Bn254, Fr};
use ark_ff::PrimeField;
use ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_serialize::CanonicalSerialize;
use num_bigint::BigUint;
use outbe_private_lifecycle_poc::{
    crypto::*,
    state::{self, Note, Public, Transition},
    vss::{digest, point_hex},
    wire::Wallet,
};
use rand::rngs::OsRng;
use serde_json::{json, Value};
use std::{path::Path, time::Instant};
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn p<'a>(j: &'a Value, k: &str) -> Result<&'a Path> {
    Ok(Path::new(j[k].as_str().ok_or("path missing")?))
}
fn check(t: Transition, want: bool) -> Result<usize> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    t.generate_constraints(cs.clone())?;
    if cs.is_satisfied()? != want {
        return Err("state constraint check failed".into());
    }
    Ok(cs.num_constraints())
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 1 {
        return Err("usage: poc-state JOB.json".into());
    }
    let j: Value = read(Path::new(&args[0]))?;
    match j["op"].as_str().ok_or("op missing")? {
        "setup" => {
            let kind = j["kind"].as_str().ok_or("kind")?;
            let dir = p(&j, "dir")?;
            std::fs::create_dir_all(dir)?;
            if dir.join("pk.bin").exists() {
                return Err("state parameters already exist".into());
            }
            let t = state::fixture(kind)?;
            let constraints = check(t.clone(), true)?;
            let mut bad = t.clone();
            let changed = if kind == "claim" { 2 } else { 1 };
            bad.notes[changed] = Note::fresh(integer(&bad.notes[changed].value)? + 1u32)?;
            bad.public.notes[changed] = bad.notes[changed].commitments()?;
            check(bad, false)?;
            let s = Instant::now();
            let pk = Groth16::<Bn254>::generate_random_parameters_with_reduction(t, &mut OsRng)?;
            write_canonical(&dir.join("pk.bin"), &pk)?;
            write_canonical(&dir.join("vk.bin"), &pk.vk)?;
            write_json(
                &dir.join("setup.json"),
                &json!({"kind":kind,"constraints":constraints,"pk_bytes":pk.compressed_size(),"vk_bytes":pk.vk.compressed_size(),"setup_ms":s.elapsed().as_secs_f64()*1000.,"wrong_conservation_rejected":true,"ceremony":"single-party experimental"}),
            )?;
        }
        "note" => {
            let value = if let Some(bits) = j["random_bits"].as_u64() {
                if bits == 0 || bits > 256 {
                    return Err("random note bit bound".into());
                }
                use rand::RngCore;
                let mut b = [0u8; 32];
                OsRng.fill_bytes(&mut b);
                BigUint::from_bytes_le(&b) & ((BigUint::from(1u32) << bits as usize) - 1u32)
            } else {
                integer(j["value"].as_str().ok_or("note value")?)?
            };
            let n = if j["canonical_zero"] == true {
                Note::zero()
            } else {
                Note::fresh(value)?
            };
            write_private_json(p(&j, "out")?, &n)?;
            write_json(p(&j, "public")?, &n.commitments()?)?;
        }
        "prepare" => {
            let kind = j["kind"].as_str().ok_or("kind")?;
            let dir = p(&j, "out")?;
            std::fs::create_dir_all(dir)?;
            let context = field_hex(Fr::from_be_bytes_mod_order(&hex::decode(digest(
                &j["context"],
            )?)?));
            let old: Note = read(p(&j, "old")?)?;
            let b = integer(&old.value)?;
            let mut nominal = BigUint::from(1u32);
            let mut blind = "0".to_string();
            let mut fraction = "0".to_string();
            let mut price = "0".to_string();
            let mut amount = "0".to_string();
            let notes = match kind {
                "claim" => {
                    let w: Wallet = read(p(&j, "wallet")?)?;
                    nominal = integer(&w.nominal)?;
                    blind = w.blinder;
                    fraction = j["fraction"].as_str().ok_or("fraction")?.to_string();
                    price = j["price"].as_str().ok_or("price")?.into();
                    let pay: Note = read(p(&j, "payment")?)?;
                    let g = &nominal * integer(&fraction)? * 1_000_000u64;
                    let c = &nominal * integer(&fraction)? * integer(&price)?;
                    if integer(&pay.value)? < c {
                        return Err("insufficient private payment balance".into());
                    }
                    vec![
                        old,
                        pay.clone(),
                        Note::fresh(b + g)?,
                        Note::fresh(integer(&pay.value)? - &c)?,
                        Note::fresh(c)?,
                    ]
                }
                "move" => {
                    if let Some(incoming) = j["incoming"].as_str() {
                        let n: Note = read(Path::new(incoming))?;
                        let v = integer(&n.value)?;
                        vec![old, n, Note::fresh(b + v)?, Note::zero()]
                    } else {
                        let div = j["divisor"].as_u64().ok_or("private split divisor")?;
                        if div < 2 {
                            return Err("split divisor >=2".into());
                        }
                        let a = &b / div;
                        vec![old, Note::zero(), Note::fresh(&b - &a)?, Note::fresh(a)?]
                    }
                }
                "withdraw" => {
                    amount = j["amount"].as_str().ok_or("withdraw amount")?.into();
                    let a = integer(&amount)?;
                    if b < a {
                        return Err("insufficient private Gratis".into());
                    }
                    vec![old, Note::fresh(b - a)?]
                }
                "pledge" => {
                    amount = j["amount"].as_str().ok_or("collateral amount")?.into();
                    let a = integer(&amount)?;
                    if b < a {
                        return Err("insufficient pledge balance".into());
                    }
                    vec![old, Note::fresh(b - &a)?, Note::fresh(a)?]
                }
                "mint" => {
                    let burn: Note = read(p(&j, "burn_note")?)?;
                    let g = integer(&burn.value)? * 1_000_000_000_000u64;
                    vec![old, burn, Note::fresh(b + g)?]
                }
                _ => return Err("unknown prepare operation".into()),
            };
            let source = point_hex(commit(&nominal, scalar(&integer(&blind)?)?)?)?;
            let public = Public {
                kind: kind.into(),
                context,
                notes: notes.iter().map(Note::commitments).collect::<Result<_>>()?,
                source,
                fraction,
                price,
                amount,
            };
            let t = Transition {
                public: public.clone(),
                notes: notes.clone(),
                nominal: nominal.to_string(),
                blinder: blind,
            };
            write_private_json(&dir.join("transition.private.json"), &t)?;
            for (i, n) in notes.iter().enumerate() {
                write_private_json(&dir.join(format!("note-{i}.private.json")), n)?;
            }
            write_json(&dir.join("statement.public.json"), &public)?;
        }
        "prove" => {
            let t: Transition = read(p(&j, "witness")?)?;
            let public = t.public.clone();
            let dir = p(&j, "out")?;
            std::fs::create_dir_all(dir)?;
            let s = Instant::now();
            let pk: ProvingKey<Bn254> = read_canonical(&p(&j, "parameters")?.join("pk.bin"))?;
            let load_ms = s.elapsed().as_secs_f64() * 1000.;
            let inputs = public.inputs()?;
            if pk.vk.gamma_abc_g1.len() != inputs.len() + 1 {
                return Err("state VK shape mismatch".into());
            }
            let s = Instant::now();
            let proof = Groth16::<Bn254>::create_random_proof_with_reduction(t, &pk, &mut OsRng)?;
            let prove_ms = s.elapsed().as_secs_f64() * 1000.;
            if !Groth16::<Bn254>::verify_proof(&prepare_verifying_key(&pk.vk), &proof, &inputs)? {
                return Err("invalid generated state proof".into());
            }
            write_canonical(&dir.join("proof.bin"), &proof)?;
            write_json(&dir.join("statement.public.json"), &public)?;
            write_json(
                &dir.join("resources.json"),
                &json!({"proof_bytes":proof.compressed_size(),"public_inputs":inputs.len(),"load_pk_ms":load_ms,"prove_ms":prove_ms,"verified":true}),
            )?;
        }
        "verify" => {
            let public: Public = read(p(&j, "statement")?)?;
            let vk: VerifyingKey<Bn254> = read_canonical(&p(&j, "parameters")?.join("vk.bin"))?;
            let proof: Proof<Bn254> = read_canonical(p(&j, "proof")?)?;
            let inputs = public.inputs()?;
            if vk.gamma_abc_g1.len() != inputs.len() + 1 {
                return Err("state VK shape".into());
            }
            let pvk = prepare_verifying_key(&vk);
            let s = Instant::now();
            if !Groth16::<Bn254>::verify_proof(&pvk, &proof, &inputs)? {
                return Err("state proof rejected".into());
            }
            let verify_ms = s.elapsed().as_secs_f64() * 1000.;
            let mut mutations = 0;
            if j["mutate"] == true {
                for i in 0..inputs.len() {
                    let mut bad = inputs.clone();
                    bad[i] += Fr::from(1u32);
                    if Groth16::<Bn254>::verify_proof(&pvk, &proof, &bad)? {
                        return Err("state proof public mutation accepted".into());
                    }
                    mutations += 1;
                }
            }
            write_json(
                p(&j, "out")?,
                &json!({"verified":true,"verify_ms":verify_ms,"public_mutations_rejected":mutations}),
            )?;
        }
        _ => return Err("unknown state operation".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("State failed: {e}");
        std::process::exit(1)
    }
}
