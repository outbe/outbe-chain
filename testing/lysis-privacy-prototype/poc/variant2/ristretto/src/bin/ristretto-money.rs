use curve25519_dalek::scalar::Scalar;
use num_bigint::BigInt;
use num_bigint::BigUint;
use outbe_ristretto_lifecycle_poc::*;
use outbe_ristretto_lifecycle_poc::{
    crypto as bc,
    state::{Public, Transition},
};
use outbe_ristretto_lifecycle_poc::{money, note_link, wide};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Instant};

type Registry = BTreeMap<String, Cipher>;
#[derive(Clone, Serialize, Deserialize)]
struct Bundle {
    statement: Public,
    ciphers: Vec<Cipher>,
    links: Vec<Option<Vec<note_link::Opening>>>,
    money: money::MoneyProof,
    economics: Vec<wide::Proof>,
    source_link: Option<note_link::SourceLink>,
    positive: Option<Positive>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Positive {
    minus_one: Cipher,
    proof: money::MoneyProof,
}
#[derive(Clone, Serialize, Deserialize)]
struct Dual {
    context: Bytes,
    sender: Cipher,
    receiver: Cipher,
    proof: money::HandlesProof,
    minus_one: Cipher,
    positive: money::MoneyProof,
}
fn verify_dual_amount(d: &Dual) -> Result<()> {
    money::verify_handles(&d.sender, &d.receiver, &d.proof, &d.context)?;
    if d.minus_one.key != d.sender.key {
        return Err("positive amount key".into());
    }
    money::verify(
        &[d.sender.clone(), d.minus_one.clone()],
        &[1, -1],
        &BigUint::from(1u32),
        &hash(&(b"positive-cross-amount", d.context)),
        &d.positive,
    )
}
fn receipted_bundle(raw: &[u8], expected: &str) -> Result<Bundle> {
    let p: Bundle = bincode::deserialize(raw)?;
    if bincode::serialize(&p)? != raw || hex::encode(hash(&p)) != expected {
        return Err("bundle differs from exact node verification receipt".into());
    }
    Ok(p)
}
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn path<'a>(j: &'a Value, k: &str) -> Result<&'a Path> {
    Ok(Path::new(j[k].as_str().ok_or("missing path")?))
}
fn key(p: &Path) -> Result<Scalar> {
    let s: String = read(p)?;
    scalar(hex::decode(s)?.try_into().map_err(|_| "key length")?)
}
fn id(key: Bytes, note_points: &[String]) -> String {
    hex::encode(hash(&(b"OUTBE-V2-CIPHER-REGISTRY-1", key, note_points)))
}
fn context(s: &Public, cts: &[Cipher]) -> Bytes {
    hash(&(b"OUTBE-V2-MONEY-STATEMENT-1", s, cts))
}
fn relation(s: &Public) -> Result<(Vec<i32>, BigUint, bool)> {
    s.validate()?;
    Ok(match s.kind.as_str() {
        "move" => (vec![1, 1, -1, -1], 0u8.into(), false),
        "withdraw" => {
            let a = bc::integer(&s.amount)?;
            if a == 0u8.into() {
                return Err("zero withdraw".into());
            }
            (vec![1, -1], a, false)
        }
        "claim" => (vec![0; s.notes.len() + 1], 0u8.into(), true),
        "mint" | "pledge" => (vec![0; s.notes.len()], 0u8.into(), true),
        _ => return Err("kind".into()),
    })
}
fn economics(s: &Public) -> Result<Vec<wide::Relation>> {
    let rel = |cs: Vec<BigInt>, rhs: BigUint| wide::Relation {
        coefficients: cs,
        rhs,
    };
    let b = |n: i32| BigInt::from(n);
    Ok(match s.kind.as_str() {
        "claim" => {
            let f = bc::integer(&s.fraction)?;
            let p = bc::integer(&s.price)?;
            if f == 0u8.into() || p == 0u8.into() {
                return Err("zero claim coefficient".into());
            }
            vec![
                rel(
                    vec![
                        b(1),
                        b(0),
                        b(-1),
                        b(0),
                        b(0),
                        BigInt::from(&f * 1_000_000u64),
                    ],
                    0u8.into(),
                ),
                rel(vec![b(0), b(1), b(0), b(-1), b(-1), b(0)], 0u8.into()),
                rel(
                    vec![b(0), b(0), b(0), b(0), b(-1), BigInt::from(f * p)],
                    0u8.into(),
                ),
            ]
        }
        "mint" => vec![rel(
            vec![b(1), BigInt::from(1_000_000_000_000u64), b(-1)],
            0u8.into(),
        )],
        "pledge" => {
            let a = bc::integer(&s.amount)?;
            if a == 0u8.into() {
                return Err("zero pledge".into());
            }
            vec![
                rel(vec![b(1), b(-1), b(0)], a.clone()),
                rel(vec![b(0), b(0), b(1)], a),
            ]
        }
        _ => vec![],
    })
}
fn positive_index(s: &Public) -> Option<usize> {
    match s.kind.as_str() {
        "claim" => Some(s.notes.len()),
        "mint" => Some(1),
        _ => None,
    }
}
fn verify(
    p: &Bundle,
    registry: &Registry,
    expected_key: Bytes,
    _parameters: &Path,
) -> Result<Value> {
    let (coeff, rhs, _) = relation(&p.statement)?;
    if p.ciphers.len() != coeff.len() || p.links.len() != p.statement.notes.len() {
        return Err("bundle shape".into());
    }
    for ct in &p.ciphers {
        if ct.key != expected_key {
            return Err("unregistered encryption key".into());
        }
    }
    let ctx = context(&p.statement, &p.ciphers);
    let t = Instant::now();
    money::verify(&p.ciphers, &coeff, &rhs, &ctx, &p.money)?;
    let money_ms = t.elapsed().as_secs_f64() * 1000.;
    let t = Instant::now();
    for (i, (ct, note)) in p.ciphers.iter().zip(&p.statement.notes).enumerate() {
        if let Some(proof) = &p.links[i] {
            note_link::verify(
                note,
                &p.money.f[i * 16..i * 16 + 16],
                proof,
                &hash(&(ctx, i as u64, &p.money)),
            )?;
        } else if registry.get(&id(expected_key, note)) != Some(ct) {
            return Err("missing established Ristretto note/cipher binding".into());
        }
    }
    let links_ms = t.elapsed().as_secs_f64() * 1000.;
    let t = Instant::now();
    if p.statement.kind == "claim" {
        let i = p.statement.notes.len();
        note_link::verify_source(
            &p.statement.source,
            &p.money.f[i * 16..i * 16 + 16],
            p.source_link.as_ref().ok_or("missing source104 link")?,
            &hash(&(ctx, b"nominal-source", &p.money)),
        )?;
    } else if p.source_link.is_some() {
        return Err("unexpected source link".into());
    }
    if let Some(i) = positive_index(&p.statement) {
        let pos = p.positive.as_ref().ok_or("missing positive input proof")?;
        if pos.minus_one.key != expected_key {
            return Err("positive key".into());
        }
        money::verify(
            &[p.ciphers[i].clone(), pos.minus_one.clone()],
            &[1, -1],
            &1u8.into(),
            &hash(&(ctx, b"positive", i)),
            &pos.proof,
        )?;
    } else if p.positive.is_some() {
        return Err("unexpected positive proof".into());
    }
    let source_positive_ms = t.elapsed().as_secs_f64() * 1000.;
    let t = Instant::now();
    let rs = economics(&p.statement)?;
    if p.economics.len() != rs.len() {
        return Err("economics proof count".into());
    }
    for (i, (r, pr)) in rs.iter().zip(&p.economics).enumerate() {
        wide::verify(
            &p.money.f,
            p.ciphers.len(),
            r,
            pr,
            &hash(&(ctx, &p.money, i)),
        )?;
    }
    Ok(
        json!({"range_cipher_carry_ms":money_ms,"same_group_note_links_ms":links_ms,"source_and_positive_ms":source_positive_ms,"public_coefficient_relations_ms":t.elapsed().as_secs_f64()*1000.}),
    )
}
fn run() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: outbe-twisted-poc JOB.json".into());
    }
    let j: Value = read(Path::new(&args[1]))?;
    match j["op"].as_str().ok_or("op")? {
        "keygen" => {
            let s = keygen();
            bc::write_private_json(path(&j, "secret")?, &hex::encode(s.to_bytes()))?;
            bc::write_json(path(&j, "public")?, &hex::encode(public_key(s)?))?;
        }
        "prove" => {
            let start = Instant::now();
            let t: Transition = read(path(&j, "witness")?)?;
            let s = key(path(&j, "secret")?)?;
            let pk = public_key(s)?;
            let registry: Registry = read(path(&j, "registry")?)?;
            let out = path(&j, "out")?;
            std::fs::create_dir_all(out)?;
            if t.notes.len() != t.public.notes.len() {
                return Err("witness shape".into());
            }
            let overrides: Registry = if j["overrides"].is_string() {
                read(path(&j, "overrides")?)?
            } else {
                BTreeMap::new()
            };
            let mut cts = vec![];
            let mut needs = vec![];
            for (n, note_points) in t.notes.iter().zip(&t.public.notes) {
                if &n.commitments()? != note_points {
                    return Err("Ristretto witness mismatch".into());
                }
                if let Some(ct) = registry.get(&id(pk, note_points)) {
                    cts.push(ct.clone());
                    needs.push(false)
                } else {
                    cts.push(if let Some(ct) = overrides.get(&id(pk, note_points)) {
                        ct.clone()
                    } else {
                        Cipher::encrypt(&bc::integer(&n.value)?, pk)?
                    });
                    needs.push(true)
                }
            }
            if t.public.kind == "claim" {
                cts.push(Cipher::encrypt(&bc::integer(&t.nominal)?, pk)?);
            }
            let ctx = context(&t.public, &cts);
            let (coeff, rhs, _) = relation(&t.public)?;
            let begun = Instant::now();
            let (proof, w) = money::prove(&cts, &vec![s; cts.len()], &coeff, &rhs, &ctx)?;
            let money_ms = begun.elapsed().as_secs_f64() * 1000.;
            let begun = Instant::now();
            let mut links = vec![];
            for i in 0..t.notes.len() {
                if w.values[i] != bc::integer(&t.notes[i].value)? {
                    return Err("cipher/note value mismatch".into());
                }
                links.push(if needs[i] {
                    Some(note_link::prove(
                        &t.notes[i],
                        &proof.f[i * 16..i * 16 + 16],
                        &w.blinds[i],
                        &hash(&(ctx, i as u64, &proof)),
                    )?)
                } else {
                    None
                });
            }
            let source_link = if t.public.kind == "claim" {
                let i = t.notes.len();
                Some(note_link::prove_source(
                    &t.public.source,
                    &proof.f[i * 16..i * 16 + 16],
                    &w.blinds[i],
                    &w.values[i],
                    bc::scalar(&bc::integer(&t.blinder)?)?,
                    &hash(&(ctx, b"nominal-source", &proof)),
                )?)
            } else {
                None
            };
            let positive = if let Some(i) = positive_index(&t.public) {
                if w.values[i] == 0u8.into() {
                    return Err("zero private source".into());
                }
                let minus_one = Cipher::encrypt(&(&w.values[i] - 1u8), pk)?;
                let (pp, _) = money::prove(
                    &[cts[i].clone(), minus_one.clone()],
                    &[s, s],
                    &[1, -1],
                    &1u8.into(),
                    &hash(&(ctx, b"positive", i)),
                )?;
                Some(Positive {
                    minus_one,
                    proof: pp,
                })
            } else {
                None
            };
            let mut eproofs = vec![];
            for (i, r) in economics(&t.public)?.iter().enumerate() {
                eproofs.push(wide::prove(
                    &proof.f,
                    &w.values,
                    &w.blinds,
                    r,
                    &hash(&(ctx, &proof, i)),
                )?);
            }
            let linkage_economics_ms = begun.elapsed().as_secs_f64() * 1000.;
            let b = Bundle {
                statement: t.public.clone(),
                ciphers: cts,
                links,
                money: proof,
                economics: eproofs,
                source_link,
                positive,
            };
            let begun = Instant::now();
            verify(&b, &registry, pk, path(&j, "parameters")?)?;
            let self_verify_ms = begun.elapsed().as_secs_f64() * 1000.;
            let bytes = bincode::serialize(&b)?;
            std::fs::write(out.join("twisted.bin"), &bytes)?;
            bc::write_json(&out.join("statement.public.json"), &b.statement)?;
            bc::write_json(&out.join("ciphertexts.json"), &b.ciphers)?;
            bc::write_json(
                &out.join("twisted-resources.json"),
                &json!({"money_prove_ms":money_ms,"linkage_economics_ms":linkage_economics_ms,"self_verify_ms":self_verify_ms,"total_ms":start.elapsed().as_secs_f64()*1000.,"bundle_bytes":bytes.len(),"money_proof_bytes":bincode::serialized_size(&b.money)?,"cross_curve_bridge_count":0,"new_note_links":needs.iter().filter(|x|**x).count(),"same_group_link_bytes":b.links.iter().flatten().map(|x|bincode::serialized_size(x).unwrap()).sum::<u64>(),"cipher_raw_bytes_each":1024,"cipher_with_key_and_lengths_bytes_each":bincode::serialized_size(&b.ciphers[0])?,"retained_groth16_bytes":0,"economics_proof_bytes":bincode::serialized_size(&b.economics)?,"recovery_profile":"cold 65536-entry exact lookup; old encryption randomness never read","verified":true}),
            )?;
        }
        "verify" => {
            let registry: Registry = read(path(&j, "registry")?)?;
            let raw = std::fs::read(path(&j, "proof")?)?;
            let p: Bundle = bincode::deserialize(&raw)?;
            if bincode::serialize(&p)? != raw {
                return Err("noncanonical/trailing proof encoding".into());
            }
            let pkstr: String = read(path(&j, "key")?)?;
            let pk: Bytes = hex::decode(pkstr)?.try_into().map_err(|_| "pk bytes")?;
            let start = Instant::now();
            let components = verify(&p, &registry, pk, path(&j, "parameters")?)?;
            let cold_verify_ms = start.elapsed().as_secs_f64() * 1000.;
            let warm = Instant::now();
            let warm_components = verify(&p, &registry, pk, path(&j, "parameters")?)?;
            let verify_ms = warm.elapsed().as_secs_f64() * 1000.;
            let mut next = registry.clone();
            for (note_points, ct) in p.statement.notes.iter().zip(&p.ciphers) {
                next.insert(id(pk, note_points), ct.clone());
            }
            bc::write_json(path(&j, "next_registry")?, &next)?;
            bc::write_json(
                path(&j, "out")?,
                &json!({"verified":true,"verify_ms":verify_ms,"components":warm_components,"cold_verify_ms":cold_verify_ms,"cold_components":components,"measurement":"verify_ms is warm after shared generator initialization; cold includes initialization","cipher_bindings":next.len(),"bundle_hash":hex::encode(hash(&p)),"statement":p.statement,"ciphers":p.ciphers}),
            )?;
        }
        "recover" => {
            let start = Instant::now();
            let s = key(path(&j, "secret")?)?;
            let cts: Vec<Cipher> = read(path(&j, "ciphers")?)?;
            let values = cts
                .iter()
                .map(|c| Ok(c.decrypt(s)?.to_string()))
                .collect::<Result<Vec<_>>>()?;
            bc::write_private_json(path(&j, "private_out")?, &values)?;
            bc::write_json(
                path(&j, "out")?,
                &json!({"count":values.len(),"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"recovered_from_key_only":true,"read_old_openings":false}),
            )?;
        }
        "dual" => {
            let n: outbe_ristretto_lifecycle_poc::state::Note = read(path(&j, "amount_note")?)?;
            let sk = key(path(&j, "secret")?)?;
            let ka = public_key(sk)?;
            let kb: String = read(path(&j, "recipient_key")?)?;
            let kb: Bytes = hex::decode(kb)?
                .try_into()
                .map_err(|_| "recipient key length")?;
            let ctx = hash(&j["context"]);
            let (sender, receiver, proof) =
                money::dual_encrypt(&bc::integer(&n.value)?, ka, kb, &ctx)?;
            let amount = bc::integer(&n.value)?;
            if amount == BigUint::from(0u32) {
                return Err("cross-owner amount must be positive".into());
            }
            let minus_one = Cipher::encrypt(&(amount - 1u32), ka)?;
            let (positive, _) = money::prove(
                &[sender.clone(), minus_one.clone()],
                &[sk; 2],
                &[1, -1],
                &BigUint::from(1u32),
                &hash(&(b"positive-cross-amount", ctx)),
            )?;
            let mut overrides = Registry::new();
            overrides.insert(id(ka, &n.commitments()?), sender.clone());
            bc::write_json(path(&j, "overrides")?, &overrides)?;
            bc::write_json(
                path(&j, "out")?,
                &Dual {
                    context: ctx,
                    sender,
                    receiver,
                    proof,
                    minus_one,
                    positive,
                },
            )?;
        }
        "receive-input" => {
            let d: Dual = read(path(&j, "dual")?)?;
            let s = key(path(&j, "secret")?)?;
            if d.context != hash(&j["context"]) {
                return Err("transfer context".into());
            }
            verify_dual_amount(&d)?;
            let n = outbe_ristretto_lifecycle_poc::state::Note::fresh(d.receiver.decrypt(s)?)?;
            let mut overrides = Registry::new();
            overrides.insert(id(public_key(s)?, &n.commitments()?), d.receiver);
            bc::write_private_json(path(&j, "private_out")?, &n)?;
            bc::write_json(path(&j, "overrides")?, &overrides)?;
        }
        "verify-dual" => {
            let d: Dual = read(path(&j, "dual")?)?;
            if d.context != hash(&j["context"]) {
                return Err("transfer context".into());
            }
            verify_dual_amount(&d)?;
            // Receipts are supplied from the node's in-memory cache, populated
            // ONLY by full verify. Binding statements alone is insufficient.
            let s = receipted_bundle(
                &std::fs::read(path(&j, "sender_proof")?)?,
                j["sender_receipt"].as_str().ok_or("sender node receipt")?,
            )?;
            let r = receipted_bundle(
                &std::fs::read(path(&j, "receiver_proof")?)?,
                j["receiver_receipt"]
                    .as_str()
                    .ok_or("receiver node receipt")?,
            )?;
            if s.statement.kind != "move"
                || r.statement.kind != "move"
                || s.ciphers.get(3) != Some(&d.sender)
                || r.ciphers.get(1) != Some(&d.receiver)
            {
                return Err("transfer amount not bound to both statements".into());
            }
            bc::write_json(
                path(&j, "out")?,
                &json!({"same_hidden_amount":true,"positive_amount_proved":true,"sender_statement":s.statement,"receiver_statement":r.statement,"handle_proof_bytes":bincode::serialized_size(&d.proof)?,"positivity_proof_bytes":bincode::serialized_size(&d.positive)?}),
            )?;
        }
        _ => return Err("unknown operation".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Twisted PoC: {e}");
        std::process::exit(1)
    }
}
