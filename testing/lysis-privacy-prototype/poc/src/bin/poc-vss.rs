//! File transport between separate actors. The controller handles public paths only.
use ark_ec::CurveGroup;
use ark_ed_on_bn254::Fr;
use ark_ff::Zero;
use outbe_private_lifecycle_poc::{crypto::*, vss::*};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Serialize, Deserialize)]
struct Bundle {
    dealer: Identity,
    polynomials: Vec<Polynomial>,
    packets: Vec<Envelope>,
    signature: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    polynomial: Polynomial,
    share: Share,
}
#[derive(Clone, Serialize, Deserialize)]
struct Store {
    epoch: u64,
    member: Identity,
    records: BTreeMap<String, Record>,
}
fn read<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&std::fs::read(p)?)?)
}
fn path<'a>(v: &'a Value, k: &str) -> Result<&'a Path> {
    Ok(Path::new(v[k].as_str().ok_or("missing path")?))
}
fn num(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().ok_or_else(|| format!("missing {k}").into())
}
fn save_store(dir: &Path, s: &Store) -> Result<()> {
    let temp = dir.join("shares.next.private.json");
    write_private_json(&temp, s)?;
    std::fs::rename(temp, dir.join("shares.private.json"))?;
    Ok(())
}
fn make_bundle(
    keys: &Keys,
    polynomials: Vec<Polynomial>,
    rows: Vec<Vec<Share>>,
    registry: &[Identity],
) -> Result<Bundle> {
    let context = digest(&polynomials)?;
    let mut packets = Vec::new();
    for (i, recipient) in registry.iter().enumerate() {
        packets.push(seal(
            recipient,
            &context,
            &serde_json::to_vec(&rows.iter().map(|r| r[i].clone()).collect::<Vec<_>>())?,
        )?);
    }
    let signature = sign(keys, &(&polynomials, &packets))?;
    Ok(Bundle {
        dealer: keys.public.clone(),
        polynomials,
        packets,
        signature,
    })
}
fn open_bundle(keys: &Keys, b: &Bundle, x: u32) -> Result<Vec<Record>> {
    verify_signature(&b.dealer, &(&b.polynomials, &b.packets), &b.signature)?;
    let packet = b
        .packets
        .iter()
        .find(|e| e.recipient == keys.public.id)
        .ok_or("missing recipient packet")?;
    let shares: Vec<Share> =
        serde_json::from_slice(&unseal(keys, packet, &digest(&b.polynomials)?)?)?;
    if shares.len() != b.polynomials.len() {
        return Err("share coverage mismatch".into());
    }
    shares
        .into_iter()
        .zip(b.polynomials.iter())
        .map(|(share, p)| {
            if share.x != x {
                return Err("wrong share coordinate".into());
            }
            check(p, &share)?;
            Ok(Record {
                polynomial: p.clone(),
                share,
            })
        })
        .collect()
}
fn receipt(keys: &Keys, store: &Store, out: &Path) -> Result<()> {
    let polynomials = store
        .records
        .values()
        .map(|r| r.polynomial.clone())
        .collect::<Vec<_>>();
    let body = json!({"epoch":store.epoch,"member":keys.public.id,"coverage":digest(&polynomials)?,"records":polynomials.len(),"durable":true});
    write_json(out, &json!({"body":body,"signature":sign(keys,&body)?}))
}
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 1 {
        return Err("usage: poc-vss JOB.json".into());
    }
    let j: Value = read(Path::new(&args[0]))?;
    match j["op"].as_str().ok_or("op missing")? {
        "install-mpc-output" => {
            let dir = path(&j, "dir")?;
            let k = keys(dir)?;
            let mut store: Store = read(&dir.join("shares.private.json"))?;
            let ps: Vec<Polynomial> = read(path(&j, "polynomials")?)?;
            let ss: Vec<Share> = read(path(&j, "shares")?)?;
            if ps.len() != 4 || ss.len() != 4 {
                return Err("four MPC limbs required".into());
            }
            for (p, s) in ps.into_iter().zip(ss) {
                if p.epoch != store.epoch || store.records.contains_key(&p.id) {
                    return Err("MPC output context/replay".into());
                }
                check(&p, &s)?;
                store.records.insert(
                    p.id.clone(),
                    Record {
                        polynomial: p,
                        share: s,
                    },
                );
            }
            save_store(dir, &store)?;
            receipt(&k, &store, path(&j, "out")?)?;
        }
        "certify-mpc" => {
            let dir = path(&j, "dir")?;
            let k = keys(dir)?;
            let body: Value = read(path(&j, "body")?)?;
            let report: Value = read(path(&j, "report")?)?;
            let computed = match report["operation"].as_str() {
                Some("update") => {
                    report["money_cohort_conservation"] == true
                        && body.get("next_state").is_some()
                        && report["request"] == body["request"]
                }
                Some("intex") => {
                    report["independent_floor"] == true && body.get("next_state").is_none()
                }
                Some("expiry") => {
                    report["private_limit_return"] == true && body.get("next_state").is_none()
                }
                _ => false,
            };
            if report["context"] != body["context"]
                || report["operation"] != body["operation"]
                || !computed
                || report["checked_u256_valid"] != true
            {
                return Err("MPC certificate execution context".into());
            }
            let entries = j["outputs"].as_array().ok_or("MPC certificate outputs")?;
            let mut hashes = Vec::new();
            let mut output_polynomials = Vec::new();
            for e in entries {
                let ps: Vec<Polynomial> = read(path(e, "polynomials")?)?;
                let ss: Vec<Share> = read(path(e, "shares")?)?;
                if ps.len() != 4 || ss.len() != 4 {
                    return Err("MPC certificate coverage".into());
                }
                for (p, s) in ps.iter().zip(ss) {
                    check(p, &s)?;
                }
                hashes.push(digest(&ps)?);
                output_polynomials.push(ps);
            }
            if serde_json::to_value(&hashes)? != body["polynomial_hashes"] {
                return Err("MPC output root mismatch".into());
            }
            if report["money_cohort_conservation"] == true {
                let layout = &report["transition_layout"];
                let next = &body["next_state"];
                let mut without_root = next.clone();
                let root = without_root
                    .as_object_mut()
                    .ok_or("next state object")?
                    .remove("root")
                    .ok_or("next state root")?;
                if root != digest(&without_root)?
                    || next["qualified"] != layout["qualified"]
                    || next["version"] != layout["version"]
                {
                    return Err("MPC next state root/anchor/version".into());
                }
                for label in ["active", "sold"] {
                    let rows = next[label].as_array().ok_or("next state rows")?;
                    let expected = layout[label].as_array().ok_or("executed MPC layout")?;
                    if rows.len() != expected.len() {
                        return Err("MPC next state row coverage".into());
                    }
                    for (row, want) in rows.iter().zip(expected) {
                        for field in ["id", "at", "sold_at"] {
                            if row[field] != want[field] {
                                return Err("MPC state metadata differs from execution".into());
                            }
                        }
                    }
                }
                let preserved = layout["preserved_sold"]
                    .as_array()
                    .ok_or("preserved sold history")?;
                if next["sold"]
                    .as_array()
                    .ok_or("sold array")?
                    .get(..preserved.len())
                    != Some(preserved.as_slice())
                {
                    return Err("MPC changed preserved history commitment".into());
                }
                let ids = layout["output_ids"]
                    .as_array()
                    .ok_or("MPC output identities")?;
                if ids.len() != output_polynomials.len() {
                    return Err("MPC output identity coverage".into());
                }
                for (i, (id, ps)) in ids.iter().zip(&output_polynomials).enumerate() {
                    let id = id.as_str().ok_or("output identity")?;
                    for (k, p) in ps.iter().enumerate() {
                        if p.id != format!("{id}:{k}") {
                            return Err("MPC output polynomial identity".into());
                        }
                    }
                    let row = next["active"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .chain(next["sold"].as_array().unwrap())
                        .find(|r| r["id"] == id)
                        .ok_or("MPC output missing from next state")?;
                    if row["polynomial_hash"] != hashes[i]
                        || format!(
                            "{}-{}",
                            row["output_tag"].as_str().ok_or("output tag")?,
                            row["output_name"].as_str().ok_or("output name")?
                        ) != id
                    {
                        return Err("MPC state commitment/output mapping".into());
                    }
                }
            }
            write_json(
                path(&j, "out")?,
                &json!({"body":body,"member":k.public,"signature":sign(&k,&body)?}),
            )?;
        }
        "sign-json" => {
            let k = keys(path(&j, "dir")?)?;
            let body: Value = read(path(&j, "body")?)?;
            write_json(
                path(&j, "out")?,
                &json!({"body":body,"member":k.public,"signature":sign(&k,&body)?}),
            )?;
        }
        "verify-json" => {
            let id: Identity = read(path(&j, "identity")?)?;
            let signed: Value = read(path(&j, "signed")?)?;
            verify_signature(
                &id,
                &signed["body"],
                signed["signature"].as_str().ok_or("signature")?,
            )?;
        }
        "interpolate-outputs" => {
            let files: Vec<String> = serde_json::from_value(j["evaluations"].clone())?;
            if files.len() != 3 {
                return Err("require three degree-one evaluations".into());
            }
            let points = files
                .iter()
                .map(|f| read::<Vec<String>>(Path::new(f)))
                .collect::<Result<Vec<_>>>()?;
            if points.iter().any(|p| p.len() != 4) {
                return Err("uint256 needs four limb evaluations".into());
            }
            let ps = (0..4)
                .map(|i| {
                    let a = point(&points[0][i])?;
                    let b = point(&points[1][i])?;
                    let c0 = (a * Fr::from(2u32) - b).into_affine();
                    let c1 = (b - a).into_affine();
                    if (c0 + c1 * Fr::from(3u32)).into_affine() != point(&points[2][i])? {
                        return Err("MPC output not degree-one committed sharing".into());
                    }
                    Ok(Polynomial {
                        id: format!("{}:{i}", j["id"].as_str().ok_or("output id")?),
                        epoch: num(&j, "epoch")?,
                        points: vec![point_hex(c0)?, point_hex(c1)?],
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            write_json(path(&j, "out")?, &ps)?;
        }
        "export-output" => {
            let rows: Vec<Share> = read(path(&j, "shares")?)?;
            let ps: Vec<Polynomial> = read(path(&j, "polynomials")?)?;
            let owner: Identity = read(path(&j, "owner")?)?;
            if rows.len() != 4 || ps.len() != 4 {
                return Err("four output limbs required".into());
            }
            let packets = rows
                .iter()
                .zip(&ps)
                .map(|(s, p)| {
                    check(p, s)?;
                    seal(&owner, &digest(p)?, &serde_json::to_vec(s)?)
                })
                .collect::<Result<Vec<_>>>()?;
            write_json(path(&j, "out")?, &packets)?;
        }
        "recover-note" => {
            let k = keys(path(&j, "dir")?)?;
            let files: Vec<String> = serde_json::from_value(j["packets"].clone())?;
            let ps: Vec<Polynomial> = read(path(&j, "polynomials")?)?;
            if files.len() < 2 || ps.len() != 4 {
                return Err("note recovery threshold/limbs".into());
            }
            let packets = files
                .iter()
                .map(|f| read::<Vec<Envelope>>(Path::new(f)))
                .collect::<Result<Vec<_>>>()?;
            if packets.iter().any(|p| p.len() != 4) {
                return Err("recovery envelope coverage".into());
            }
            let mut value = num_bigint::BigUint::from(0u32);
            let mut blinds = Vec::new();
            for i in 0..4 {
                let ss = packets
                    .iter()
                    .map(|row| {
                        let s: Share =
                            serde_json::from_slice(&unseal(&k, &row[i], &digest(&ps[i])?)?)?;
                        check(&ps[i], &s)?;
                        Ok(s)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let (y, z) = recover(&ss)?;
                let v = scalar_integer(y);
                if v.bits() > 64 || commit(&v, z)? != point(&ps[i].points[0])? {
                    return Err("recovered limb/commitment mismatch".into());
                }
                value += v << (64 * i);
                blinds.push(scalar_integer(z).to_string());
            }
            write_private_json(
                path(&j, "out")?,
                &outbe_private_lifecycle_poc::state::Note {
                    value: value.to_string(),
                    blinds,
                },
            )?;
        }
        "wallet-secrets" => {
            let w: outbe_private_lifecycle_poc::wire::Wallet = read(path(&j, "wallet")?)?;
            let mut secrets = if j["skip_nominal"] == true {
                Vec::new()
            } else {
                vec![json!({"id":w.offer.nft_hash,"value":w.nominal,"blind":w.blinder})]
            };
            if let Some(notes) = j["notes"].as_array() {
                for n in notes {
                    let note: outbe_private_lifecycle_poc::state::Note =
                        read(Path::new(n["path"].as_str().ok_or("note path")?))?;
                    let value = integer(&note.value)?;
                    let mask = (num_bigint::BigUint::from(1u32) << 64usize) - 1u32;
                    for i in 0usize..4 {
                        secrets.push(json!({"id":format!("{}:{i}",n["id"].as_str().ok_or("note id")?),"value":((&value>>(i*64)) & &mask).to_string(),"blind":note.blinds[i]}));
                    }
                }
            }
            write_private_json(path(&j, "out")?, &secrets)?;
        }
        "keygen" => keygen(
            path(&j, "dir")?,
            j["id"].as_str().ok_or("id missing")?.into(),
        )?,
        "deal" => {
            let keys = keys(path(&j, "dir")?)?;
            let registry: Vec<Identity> = read(path(&j, "registry")?)?;
            if registry.len() != 3 {
                return Err("experimental committee must have three members".into());
            }
            // The wallet is the only actor which reads this witness file.
            let secrets: Vec<Value> = read(path(&j, "secrets")?)?;
            let mut ps = Vec::new();
            let mut rows = Vec::new();
            for s in secrets {
                let id = s["id"].as_str().ok_or("secret id")?.to_string();
                let y = scalar(&integer(s["value"].as_str().ok_or("secret value")?)?)?;
                let z = scalar(&integer(s["blind"].as_str().ok_or("secret blind")?)?)?;
                let (p, ss) = deal(id, num(&j, "epoch")?, y, z, 3)?;
                ps.push(p);
                rows.push(ss);
            }
            write_json(path(&j, "out")?, &make_bundle(&keys, ps, rows, &registry)?)?;
        }
        "accept" => {
            let dir = path(&j, "dir")?;
            let keys = keys(dir)?;
            let b: Bundle = read(path(&j, "bundle")?)?;
            let rs = open_bundle(&keys, &b, num(&j, "x")?.try_into()?)?;
            let epoch = num(&j, "epoch")?;
            let mut store = if dir.join("shares.private.json").exists() {
                read::<Store>(&dir.join("shares.private.json"))?
            } else {
                Store {
                    epoch,
                    member: keys.public.clone(),
                    records: BTreeMap::new(),
                }
            };
            if store.epoch != epoch {
                return Err("store epoch mismatch".into());
            }
            for r in rs {
                if r.polynomial.epoch != epoch
                    || store.records.insert(r.polynomial.id.clone(), r).is_some()
                {
                    return Err("duplicate or wrong epoch VSS record".into());
                }
            }
            save_store(dir, &store)?;
            receipt(&keys, &store, path(&j, "out")?)?;
        }
        "rotate-deal" => {
            let dir = path(&j, "dir")?;
            let keys = keys(dir)?;
            let old: Store = read(&dir.join("shares.private.json"))?;
            let selected: Vec<u32> = serde_json::from_value(j["selected"].clone())?;
            if selected.len() != 2 {
                return Err("reshare needs two distinct old members".into());
            }
            let x = num(&j, "x")? as u32;
            let i = selected
                .iter()
                .position(|v| *v == x)
                .ok_or("unselected dealer")?;
            let l = lagrange(&selected, i)?;
            let epoch = num(&j, "epoch")?;
            if epoch != old.epoch + 1 {
                return Err("nonconsecutive epoch".into());
            }
            let registry: Vec<Identity> = read(path(&j, "registry")?)?;
            let mut ps = Vec::new();
            let mut rows = Vec::new();
            for r in old.records.values() {
                if r.share.x != x {
                    return Err("old coordinate mismatch".into());
                }
                check(&r.polynomial, &r.share)?;
                let (p, ss) = deal(
                    r.polynomial.id.clone(),
                    epoch,
                    l * scalar(&integer(&r.share.y)?)?,
                    l * scalar(&integer(&r.share.z)?)?,
                    3,
                )?;
                if point(&p.points[0])? != (eval(&r.polynomial, x)? * l).into_affine() {
                    return Err("reshare constant mismatch".into());
                }
                ps.push(p);
                rows.push(ss);
            }
            write_json(path(&j, "out")?, &make_bundle(&keys, ps, rows, &registry)?)?;
        }
        "rotate-accept" => {
            let dir = path(&j, "dir")?;
            let keys = keys(dir)?;
            let prior: Vec<Polynomial> = read(path(&j, "prior_polynomials")?)?;
            let old_registry: Vec<Identity> = read(path(&j, "old_registry")?)?;
            let selected: Vec<u32> = serde_json::from_value(j["selected"].clone())?;
            let files: Vec<String> = serde_json::from_value(j["bundles"].clone())?;
            if selected.len() != 2 || files.len() != 2 {
                return Err("bad reshare quorum".into());
            }
            let mut received = Vec::new();
            for (i, f) in files.iter().enumerate() {
                let b: Bundle = read(Path::new(f))?;
                if digest(&b.dealer)? != digest(&old_registry[(selected[i] - 1) as usize])? {
                    return Err("reshare dealer identity".into());
                }
                let rr = open_bundle(&keys, &b, num(&j, "x")? as u32)?;
                if rr.len() != prior.len() {
                    return Err("handoff incomplete coverage".into());
                }
                for (r, p) in rr.iter().zip(&prior) {
                    if r.polynomial.id != p.id
                        || r.polynomial.epoch != num(&j, "epoch")?
                        || num(&j, "epoch")? != p.epoch + 1
                    {
                        return Err("handoff context".into());
                    }
                    if point(&r.polynomial.points[0])?
                        != (eval(p, selected[i])? * lagrange(&selected, i)?).into_affine()
                    {
                        return Err("unlinked reshare contribution".into());
                    }
                }
                received.push(rr);
            }
            let mut store = Store {
                epoch: num(&j, "epoch")?,
                member: keys.public.clone(),
                records: BTreeMap::new(),
            };
            for i in 0..prior.len() {
                let a = &received[0][i];
                let b = &received[1][i];
                let poly = Polynomial {
                    id: prior[i].id.clone(),
                    epoch: store.epoch,
                    points: (0..2)
                        .map(|k| {
                            point_hex(
                                (point(&a.polynomial.points[k])? + point(&b.polynomial.points[k])?)
                                    .into_affine(),
                            )
                        })
                        .collect::<Result<_>>()?,
                };
                if poly.points[0] != prior[i].points[0] {
                    return Err("constant changed during rotation".into());
                }
                let s = Share {
                    x: num(&j, "x")? as u32,
                    y: scalar_integer(
                        scalar(&integer(&a.share.y)?)? + scalar(&integer(&b.share.y)?)?,
                    )
                    .to_string(),
                    z: scalar_integer(
                        scalar(&integer(&a.share.z)?)? + scalar(&integer(&b.share.z)?)?,
                    )
                    .to_string(),
                };
                check(&poly, &s)?;
                store.records.insert(
                    poly.id.clone(),
                    Record {
                        polynomial: poly,
                        share: s,
                    },
                );
            }
            save_store(dir, &store)?;
            write_json(
                path(&j, "polynomials_out")?,
                &store
                    .records
                    .values()
                    .map(|r| r.polynomial.clone())
                    .collect::<Vec<_>>(),
            )?;
            receipt(&keys, &store, path(&j, "out")?)?;
        }
        "aggregate" => {
            if j["closed"] != true {
                return Err("aggregate opening before final close".into());
            }
            let keys = keys(path(&j, "dir")?)?;
            let s: Store = read(&path(&j, "dir")?.join("shares.private.json"))?;
            if s.epoch != num(&j, "epoch")? {
                return Err("aggregate stale epoch".into());
            }
            let groups: BTreeMap<String, Vec<String>> =
                serde_json::from_value(j["groups"].clone())?;
            let mut outputs = BTreeMap::new();
            for (group, ids) in groups {
                let mut y = Fr::zero();
                let mut z = y;
                let mut x = 0;
                let mut seen = std::collections::BTreeSet::new();
                for id in ids {
                    if !seen.insert(id.clone()) {
                        return Err("duplicate aggregate record".into());
                    }
                    let r = s.records.get(&id).ok_or("missing aggregate input")?;
                    check(&r.polynomial, &r.share)?;
                    x = r.share.x;
                    y += scalar(&integer(&r.share.y)?)?;
                    z += scalar(&integer(&r.share.z)?)?;
                }
                outputs.insert(
                    group,
                    Share {
                        x,
                        y: scalar_integer(y).to_string(),
                        z: scalar_integer(z).to_string(),
                    },
                );
            }
            let body = json!({"epoch":s.epoch,"root":j["root"],"shares":outputs});
            write_json(
                path(&j, "out")?,
                &json!({"body":body,"signature":sign(&keys,&body)?}),
            )?;
        }
        "open" => {
            let ps: Vec<Polynomial> = read(path(&j, "polynomials")?)?;
            let registry: Vec<Identity> = read(path(&j, "registry")?)?;
            let files: Vec<String> = serde_json::from_value(j["shares"].clone())?;
            if files.len() != 2 {
                return Err("opening threshold two".into());
            }
            let mut all = Vec::new();
            for (i, p) in files.iter().enumerate() {
                let v: Value = read(Path::new(p))?;
                verify_signature(
                    &registry[i],
                    &v["body"],
                    v["signature"].as_str().ok_or("signature")?,
                )?;
                if v["body"]["root"] != j["root"] || v["body"]["epoch"] != j["epoch"] {
                    return Err("aggregate certificate context".into());
                }
                all.push(v);
            }
            let groups: BTreeMap<String, Vec<String>> =
                serde_json::from_value(j["groups"].clone())?;
            let mut sums = BTreeMap::new();
            for (g, ids) in groups {
                let mut points = [ark_ed_on_bn254::EdwardsProjective::zero(); 2];
                for id in ids {
                    let p = ps.iter().find(|p| p.id == id).ok_or("aggregate coverage")?;
                    for k in 0..2 {
                        points[k] += point(&p.points[k])?;
                    }
                }
                let p = Polynomial {
                    id: g.clone(),
                    epoch: num(&j, "epoch")?,
                    points: points
                        .into_iter()
                        .map(|p| point_hex(p.into_affine()))
                        .collect::<Result<_>>()?,
                };
                let shares = all
                    .iter()
                    .map(|v| {
                        serde_json::from_value::<Share>(v["body"]["shares"][&g].clone())
                            .map_err(Into::into)
                    })
                    .collect::<Result<Vec<_>>>()?;
                for s in &shares {
                    check(&p, s)?;
                }
                let (a, r) = recover(&shares)?;
                if commit(&scalar_integer(a), r)? != point(&p.points[0])? {
                    return Err("aggregate commitment mismatch".into());
                }
                if scalar_integer(a).bits() > 136 {
                    return Err("source count aggregate bound exceeded".into());
                }
                sums.insert(g, scalar_integer(a).to_string());
            }
            write_json(
                path(&j, "out")?,
                &json!({"root":j["root"],"sums":sums,"verified":true}),
            )?;
        }
        "commit-share" => {
            let input: Vec<Share> = read(path(&j, "input")?)?;
            let points = input
                .iter()
                .map(|s| point_hex(commit(&integer(&s.y)?, scalar(&integer(&s.z)?)?)?))
                .collect::<Result<Vec<_>>>()?;
            write_json(path(&j, "out")?, &points)?;
        }
        "recover-owner" => {
            let k = keys(path(&j, "dir")?)?;
            let files: Vec<String> = serde_json::from_value(j["packets"].clone())?;
            let p: Polynomial = read(path(&j, "polynomial")?)?;
            let mut shares = Vec::new();
            for f in files {
                let e: Envelope = read(Path::new(&f))?;
                let s: Share = serde_json::from_slice(&unseal(&k, &e, &digest(&p)?)?)?;
                check(&p, &s)?;
                shares.push(s);
            }
            let (y, z) = recover(&shares)?;
            if commit(&scalar_integer(y), z)? != point(&p.points[0])? {
                return Err("recovered owner commitment".into());
            }
            write_private_json(
                path(&j, "out")?,
                &json!({"id":p.id,"value":scalar_integer(y).to_string(),"blind":scalar_integer(z).to_string()}),
            )?;
        }
        "export-owner" => {
            let s: Share = read(path(&j, "share")?)?;
            let p: Polynomial = read(path(&j, "polynomial")?)?;
            check(&p, &s)?;
            let owner: Identity = read(path(&j, "owner")?)?;
            write_json(
                path(&j, "out")?,
                &seal(&owner, &digest(&p)?, &serde_json::to_vec(&s)?)?,
            )?;
        }
        _ => return Err("unknown VSS operation".into()),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("VSS failed: {e}");
        std::process::exit(1)
    }
}
