use ark_bn254::Bn254;
use ark_groth16::{prepare_verifying_key, Groth16, Proof, VerifyingKey};
use curve25519_dalek::scalar::Scalar;
use num_bigint::BigUint;
use outbe_private_lifecycle_poc::{
    crypto as bc,
    state::{Public, Transition},
};
use outbe_twisted_poc::*;
use outbe_twisted_poc::{bridge, money};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, time::Instant};

type Registry = BTreeMap<String, Cipher>;
#[derive(Clone, Serialize, Deserialize)]
struct Bundle {
    statement: Public,
    ciphers: Vec<Cipher>,
    bridges: Vec<Option<bridge::Bridge>>,
    money: money::MoneyProof,
    economics: Vec<u8>,
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
fn id(key: Bytes, baby: &[String]) -> String {
    hex::encode(hash(&(b"OUTBE-V2-CIPHER-REGISTRY-1", key, baby)))
}
fn context(s: &Public, cts: &[Cipher]) -> Bytes {
    hash(&(b"OUTBE-V2-MONEY-STATEMENT-1", s, cts))
}
fn relation(s: &Public) -> Result<(Vec<i32>, BigUint, bool)> {
    s.inputs()?;
    Ok(match s.kind.as_str() {
        "move" => (vec![1, 1, -1, -1], BigUint::from(0u32), false),
        "withdraw" => {
            let amount = bc::integer(&s.amount)?;
            if amount == BigUint::from(0u32) {
                return Err("withdraw amount must be nonzero".into());
            }
            (vec![1, -1], amount, false)
        }
        "claim" | "mint" | "pledge" => (vec![0; s.notes.len()], BigUint::from(0u32), true),
        _ => return Err("unsupported relation".into()),
    })
}
fn verify(p: &Bundle, registry: &Registry, expected_key: Bytes, parameters: &Path) -> Result<()> {
    let (coeff, rhs, econ) = relation(&p.statement)?;
    if p.ciphers.len() != p.statement.notes.len() || p.bridges.len() != p.ciphers.len() {
        return Err("bundle shape".into());
    }
    let ctx = context(&p.statement, &p.ciphers);
    money::verify(&p.ciphers, &coeff, &rhs, &ctx, &p.money)?;
    for (i, (ct, baby)) in p.ciphers.iter().zip(&p.statement.notes).enumerate() {
        if ct.key != expected_key {
            return Err("unregistered encryption key".into());
        }
        if let Some(proof) = &p.bridges[i] {
            bridge::verify(
                baby,
                &p.money.f[i * 16..i * 16 + 16],
                &hash(&(ctx, i as u64, &p.money)),
                proof,
            )?;
        } else if registry.get(&id(expected_key, baby)) != Some(ct) {
            return Err("missing established Baby/cipher binding".into());
        }
    }
    if econ {
        let vk: VerifyingKey<Bn254> =
            bc::read_canonical(&parameters.join(format!("{}-v2/vk.bin", p.statement.kind)))?;
        let proof: Proof<Bn254> = bc::decode(&p.economics)?;
        if !Groth16::<Bn254>::verify_proof(
            &prepare_verifying_key(&vk),
            &proof,
            &p.statement.inputs()?,
        )? {
            return Err("economics proof rejected".into());
        }
    } else if !p.economics.is_empty() {
        return Err("unexpected economics proof".into());
    }
    Ok(())
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
            for (n, baby) in t.notes.iter().zip(&t.public.notes) {
                if &n.commitments()? != baby {
                    return Err("Baby witness mismatch".into());
                }
                if let Some(ct) = registry.get(&id(pk, baby)) {
                    cts.push(ct.clone());
                    needs.push(false)
                } else {
                    cts.push(if let Some(ct) = overrides.get(&id(pk, baby)) {
                        ct.clone()
                    } else {
                        Cipher::encrypt(&bc::integer(&n.value)?, pk)?
                    });
                    needs.push(true)
                }
            }
            let ctx = context(&t.public, &cts);
            let (coeff, rhs, econ) = relation(&t.public)?;
            let begun = Instant::now();
            let (proof, w) = money::prove(&cts, &vec![s; cts.len()], &coeff, &rhs, &ctx)?;
            let money_ms = begun.elapsed().as_secs_f64() * 1000.;
            let begun = Instant::now();
            let mut bridges = vec![];
            for i in 0..cts.len() {
                if w.values[i] != bc::integer(&t.notes[i].value)? {
                    return Err("cipher/Baby witness values differ".into());
                }
                bridges.push(if needs[i] {
                    Some(bridge::prove(
                        &t.notes[i],
                        &proof.f[i * 16..i * 16 + 16],
                        &w.blinds[i],
                        &hash(&(ctx, i as u64, &proof)),
                    )?)
                } else {
                    None
                });
            }
            let bridge_ms = begun.elapsed().as_secs_f64() * 1000.;
            let economics = if econ {
                std::fs::read(path(&j, "economics")?)?
            } else {
                vec![]
            };
            let b = Bundle {
                statement: t.public.clone(),
                ciphers: cts,
                bridges,
                money: proof,
                economics,
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
                &json!({"money_prove_ms":money_ms,"cross_curve_bridge_ms":bridge_ms,"self_verify_ms":self_verify_ms,"total_ms":start.elapsed().as_secs_f64()*1000.,"bundle_bytes":bytes.len(),"money_proof_bytes":bincode::serialized_size(&b.money)?,"bridge_count":needs.iter().filter(|x|**x).count(),"bridge_bytes":b.bridges.iter().flatten().map(|x|bincode::serialized_size(x).unwrap()).sum::<u64>(),"cipher_raw_bytes_each":1024,"cipher_with_key_and_lengths_bytes_each":bincode::serialized_size(&b.ciphers[0])?,"retained_groth16_bytes":b.economics.len(),"recovery_profile":"cold 65536-entry exact lookup; old encryption randomness never read","verified":true}),
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
            verify(&p, &registry, pk, path(&j, "parameters")?)?;
            let verify_ms = start.elapsed().as_secs_f64() * 1000.;
            let mut next = registry.clone();
            for (baby, ct) in p.statement.notes.iter().zip(&p.ciphers) {
                next.insert(id(pk, baby), ct.clone());
            }
            bc::write_json(path(&j, "next_registry")?, &next)?;
            bc::write_json(
                path(&j, "out")?,
                &json!({"verified":true,"verify_ms":verify_ms,"cipher_bindings":next.len(),"bundle_hash":hex::encode(hash(&p)),"statement":p.statement,"ciphers":p.ciphers}),
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
            let n: outbe_private_lifecycle_poc::state::Note = read(path(&j, "amount_note")?)?;
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
            let n = outbe_private_lifecycle_poc::state::Note::fresh(d.receiver.decrypt(s)?)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_withdraw_rejected_at_verifier_relation() -> Result<()> {
        let mut s = outbe_private_lifecycle_poc::state::fixture("withdraw")?.public;
        s.amount = "0".into();
        assert!(relation(&s).is_err());
        s.amount = "1".into();
        assert!(relation(&s).is_ok());
        Ok(())
    }
    #[test]
    fn altered_cipher_bundle_cannot_reuse_statement_receipt() -> Result<()> {
        let t = outbe_private_lifecycle_poc::state::fixture("move")?;
        let sk = keygen();
        let pk = public_key(sk)?;
        let ciphers = t
            .notes
            .iter()
            .map(|n| Cipher::encrypt(&bc::integer(&n.value)?, pk))
            .collect::<Result<Vec<_>>>()?;
        let (money, _) = money::prove(
            &ciphers,
            &[sk; 4],
            &[1, 1, -1, -1],
            &BigUint::from(0u32),
            &context(&t.public, &ciphers),
        )?;
        let p = Bundle {
            statement: t.public,
            ciphers,
            bridges: vec![None; 4],
            money,
            economics: vec![],
        };
        let receipt = hex::encode(hash(&p));
        assert!(receipted_bundle(&bincode::serialize(&p)?, &receipt).is_ok());
        let mut bad = p.clone();
        bad.ciphers[3] = Cipher::encrypt(&BigUint::from(99u32), pk)?;
        assert_eq!(
            bincode::serialize(&p.statement)?,
            bincode::serialize(&bad.statement)?
        );
        assert!(receipted_bundle(&bincode::serialize(&bad)?, &receipt).is_err());
        Ok(())
    }
    #[test]
    fn zero_cross_credit_cannot_pass_positive_proof() -> Result<()> {
        let sk = keygen();
        let pk = public_key(sk)?;
        let n = BigUint::from(0u32);
        let c = Cipher::encrypt(&n, pk)?;
        let wrapped = Cipher::encrypt(&max_u256(), pk)?;
        assert!(money::prove(
            &[c, wrapped],
            &[sk; 2],
            &[1, -1],
            &BigUint::from(1u32),
            &hash(&"zero cross credit")
        )
        .is_err());
        // An adversary attaches a valid positive-amount proof for a DIFFERENT
        // ciphertext to an otherwise valid zero-amount dual-handle statement.
        let ctx = hash(&"positive amount context");
        let recipient = public_key(keygen())?;
        let (sender, receiver, proof) =
            money::dual_encrypt(&BigUint::from(1u32), pk, recipient, &ctx)?;
        let minus_one = Cipher::encrypt(&n, pk)?;
        let (positive, _) = money::prove(
            &[sender.clone(), minus_one.clone()],
            &[sk; 2],
            &[1, -1],
            &BigUint::from(1u32),
            &hash(&(b"positive-cross-amount", ctx)),
        )?;
        let mut d = Dual {
            context: ctx,
            sender,
            receiver,
            proof,
            minus_one,
            positive,
        };
        verify_dual_amount(&d)?;
        let (sender, receiver, proof) = money::dual_encrypt(&n, pk, recipient, &ctx)?;
        d.sender = sender;
        d.receiver = receiver;
        d.proof = proof;
        assert!(verify_dual_amount(&d).is_err());
        Ok(())
    }
}
