//! Real pinned FullProof circuit. Fresh local L2 fixtures, never mock ZK proofs.
use alloy_primitives::{Address, B256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use ark_std::rand::rngs::OsRng;
use num_bigint::BigUint;
use outbe_protocol::{Codec, OutbeV1, Suite};
use outbe_protocol::protocol::entity::Entity;
use outbe_protocol::protocol::imt::Imt;
use outbe_protocol::protocol::key::{Signer,NftSigner};
use outbe_protocol::protocol::zk::{Circuit,ProofGenerator,ProofVerifier};
use outbe_protocol_derive::Entity;
use outbe_zk_canonical::full::{full_circuit_domain,FullProvable};
use outbe_zk_canonical::noir::full_proof::FullProof;
use outbe_zk_backend::barretenberg::{Barretenberg,verify_circuit};
use serde_json::{Value,json};
use std::{path::Path,time::Instant};
type Result<T> = std::result::Result<T,Box<dyn std::error::Error>>;

#[derive(Entity)]
struct TributeDraftClaim {
    #[outbe(id_seed)] id: B256,
    #[outbe(body, owner, pos = 0)] derived_owner: B256,
    #[outbe(body, pos = 1)] worldwide_day: u64,
    #[outbe(body, pos = 2)] currency: u16,
    #[outbe(body, pos = 3)] base: u64,
    #[outbe(body, pos = 4)] atto: u64,
    #[outbe(body, pos = 5)] su_ids: Vec<B256>,
}
fn word(f: Fr) -> B256 {
    let b = f.into_bigint().to_bytes_be();
    let mut out = [0;32];out[32-b.len()..].copy_from_slice(&b);B256::from(out)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().ok_or_else(|| format!("missing string field {key}").into())
}
fn number(v: &Value, key: &str) -> Result<u64> {
    v[key].as_u64().ok_or_else(|| format!("missing integer field {key}").into())
}
fn b256(s: &str) -> Result<B256> {
    let b=hex::decode(s)?;
    if b.len()!=32 {return Err("expected canonical BE32".into());}
    let n=BigUint::from_bytes_be(&b);
    if n>=BigUint::from_bytes_be(&Fr::MODULUS.to_bytes_be()) {return Err("noncanonical Fr".into());}
    Ok(B256::from_slice(&b))
}
fn generate(wallet_path: &Path, out: &Path) -> Result<Value> {
    std::fs::create_dir_all(out)?;
    let mut wallet: Value=serde_json::from_slice(&std::fs::read(wallet_path)?)?;
    let p=&wallet["offer"];
    let signer=Signer::<OutbeV1>::local(&mut OsRng)?;
    let seed=signer.owner_seed();let owner=OutbeV1::derive_owner(&seed.pk,seed.nonce)?;
    let id=b256(text(&wallet,"draft_id")?)?;
    let count=number(p,"source_count")? as usize;
    let ids=p["source_ids"].as_array().ok_or("source_ids missing")?;
    let source_capacity=ids.len();
    if count>ids.len() {return Err("source count invalid".into());}
    let draft=TributeDraftClaim {
        id,derived_owner:word(owner),worldwide_day:number(p,"day")?,
        currency:number(p,"currency")?.try_into()?,base:number(&wallet,"base")?,atto:number(&wallet,"atto")?,
        su_ids:ids[..count].iter().map(|x|b256(x.as_str().ok_or("SU not string")?)).collect::<Result<_>>()?,
    };
    let sender=BigUint::parse_bytes(text(p,"sender")?.as_bytes(),10).ok_or("bad sender")?.to_bytes_be();
    if sender.len()>20 {return Err("sender exceeds address".into());}
    let mut address=[0;20];address[20-sender.len()..].copy_from_slice(&sender);
    let binding=OutbeV1::binding(&Address::from(address),&id,number(p,"chain_id")?)?;
    let mut tree=Imt::<OutbeV1>::new(full_circuit_domain(),32)?;
    let path=tree.empty_inclusion_path(0);
    let nft_hash=<TributeDraftClaim as Entity<OutbeV1>>::entity_hash(&draft)?;
    tree.append(nft_hash)?;
    let (w,public)=draft.derive_full_witness(&mut OsRng,&signer,binding,&path)?;
    if tree.root()!=public.expected_merkle_root {return Err("fixture inclusion root mismatch".into());}
    let backend=Barretenberg{disable_zk:false,low_memory:true,max_storage_usage:Some(2_000_000_000)};
    let start=Instant::now();
    let proof=ProofGenerator::<OutbeV1,FullProof>::generate(&backend,&w,&public)?;
    let prove_ms=start.elapsed().as_secs_f64()*1000.;
    let verify_start=Instant::now();
    if !ProofVerifier::<OutbeV1,FullProof>::verify(&backend,&public,&proof)? {return Err("typed FullProof failed".into());}
    let fields=<FullProof as Circuit<OutbeV1>>::public_inputs(&public);
    let mut combined=(fields.len() as u32).to_be_bytes().to_vec();
    for f in fields {combined.extend(OutbeV1::field_to_be_bytes(&f));}
    for part in &proof.proof {combined.extend(part);}
    outbe_zk_canonical::full_proof::decode_public_inputs(&combined)?;
    if !verify_circuit::<FullProof>(&combined)? {return Err("production FullProof verifier failed".into());}
    let verify_ms=verify_start.elapsed().as_secs_f64()*1000.;
    wallet["offer"]["derived_owner"]=json!(hex::encode(word(owner)));
    wallet["offer"]["nft_hash"]=json!(hex::encode(word(nft_hash)));
    wallet["offer"]["binding_hash"]=json!(hex::encode(word(binding)));
    wallet["offer"]["merkle_root"]=json!(hex::encode(word(public.expected_merkle_root)));
    // Existing private wallet stays private; public artifacts contain no source amount/signature witness.
    std::fs::write(wallet_path,serde_json::to_vec(&wallet)?)?;
    std::fs::write(out.join("offer.public.json"),serde_json::to_vec_pretty(&wallet["offer"])?)?;
    std::fs::write(out.join("p_l2.bin"),&combined)?;
    let report=json!({"real_p_l2_verified":true,"proof_bytes":combined.len(),"prove_ms":prove_ms,
        "two_verifications_ms":verify_ms,"circuit":"outbe.full_proof@1.1.0",
        "source_capacity":source_capacity,"source_trust":"fresh one-leaf local L2 fixture; root certificate/registry admission is separate"});
    std::fs::write(out.join("l2.json"),serde_json::to_vec_pretty(&report)?)?;
    Ok(report)
}
fn run()->Result<()> {
    let args=std::env::args().skip(1).collect::<Vec<_>>();
    if args.len()!=2{return Err("usage: outbe-poc-l2-source [batch|verify-batch MANIFEST] or WALLET OUT".into());}
    if args[0]=="batch" || args[0]=="verify-batch" {
        let manifest:Value=serde_json::from_slice(&std::fs::read(&args[1])?)?;
        let rows=manifest["rows"].as_array().ok_or("manifest rows")?;
        let verify=args[0]=="verify-batch";
        if verify {outbe_zk_backend::barretenberg::preinit_srs(8193)?;}
        let start=Instant::now();let mut reports=Vec::new();
        for row in rows {
            let out=Path::new(text(row,"out")?);
            if verify {
                let bytes=std::fs::read(out.join("p_l2.bin"))?;
                outbe_zk_canonical::full_proof::decode_public_inputs(&bytes)?;
                if !verify_circuit::<FullProof>(&bytes)? {return Err("P_L2 verification failed".into());}
            } else {reports.push(generate(Path::new(text(row,"wallet")?),out)?);}
        }
        let report=json!({"distinct_proofs":rows.len(),"verify_only":verify,"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"rows":reports});
        std::fs::write(text(&manifest,if verify{"verify_report"}else{"report"})?,serde_json::to_vec_pretty(&report)?)?;
        println!("{}",json!({"distinct_proofs":rows.len(),"verify_only":verify,"elapsed_ms":report["elapsed_ms"]}));
    } else {println!("{}",generate(Path::new(&args[0]),Path::new(&args[1]))?);}
    Ok(())
}
fn main(){if let Err(e)=run(){eprintln!("L2 fixture failed: {e}");std::process::exit(1)}}
