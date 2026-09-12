//! Wallet-only adapter. Preserve every numeric witness and public coefficient,
//! replace Ristretto randomness/points with fresh baseline Baby-Jubjub openings.
//! Never output private values, keys or randomness to the benchmark controller.
use outbe_private_lifecycle_poc::{crypto::*, state::{Note, Transition}, wire::Wallet};
use serde_json::{json, Value};
use std::path::Path;

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 { return Err("usage: comparison-helper transition|source INPUT OUTPUT".into()); }
    let mut v: Value = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    let out = Path::new(&args[2]);
    match args[0].as_str() {
        "transition" => {
            // The wire shapes coincide, but curve-specific strings are never
            // decoded as baseline points before replacement.
            let mut t: Transition = serde_json::from_value(v)?;
            t.notes = t.notes.iter().map(|n| Note::fresh(integer(&n.value)?)).collect::<Result<_>>()?;
            t.public.notes = t.notes.iter().map(Note::commitments).collect::<Result<_>>()?;
            let blind = Note::fresh(0u8.into())?.blinds[0].clone();
            t.blinder = blind.clone();
            t.public.source = hex_point(&t.nominal, &blind)?;
            write_private_json(out, &t)?;
        }
        "source" => {
            v.as_object_mut().ok_or("wallet")?.remove("salt");
            v["offer"].as_object_mut().ok_or("offer")?.remove("opening_binding");
            let mut w: Wallet = serde_json::from_value(v)?;
            w.blinder = Note::fresh(0u8.into())?.blinds[0].clone();
            w.offer.commitment = hex_point(&w.nominal, &w.blinder)?;
            w.circuit()?;
            write_private_json(out, &w)?;
        }
        _ => return Err("comparison operation".into()),
    }
    println!("{}", json!({"numeric_witness_preserved":true,"curve_randomness_regenerated":true}));
    Ok(())
}
fn hex_point(n: &str, blind: &str) -> Result<String> {
    outbe_private_lifecycle_poc::vss::point_hex(commit(&integer(n)?, scalar(&integer(blind)?)?)?)
}
fn main() { if let Err(e)=run() { eprintln!("comparison adapter: {e}"); std::process::exit(1); } }
