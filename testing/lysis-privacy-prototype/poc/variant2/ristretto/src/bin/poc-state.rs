use ark_bn254::Fr;
use ark_ff::PrimeField;
use num_bigint::BigUint;
use outbe_ristretto_lifecycle_poc::{
    crypto::*,
    state::{Note, Public, Transition},
    vss::{digest, point_hex},
    wire::Wallet,
};
use rand::rngs::OsRng;
use serde_json::Value;
use std::path::Path;
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn p<'a>(j: &'a Value, k: &str) -> Result<&'a Path> {
    Ok(Path::new(j[k].as_str().ok_or("missing path")?))
}
fn run() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage poc-state JOB".into());
    }
    let j: Value = read(Path::new(&args[1]))?;
    match j["op"].as_str().ok_or("op")? {
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
        _ => return Err("only note/prepare; monetary proofs use Ristretto prover".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("state: {e}");
        std::process::exit(1)
    }
}
