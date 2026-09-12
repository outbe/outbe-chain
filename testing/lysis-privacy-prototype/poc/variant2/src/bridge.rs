//! Deliberately conservative bitwise cross-group bridge. Each bit has a joint
//! (Baby AND Ristretto) OR proof, then weighted zero-message opening proofs.
//! No single uint256 is reduced into either scalar field.
use crate::*;
use ark_ec::{AffineRepr, CurveGroup};
use ark_ed_on_bn254::{EdwardsAffine as Baby, Fr as BS};
use ark_ff::{PrimeField, UniformRand};
use outbe_private_lifecycle_poc::{crypto as bc, state::Note, vss};

fn benc(p: Baby) -> Bytes {
    bc::encode(&p).unwrap().try_into().unwrap()
}
fn bpoint(p: Bytes) -> Result<Baby> {
    bc::decode(&p)
}
fn bscalar(p: Bytes) -> Result<BS> {
    bc::decode(&p)
}
fn bsenc(s: BS) -> Bytes {
    bc::encode(&s).unwrap().try_into().unwrap()
}
fn cb(c: [u8; 16]) -> BS {
    BS::from_le_bytes_mod_order(&c)
}
fn cr(c: [u8; 16]) -> Scalar {
    let mut b = [0; 32];
    b[..16].copy_from_slice(&c);
    Scalar::from_bytes_mod_order(b)
}
fn xor(a: [u8; 16], b: [u8; 16]) -> [u8; 16] {
    std::array::from_fn(|i| a[i] ^ b[i])
}
fn random_challenge() -> [u8; 16] {
    use rand::RngCore;
    let mut b = [0; 16];
    OsRng.fill_bytes(&mut b);
    b
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Bit {
    pub baby: Bytes,
    pub rist: Bytes,
    pub a: [Bytes; 4],
    pub c0: [u8; 16],
    pub z: [Bytes; 4],
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Bridge {
    pub bits: Vec<Bit>,
    pub a_baby: Vec<Bytes>,
    pub a_rist: Vec<Bytes>,
    pub z_baby: Vec<Bytes>,
    pub z_rist: Vec<Bytes>,
}
fn challenge_bridge(baby: &[String], fs: &[Bytes], context: &Bytes, p: &Bridge) -> [u8; 16] {
    let heads = p
        .bits
        .iter()
        .map(|b| (b.baby, b.rist, b.a))
        .collect::<Vec<_>>();
    hash(&(
        b"OUTBE-V2-JOINT-BIT-BRIDGE-1",
        context,
        baby,
        fs,
        heads,
        &p.a_baby,
        &p.a_rist,
    ))[..16]
        .try_into()
        .unwrap()
}
pub fn prove(note: &Note, fs: &[Bytes], rs: &[Scalar], context: &Bytes) -> Result<Bridge> {
    if fs.len() != 16 || rs.len() != 16 {
        return Err("bridge shape".into());
    }
    let n = bc::integer(&note.value)?;
    let baby = note.commitments()?;
    let (g, h) = bc::generators();
    let mut out = Bridge {
        bits: vec![],
        a_baby: vec![],
        a_rist: vec![],
        z_baby: vec![],
        z_rist: vec![],
    };
    let mut secrets = vec![];
    let mut db = vec![BS::from(0u64); 4];
    let mut dr = vec![Scalar::ZERO; 16];
    for i in 0..256 {
        let bit = usize::from(n.bit(i as u64));
        let rb = BS::rand(&mut OsRng);
        let rr = Scalar::random(&mut OsRng);
        let b = (g * BS::from(bit as u64) + h * rb).into_affine();
        let r = pc().commit(Scalar::from(bit as u64), rr);
        let wb = BS::rand(&mut OsRng);
        let wr = Scalar::random(&mut OsRng);
        let cf = random_challenge();
        let zb = BS::rand(&mut OsRng);
        let zr = Scalar::random(&mut OsRng);
        let false_bit = 1 - bit;
        let mut a = [[0; 32]; 4];
        a[2 * bit] = benc((h * wb).into_affine());
        a[2 * bit + 1] = enc(wr * pc().B_blinding);
        a[2 * false_bit] = benc(
            (h * zb - (b.into_group() - g * BS::from(false_bit as u64)) * cb(cf)).into_affine(),
        );
        a[2 * false_bit + 1] =
            enc(zr * pc().B_blinding - cr(cf) * (r - Scalar::from(false_bit as u64) * pc().B));
        out.bits.push(Bit {
            baby: benc(b),
            rist: enc(r),
            a,
            c0: [0; 16],
            z: [[0; 32]; 4],
        });
        secrets.push((bit, rb, rr, wb, wr, cf, zb, zr));
        db[i / 64] += rb * BS::from(1u64 << (i % 64));
        dr[i / 16] += rr * Scalar::from(1u64 << (i % 16));
    }
    for i in 0..4 {
        db[i] = bc::scalar(&bc::integer(&note.blinds[i])?)? - db[i];
    }
    for i in 0..16 {
        dr[i] = rs[i] - dr[i];
    }
    let wb: Vec<_> = (0..4).map(|_| BS::rand(&mut OsRng)).collect();
    let wr: Vec<_> = (0..16).map(|_| Scalar::random(&mut OsRng)).collect();
    out.a_baby = wb.iter().map(|w| benc((h * w).into_affine())).collect();
    out.a_rist = wr.iter().map(|w| enc(w * pc().B_blinding)).collect();
    let ch = challenge_bridge(&baby, fs, context, &out);
    for (p, (bit, rb, rr, wb, wr, cf, zb, zr)) in out.bits.iter_mut().zip(secrets) {
        let real = xor(ch, cf);
        p.c0 = if bit == 0 { real } else { cf };
        p.z[2 * bit] = bsenc(wb + cb(real) * rb);
        p.z[2 * bit + 1] = (wr + cr(real) * rr).to_bytes();
        p.z[2 * (1 - bit)] = bsenc(zb);
        p.z[2 * (1 - bit) + 1] = zr.to_bytes();
    }
    out.z_baby = wb
        .iter()
        .zip(db)
        .map(|(w, d)| bsenc(*w + cb(ch) * d))
        .collect();
    out.z_rist = wr
        .iter()
        .zip(dr)
        .map(|(w, d)| (*w + cr(ch) * d).to_bytes())
        .collect();
    Ok(out)
}
pub fn verify(baby: &[String], fs: &[Bytes], context: &Bytes, p: &Bridge) -> Result<()> {
    if baby.len() != 4
        || fs.len() != 16
        || p.bits.len() != 256
        || p.a_baby.len() != 4
        || p.z_baby.len() != 4
        || p.a_rist.len() != 16
        || p.z_rist.len() != 16
    {
        return Err("bridge proof shape".into());
    }
    let targets = baby
        .iter()
        .map(|s| vss::point(s))
        .collect::<Result<Vec<_>>>()?;
    let (g, h) = bc::generators();
    let ch = challenge_bridge(baby, fs, context, p);
    let mut sum_b = vec![Baby::zero().into_group(); 4];
    let mut sum_r = vec![RistrettoPoint::default(); 16];
    for (i, bit) in p.bits.iter().enumerate() {
        let b = bpoint(bit.baby)?;
        let r = point(bit.rist)?;
        let cs = [bit.c0, xor(ch, bit.c0)];
        for j in 0..2 {
            if h * bscalar(bit.z[2 * j])?
                != bpoint(bit.a[2 * j])?.into_group()
                    + (b.into_group() - g * BS::from(j as u64)) * cb(cs[j])
            {
                return Err("joint Baby bit branch".into());
            }
            if scalar(bit.z[2 * j + 1])? * pc().B_blinding
                != point(bit.a[2 * j + 1])? + cr(cs[j]) * (r - Scalar::from(j as u64) * pc().B)
            {
                return Err("joint Ristretto bit branch".into());
            }
        }
        sum_b[i / 64] += b * BS::from(1u64 << (i % 64));
        sum_r[i / 16] += Scalar::from(1u64 << (i % 16)) * r;
    }
    for i in 0..4 {
        if h * bscalar(p.z_baby[i])?
            != bpoint(p.a_baby[i])?.into_group() + (targets[i].into_group() - sum_b[i]) * cb(ch)
        {
            return Err("Baby weighted limb opening".into());
        }
    }
    for i in 0..16 {
        if scalar(p.z_rist[i])? * pc().B_blinding
            != point(p.a_rist[i])? + cr(ch) * (point(fs[i])? - sum_r[i])
        {
            return Err("Ristretto weighted chunk opening".into());
        }
    }
    Ok(())
}
