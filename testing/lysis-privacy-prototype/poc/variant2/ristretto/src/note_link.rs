//! Four same-group 64-bit limb links; no cross-curve proof or integer modulo-q alias.
use crate::state::Note;
use crate::*;
#[derive(Clone, Serialize, Deserialize)]
pub struct Opening {
    pub a: Bytes,
    pub z: Bytes,
}
pub fn prove_zero(target: RistrettoPoint, r: Scalar, context: &Bytes) -> Opening {
    let w = Scalar::random(&mut OsRng);
    let a = enc(w * pc().B_blinding);
    let c = challenge(b"OUTBE-RISTRETTO-ZERO-1", &(context, enc(target), a));
    Opening {
        a,
        z: (w + c * r).to_bytes(),
    }
}
pub fn verify_zero(target: RistrettoPoint, p: &Opening, context: &Bytes) -> Result<()> {
    let c = challenge(b"OUTBE-RISTRETTO-ZERO-1", &(context, enc(target), p.a));
    if scalar(p.z)? * pc().B_blinding != point(p.a)? + c * target {
        return Err("same-group opening".into());
    }
    Ok(())
}
fn targets(notes: &[String], fs: &[Bytes]) -> Result<Vec<RistrettoPoint>> {
    if notes.len() != 4 || fs.len() != 16 {
        return Err("limb link shape".into());
    }
    (0..4)
        .map(|i| {
            let mut p = crypto::point_decode(&notes[i])?;
            for j in 0..4 {
                p -= Scalar::from(1u64 << (16 * j)) * point(fs[4 * i + j])?;
            }
            Ok(p)
        })
        .collect()
}
pub fn prove(n: &Note, fs: &[Bytes], rs: &[Scalar], context: &Bytes) -> Result<Vec<Opening>> {
    if rs.len() != 16 {
        return Err("limb blinds".into());
    }
    let ts = targets(&n.commitments()?, fs)?;
    (0..4)
        .map(|i| {
            let mut r = crypto::scalar(&crypto::integer(&n.blinds[i])?)?;
            for j in 0..4 {
                r -= Scalar::from(1u64 << (16 * j)) * rs[4 * i + j];
            }
            Ok(prove_zero(ts[i], r, &hash(&(context, i))))
        })
        .collect()
}
pub fn verify(notes: &[String], fs: &[Bytes], ps: &[Opening], context: &Bytes) -> Result<()> {
    if ps.len() != 4 {
        return Err("limb proof count".into());
    }
    for (i, t) in targets(notes, fs)?.into_iter().enumerate() {
        verify_zero(t, &ps[i], &hash(&(context, i)))?;
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SourceLink {
    pub top_range: Vec<u8>,
    pub upper: Vec<Opening>,
    pub opening: Opening,
}
pub fn prove_source(
    source: &str,
    fs: &[Bytes],
    rs: &[Scalar],
    value: &BigUint,
    r: Scalar,
    context: &Bytes,
) -> Result<SourceLink> {
    if value.bits() > 104 || fs.len() != 16 || rs.len() != 16 {
        return Err("source104".into());
    }
    let ds = digits(value)?;
    let (top, top_range) = prove_range(&[ds[6]], &[rs[6]], 8, &hash(&(context, b"top8")))?;
    if top[0] != fs[6] {
        return Err("source top link".into());
    }
    let mut target = crypto::point_decode(source)?;
    let mut blind = r;
    let mut factor = Scalar::ONE;
    for i in 0..7 {
        target -= factor * point(fs[i])?;
        blind -= factor * rs[i];
        factor *= Scalar::from(65536u64);
    }
    let upper = (7..16)
        .map(|i| {
            Ok(prove_zero(
                point(fs[i])?,
                rs[i],
                &hash(&(context, b"upper", i)),
            ))
        })
        .collect::<Result<_>>()?;
    Ok(SourceLink {
        top_range,
        upper,
        opening: prove_zero(target, blind, &hash(&(context, b"source104"))),
    })
}
pub fn verify_source(source: &str, fs: &[Bytes], p: &SourceLink, context: &Bytes) -> Result<()> {
    if fs.len() != 16 || p.upper.len() != 9 {
        return Err("source proof shape".into());
    }
    verify_range(&[fs[6]], &p.top_range, 8, &hash(&(context, b"top8")))?;
    for i in 7..16 {
        verify_zero(
            point(fs[i])?,
            &p.upper[i - 7],
            &hash(&(context, b"upper", i)),
        )?;
    }
    let mut target = crypto::point_decode(source)?;
    let mut factor = Scalar::ONE;
    for i in 0..7 {
        target -= factor * point(fs[i])?;
        factor *= Scalar::from(65536u64);
    }
    verify_zero(target, &p.opening, &hash(&(context, b"source104")))
}
