use ark_bn254::{Bn254, Fr};
use ark_groth16::{prepare_verifying_key, Groth16, Proof, ProvingKey, VerifyingKey};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use ark_serialize::CanonicalSerialize;
use outbe_private_lifecycle_poc::{
    crypto::*,
    link,
    wire::{Offer, Wallet},
};
use rand::rngs::OsRng;
use serde_json::json;
use std::{path::Path, time::Instant};

fn argument(args: &[String], n: usize) -> Result<&str> {
    args.get(n)
        .map(String::as_str)
        .ok_or_else(|| "missing command argument".into())
}
fn constraint_check(c: link::LinkCircuit, expected: bool) -> Result<(usize, usize)> {
    let cs = ConstraintSystem::<Fr>::new_ref();
    c.generate_constraints(cs.clone())?;
    if cs.is_satisfied()? != expected {
        return Err("unexpected circuit satisfaction result".into());
    }
    Ok((cs.num_constraints(), cs.num_instance_variables() - 1))
}
fn read_wallet(path: &Path) -> Result<Wallet> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn report(path: &Path, value: serde_json::Value) -> Result<()> {
    write_json(path, &value)?;
    println!("{value}");
    Ok(())
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match argument(&args,0)? {
        "check" => {
            let capacity = argument(&args,1)?.parse()?;
            let c = link::fixture(capacity,0);
            let (constraints, public_inputs) = constraint_check(c.clone(), true)?;
            let mut bad = c.clone();
            bad.private.nominal += 1u32;
            bad.public.commitment = commit(&bad.private.nominal,bad.private.blinder)?;
            constraint_check(bad,false)?;
            let mut bad = c.clone();
            bad.private.blinder += ark_ed_on_bn254::Fr::from(1u32);
            constraint_check(bad,false)?;
            let mut bad = c.clone();
            bad.private.base += 1;
            bad.private.nominal = link::nominal(&bad.public,bad.private.base,bad.private.atto)?;
            bad.public.commitment = commit(&bad.private.nominal,bad.private.blinder)?;
            constraint_check(bad,false)?;
            let mut edge = c;
            edge.private.base = u64::MAX;
            edge.private.atto = 999_999;
            edge.private.nominal = link::nominal(&edge.public,edge.private.base,edge.private.atto)?;
            edge.public.nft_hash = link::draft_hash(&edge.public,&edge.private);
            edge.public.commitment = commit(&edge.private.nominal,edge.private.blinder)?;
            constraint_check(edge,true)?;
            println!("{}",json!({"source_capacity":capacity,"constraints":constraints,
                "public_inputs":public_inputs,"correct_source_and_u64_boundary":true,
                "wrong_nominal_same_commit_rejected":true,"wrong_blinder_rejected":true,
                "wrong_source_same_economics_rejected":true}));
        }
        "setup" => {
            let dir = Path::new(argument(&args,1)?);
            let capacity = argument(&args,2)?.parse()?;
            if dir.join("pk.bin").exists() { return Err("parameters already exist".into()); }
            std::fs::create_dir_all(dir)?;
            let c = link::fixture(capacity,0);
            let (constraints, public_inputs) = constraint_check(c.clone(),true)?;
            let started = Instant::now();
            let pk = Groth16::<Bn254>::generate_random_parameters_with_reduction(c,&mut OsRng)?;
            let setup_ms = started.elapsed().as_secs_f64()*1000.;
            write_canonical(&dir.join("pk.bin"),&pk)?;
            write_canonical(&dir.join("vk.bin"),&pk.vk)?;
            report(&dir.join("setup.json"),json!({"source_capacity":capacity,
                "constraints":constraints,"public_inputs":public_inputs,"setup_ms":setup_ms,
                "pk_bytes":pk.compressed_size(),"vk_bytes":pk.vk.compressed_size(),
                "setup_role":"separate server process; single-party PoC ceremony, not production"}))?;
        }
        "wallet" => {
            let dir = Path::new(argument(&args,1)?);
            let capacity = argument(&args,2)?.parse()?;
            let index = argument(&args,3)?.parse()?;
            std::fs::create_dir_all(dir)?;
            let c = link::fixture(capacity,index);
            let wallet = Wallet::from_circuit(&c)?;
            write_private_json(&dir.join("wallet.private.json"),&wallet)?;
            write_json(&dir.join("offer.public.json"),&wallet.offer)?;
            println!("{}",json!({"wallet_created":true,"source_capacity":capacity}));
        }
        "prove" => {
            let parameters = Path::new(argument(&args,1)?);
            let wallet = read_wallet(Path::new(argument(&args,2)?))?;
            let out = Path::new(argument(&args,3)?);
            std::fs::create_dir_all(out)?;
            let c = wallet.circuit()?;
            let inputs = c.public.inputs()?;
            let load = Instant::now();
            let pk: ProvingKey<Bn254> = read_canonical(&parameters.join("pk.bin"))?;
            let load_ms = load.elapsed().as_secs_f64()*1000.;
            if inputs.len()+1 != pk.vk.gamma_abc_g1.len() {
                return Err("source capacity/public input layout does not match registered VK".into());
            }
            let start = Instant::now();
            let proof = Groth16::<Bn254>::create_random_proof_with_reduction(c,&pk,&mut OsRng)?;
            let prove_ms = start.elapsed().as_secs_f64()*1000.;
            if !Groth16::<Bn254>::verify_proof(&prepare_verifying_key(&pk.vk),&proof,&inputs)? {
                return Err("generated P_link failed verification".into());
            }
            write_canonical(&out.join("p_link.bin"),&proof)?;
            write_json(&out.join("offer.public.json"),&wallet.offer)?;
            report(&out.join("prove.json"),json!({"load_pk_ms":load_ms,"prove_ms":prove_ms,
                "proof_bytes":proof.compressed_size(),"commitment_bytes":32,
                "public_input_bytes":inputs.len()*32,"p_link_verified":true,
                "scope":"complete canonical source/economics/commitment P_link; P_L2 admission is a separate required check"}))?;
        }
        "verify" => {
            let parameters = Path::new(argument(&args,1)?);
            let out = Path::new(argument(&args,2)?);
            let count: usize = args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(5);
            if count == 0 { return Err("verification count must be positive".into()); }
            let vk: VerifyingKey<Bn254> = read_canonical(&parameters.join("vk.bin"))?;
            let proof: Proof<Bn254> = read_canonical(&out.join("p_link.bin"))?;
            let offer: Offer = serde_json::from_slice(&std::fs::read(out.join("offer.public.json"))?)?;
            let inputs = offer.to_public()?.inputs()?;
            if inputs.len()+1 != vk.gamma_abc_g1.len() { return Err("wrong public-input count".into()); }
            let pvk = prepare_verifying_key(&vk);
            let start = Instant::now();
            for _ in 0..count {
                if !Groth16::<Bn254>::verify_proof(&pvk,&proof,&inputs)? { return Err("invalid P_link".into()); }
            }
            let verification_ms = start.elapsed().as_secs_f64()*1000.;
            for i in 0..inputs.len() {
                let mut bad = inputs.clone(); bad[i] += Fr::from(1u32);
                if Groth16::<Bn254>::verify_proof(&pvk,&proof,&bad)? {
                    return Err("public input mutation verified".into());
                }
            }
            report(&out.join("verify.json"),json!({"p_link_valid":true,"verification_iterations":count,
                "verification_total_ms":verification_ms,"verification_mean_ms":verification_ms/count as f64,
                "public_field_mutations_rejected":inputs.len(),"benchmark_reuses_one_proof":true}))?;
        }
        "batch-prove" | "batch-verify" => {
            let verify=args[0]=="batch-verify";
            let parameters=Path::new(argument(&args,1)?);
            let manifest:serde_json::Value=serde_json::from_slice(&std::fs::read(argument(&args,2)?)?)?;
            let rows=manifest["rows"].as_array().ok_or("batch rows")?;
            let load=Instant::now();
            let pk:Option<ProvingKey<Bn254>>=if verify{None}else{Some(read_canonical(&parameters.join("pk.bin"))?)};
            let vk:VerifyingKey<Bn254>=read_canonical(&parameters.join("vk.bin"))?;
            let pvk=prepare_verifying_key(&vk);let load_ms=load.elapsed().as_secs_f64()*1000.;
            let start=Instant::now();
            for row in rows {
                let out=Path::new(row["out"].as_str().ok_or("batch out")?);std::fs::create_dir_all(out)?;
                if verify {
                    let o:Offer=serde_json::from_slice(&std::fs::read(out.join("offer.public.json"))?)?;
                    let proof:Proof<Bn254>=read_canonical(&out.join("p_link.bin"))?;
                    if !Groth16::<Bn254>::verify_proof(&pvk,&proof,&o.to_public()?.inputs()?)?{return Err("batch P_link invalid".into());}
                }else{
                    let w=read_wallet(Path::new(row["wallet"].as_str().ok_or("batch wallet")?))?;
                    let c=w.circuit()?;let inputs=c.public.inputs()?;
                    if inputs.len()+1!=vk.gamma_abc_g1.len(){return Err("batch circuit capacity".into());}
                    let proof=Groth16::<Bn254>::create_random_proof_with_reduction(c,pk.as_ref().unwrap(),&mut OsRng)?;
                    if !Groth16::<Bn254>::verify_proof(&pvk,&proof,&inputs)?{return Err("generated batch P_link invalid".into());}
                    write_canonical(&out.join("p_link.bin"),&proof)?;write_json(&out.join("offer.public.json"),&w.offer)?;
                }
            }
            report(Path::new(manifest[if verify{"verify_report"}else{"report"}].as_str().ok_or("batch report")?),json!({"distinct_proofs":rows.len(),"verify_only":verify,"load_parameters_ms":load_ms,"batch_ms":start.elapsed().as_secs_f64()*1000.}))?;
        }
        _ => return Err("commands: check CAP | setup DIR CAP | wallet DIR CAP INDEX | prove PARAM WALLET OUT | verify PARAM OUT [ITERATIONS]".into()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("PoC command failed: {error}");
        std::process::exit(1);
    }
}
