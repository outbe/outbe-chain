//! Source CLI: one native source proof plus all indexed Ristretto opening proofs.
use ark_bn254::{Bn254, Fr};
use ark_ff::AdditiveGroup;
use ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{
    ConstraintSynthesizer, ConstraintSystem, ConstraintSystemRef, SynthesisError,
};
use ark_serialize::CanonicalSerialize;
use outbe_ristretto_lifecycle_poc::{
    crypto::*,
    link::{self, LinkCircuit, LinkWitness},
    source_opening::{self, Step, STEPS},
    wire::{Offer, Wallet},
};
use rand::rngs::OsRng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
const PROOF_BYTES: usize = 128;
#[derive(Clone)]
enum Circuit {
    Source(LinkCircuit),
    Opening(Step),
}
impl ConstraintSynthesizer<Fr> for Circuit {
    fn generate_constraints(
        self,
        cs: ConstraintSystemRef<Fr>,
    ) -> std::result::Result<(), SynthesisError> {
        match self {
            Self::Source(c) => c.generate_constraints(cs),
            Self::Opening(c) => c.generate_constraints(cs),
        }
    }
}
fn circuit(c: LinkCircuit, part: usize) -> Circuit {
    if part == 0 {
        Circuit::Source(c)
    } else {
        Circuit::Opening(Step::new(c, part - 1))
    }
}
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn check(c: Circuit, want: bool) -> Result<usize> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    c.generate_constraints(cs.clone())?;
    if cs.is_satisfied()? != want {
        return Err("source/step constraint mismatch".into());
    }
    Ok(cs.num_constraints())
}
fn dummy(o: Offer) -> Result<LinkCircuit> {
    Ok(LinkCircuit {
        public: o.to_public()?,
        private: LinkWitness {
            draft_id: Fr::ZERO,
            base: 0,
            atto: 0,
            nominal: 0u8.into(),
            blinder: curve25519_dalek::scalar::Scalar::ZERO,
            salt: Fr::ZERO,
        },
    })
}
fn public_digests(p: &Path) -> Result<Vec<Fr>> {
    let v: Value = read(p)?;
    if v["version"] != 1 || v["steps"] != STEPS {
        return Err("opening schedule/version".into());
    }
    let ds = v["digests"].as_array().ok_or("digests")?;
    if ds.len() != STEPS + 1 {
        return Err("opening digest chain length".into());
    }
    ds.iter()
        .map(|x| field_from_hex(x.as_str().ok_or("digest")?))
        .collect()
}
fn inputs(c: &LinkCircuit, part: usize, ds: &[Fr]) -> Result<Vec<Fr>> {
    if part == 0 {
        Ok(c.public.inputs()?)
    } else {
        Ok(source_opening::step_inputs(c, part - 1, ds))
    }
}
fn parameters(p: &Path, part: usize) -> PathBuf {
    p.join(format!("part-{part:02}"))
}
struct Frozen {
    source: LinkCircuit,
    digests: Vec<Fr>,
    proofs: Vec<Proof<Bn254>>,
    receipt: Value,
}
fn freeze(row: &Value) -> Result<Frozen> {
    let out = Path::new(row["out"].as_str().ok_or("out")?);
    let offer: Offer = read(&out.join("offer.public.json"))?;
    let digests = public_digests(&out.join("opening.public.json"))?;
    let raw = std::fs::read(out.join("p_link.bin"))?;
    if raw.len() != (STEPS + 1) * PROOF_BYTES {
        return Err("incomplete source/opening proof set".into());
    }
    let proofs = raw
        .chunks_exact(PROOF_BYTES)
        .map(decode)
        .collect::<Result<Vec<_>>>()?;
    let receipt = json!({"out":out,"offer_hash":outbe_ristretto_lifecycle_poc::vss::digest(&serde_json::to_value(&offer)?)?,"proof_sha256":hex::encode(Sha256::digest(&raw)),"opening_digests":digests.iter().map(|x|field_hex(*x)).collect::<Vec<_>>()});
    Ok(Frozen {
        source: dummy(offer)?,
        digests,
        proofs,
        receipt,
    })
}
fn batches(params: &Path, rows: &[Value], verify: bool, only_part: Option<usize>) -> Result<Value> {
    if only_part.is_some_and(|p| p > STEPS) || (verify && only_part.is_some()) {
        return Err("partial verification is forbidden; invalid proving step".into());
    }
    // Freeze each public statement/proof set once. Every part and the admission
    // receipt refer to these exact bytes, even if a prover changes its files.
    let frozen = if verify {
        rows.iter().map(freeze).collect::<Result<Vec<_>>>()?
    } else {
        vec![]
    };
    let start = Instant::now();
    let mut parts = vec![];
    for part in 0..=STEPS {
        if only_part.is_some_and(|p| p != part) {
            continue;
        }
        let dir = parameters(params, part);
        let begun = Instant::now();
        let pk: Option<ProvingKey<Bn254>> = if verify {
            None
        } else {
            Some(read_canonical(&dir.join("pk.bin"))?)
        };
        let vk: VerifyingKey<Bn254> = read_canonical(&dir.join("vk.bin"))?;
        let pvk = prepare_verifying_key(&vk);
        let load_ms = begun.elapsed().as_secs_f64() * 1000.;
        let begun = Instant::now();
        for (row_index, row) in rows.iter().enumerate() {
            let out = Path::new(row["out"].as_str().ok_or("out")?);
            std::fs::create_dir_all(out)?;
            if verify {
                let f = &frozen[row_index];
                if !Groth16::<Bn254>::verify_proof(
                    &pvk,
                    &f.proofs[part],
                    &inputs(&f.source, part, &f.digests)?,
                )? {
                    return Err(format!("P_link part {part} rejected").into());
                }
            } else {
                let w: Wallet = read(Path::new(row["wallet"].as_str().ok_or("wallet")?))?;
                let c = w.circuit()?;
                let ds = source_opening::digests(&c);
                let ins = inputs(&c, part, &ds)?;
                if ins.len() + 1 != vk.gamma_abc_g1.len() {
                    return Err("registered source/step input count".into());
                }
                let proof = Groth16::<Bn254>::create_random_proof_with_reduction(
                    circuit(c, part),
                    pk.as_ref().unwrap(),
                    &mut OsRng,
                )?;
                if !Groth16::<Bn254>::verify_proof(&pvk, &proof, &ins)? {
                    return Err(format!("generated P_link part {part} rejected").into());
                }
                let bytes = encode(&proof)?;
                if bytes.len() != PROOF_BYTES {
                    return Err("proof codec size".into());
                }
                if part == 0 {
                    std::fs::write(out.join("p_link.bin"), &bytes)?;
                    write_json(&out.join("offer.public.json"), &w.offer)?;
                    write_json(
                        &out.join("opening.public.json"),
                        &json!({"version":1,"steps":STEPS,"digests":ds.iter().map(|x|field_hex(*x)).collect::<Vec<_>>()}),
                    )?;
                } else {
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(out.join("p_link.bin"))?;
                    f.write_all(&bytes)?;
                }
            }
        }
        parts.push(
            json!({"part":part,"load_ms":load_ms,"work_ms":begun.elapsed().as_secs_f64()*1000.}),
        );
        eprintln!(
            "P_link {} part {part}/{}: {} offers",
            if verify { "verify" } else { "prove" },
            STEPS,
            rows.len()
        );
    }
    Ok(
        json!({"distinct_proofs":rows.len(),"verify_only":verify,"parts":parts,"batch_ms":start.elapsed().as_secs_f64()*1000.,"proof_bytes":(STEPS+1)*PROOF_BYTES,"group":"Ristretto255","cross_curve_bridges":0,"mandatory_step_count":STEPS,"verified_rows":frozen.iter().map(|f|&f.receipt).collect::<Vec<_>>()}),
    )
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let a = |i: usize| args.get(i).map(String::as_str).ok_or("argument");
    match a(0)? {
        "bind-wallet" => {
            let path = Path::new(a(1)?);
            let mut w: Wallet = read(path)?;
            let mut c = w.circuit()?;
            c.public.opening_binding = source_opening::witness_digest(&c);
            w.offer = Offer::from_public(&c.public)?;
            std::fs::write(path, serde_json::to_vec(&w)?)?;
            write_json(Path::new(a(2)?), &w.offer)?;
        }
        "wallet" => {
            let dir = Path::new(a(1)?);
            std::fs::create_dir_all(dir)?;
            let c = link::fixture(a(2)?.parse()?, a(3)?.parse()?);
            let w = Wallet::from_circuit(&c)?;
            write_private_json(&dir.join("wallet.private.json"), &w)?;
            write_json(&dir.join("offer.public.json"), &w.offer)?;
        }
        "setup" => {
            let dir = Path::new(a(1)?);
            let cap: usize = a(2)?.parse()?;
            let c = link::fixture(cap, 0);
            let mut records = vec![];
            for part in 0..=STEPS {
                let p = parameters(dir, part);
                std::fs::create_dir_all(&p)?;
                if p.join("pk.bin").exists() {
                    return Err("existing parameters".into());
                }
                let ck = circuit(c.clone(), part);
                let count = check(ck.clone(), true)?;
                let t = Instant::now();
                let pk =
                    Groth16::<Bn254>::generate_random_parameters_with_reduction(ck, &mut OsRng)?;
                write_canonical(&p.join("pk.bin"), &pk)?;
                write_canonical(&p.join("vk.bin"), &pk.vk)?;
                let r = json!({"part":part,"constraints":count,"pk_bytes":pk.compressed_size(),"vk_bytes":pk.vk.compressed_size(),"setup_ms":t.elapsed().as_secs_f64()*1000.});
                write_json(&p.join("setup.json"), &r)?;
                println!("{r}");
                records.push(r);
            }
            write_json(
                &dir.join("setup.json"),
                &json!({"capacity":cap,"steps":STEPS,"parts":records,"ceremony":"single-party PoC setup"}),
            )?;
        }
        "check" => {
            let cap = a(1)?.parse()?;
            let part: usize = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(0);
            let c = link::fixture(cap, 0);
            let count = check(circuit(c, part), true)?;
            println!(
                "{}",
                json!({"part":part,"constraints":count,"satisfied":true})
            );
        }
        "prove" => {
            let out = Path::new(a(3)?);
            let r = batches(
                Path::new(a(1)?),
                &[json!({"wallet":a(2)?,"out":a(3)?})],
                false,
                None,
            )?;
            write_json(&out.join("prove.json"), &r)?;
            println!("{r}");
        }
        "verify" => {
            let r = batches(Path::new(a(1)?), &[json!({"out":a(2)?})], true, None)?;
            write_json(&Path::new(a(2)?).join("verify.json"), &r)?;
            println!("{r}");
        }
        "batch-prove" | "batch-verify" => {
            let verify = a(0)? == "batch-verify";
            let m: Value = read(Path::new(a(2)?))?;
            let r = batches(
                Path::new(a(1)?),
                m["rows"].as_array().ok_or("rows")?,
                verify,
                None,
            )?;
            write_json(
                Path::new(
                    m[if verify { "verify_report" } else { "report" }]
                        .as_str()
                        .ok_or("report")?,
                ),
                &r,
            )?;
            println!("{r}");
        }
        "batch-prove-part" => {
            let m: Value = read(Path::new(a(2)?))?;
            let r = batches(
                Path::new(a(1)?),
                m["rows"].as_array().ok_or("rows")?,
                false,
                Some(a(3)?.parse()?),
            )?;
            write_json(Path::new(a(4)?), &r)?;
        }
        _ => return Err("source command".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Ristretto source: {e}");
        std::process::exit(1)
    }
}
