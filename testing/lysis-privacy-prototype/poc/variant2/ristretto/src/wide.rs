//! Exact public-coefficient integer relations. Values have separately proven
//! 16x16-bit ranges. 48 columns cover a uint256 times a 512-bit coefficient.
//! Signed 32-bit carries imply each local message is <2^48 << Ristretto order.
use crate::note_link::{prove_zero, verify_zero, Opening};
use crate::*;
use num_bigint::{BigInt, Sign};
use num_traits::{ToPrimitive, Zero};
const COLS: usize = 48;
const OFFSET: u64 = 1u64 << 31;
#[derive(Clone, Serialize, Deserialize)]
pub struct Relation {
    pub coefficients: Vec<BigInt>,
    pub rhs: BigUint,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Proof {
    pub carries: Vec<Bytes>,
    pub range: Vec<u8>,
    pub openings: Vec<Opening>,
}
fn validate(r: &Relation, n: usize) -> Result<Vec<Vec<i64>>> {
    if n == 0 || n > 8 || r.coefficients.len() != n || r.rhs.bits() > 256 {
        return Err("wide relation shape".into());
    }
    r.coefficients
        .iter()
        .map(|c| {
            if c.bits() > 512 {
                return Err("coefficient overflow".into());
            }
            let s = if c.sign() == Sign::Minus { -1 } else { 1 };
            let u = c.magnitude();
            Ok((0usize..32)
                .map(|i| {
                    (((u >> (16 * i)) & BigUint::from(65535u32))
                        .to_u64()
                        .unwrap() as i64)
                        * s
                })
                .collect())
        })
        .collect()
}
fn signed(v: i64) -> Scalar {
    if v < 0 {
        -Scalar::from((-v) as u64)
    } else {
        Scalar::from(v as u64)
    }
}
fn terms(fs: &[Bytes], cd: &[Vec<i64>], r: &Relation, k: usize) -> Result<RistrettoPoint> {
    let rd = if k < 16 {
        ((&r.rhs >> (16 * k)) & BigUint::from(65535u32))
            .to_u64()
            .unwrap()
    } else {
        0
    };
    let mut p = -Scalar::from(rd) * pc().B;
    for j in 0..cd.len() {
        for i in 0..16 {
            if k >= i && k - i < 32 && cd[j][k - i] != 0 {
                p += signed(cd[j][k - i]) * point(fs[j * 16 + i])?;
            }
        }
    }
    Ok(p)
}
pub fn prove(
    fs: &[Bytes],
    values: &[BigUint],
    blinds: &[Vec<Scalar>],
    r: &Relation,
    context: &Bytes,
) -> Result<Proof> {
    let cd = validate(r, values.len())?;
    if fs.len() < values.len() * 16 || blinds.len() != values.len() {
        return Err("wide witness shape".into());
    }
    let ds = values.iter().map(digits).collect::<Result<Vec<_>>>()?;
    let mut carry = BigInt::zero();
    let mut us = vec![];
    for k in 0..COLS {
        let mut total = carry.clone();
        for j in 0..cd.len() {
            for i in 0..16 {
                if k >= i && k - i < 32 {
                    total += BigInt::from(ds[j][i]) * cd[j][k - i];
                }
            }
        }
        if k < 16 {
            total -= BigInt::from(
                ((&r.rhs >> (16 * k)) & BigUint::from(65535u32))
                    .to_u64()
                    .unwrap(),
            );
        }
        if &total % 65536u32 != BigInt::zero() {
            return Err("wide nonintegral carry".into());
        }
        carry = total / 65536u32;
        let u = &carry + BigInt::from(OFFSET);
        us.push(u.to_u64().ok_or("wide carry range")?);
    }
    if !carry.is_zero() {
        return Err("wide overflow or unequal totals".into());
    }
    let mut cr = (0..COLS)
        .map(|_| Scalar::random(&mut OsRng))
        .collect::<Vec<_>>();
    cr[COLS - 1] = Scalar::ZERO;
    let rc = hash(&(b"wide-range", context, r, fs));
    let (carries, range) = prove_range(&us, &cr, 32, &rc)?;
    let ctx = hash(&(b"wide-openings", context, r, fs, &carries, &range));
    let mut openings = vec![];
    for k in 0..COLS {
        let mut e = terms(fs, &cd, r, k)?;
        let mut blind = Scalar::ZERO;
        for j in 0..cd.len() {
            for i in 0..16 {
                if k >= i && k - i < 32 {
                    blind += signed(cd[j][k - i]) * blinds[j][i];
                }
            }
        }
        if k > 0 {
            e += point(carries[k - 1])? - Scalar::from(OFFSET) * pc().B;
            blind += cr[k - 1];
        }
        e -= Scalar::from(65536u64) * (point(carries[k])? - Scalar::from(OFFSET) * pc().B);
        blind -= Scalar::from(65536u64) * cr[k];
        openings.push(prove_zero(e, blind, &hash(&(ctx, k))));
    }
    Ok(Proof {
        carries,
        range,
        openings,
    })
}
pub fn verify(fs: &[Bytes], n: usize, r: &Relation, p: &Proof, context: &Bytes) -> Result<()> {
    let cd = validate(r, n)?;
    if fs.len() < n * 16 || p.carries.len() != 64 || p.openings.len() != COLS {
        return Err("wide proof shape".into());
    }
    for x in &p.carries[COLS..] {
        if *x != enc(RistrettoPoint::default()) {
            return Err("wide padding".into());
        }
    }
    verify_range(
        &p.carries,
        &p.range,
        32,
        &hash(&(b"wide-range", context, r, fs)),
    )?;
    if p.carries[COLS - 1] != enc(Scalar::from(OFFSET) * pc().B) {
        return Err("wide terminal carry".into());
    }
    let ctx = hash(&(b"wide-openings", context, r, fs, &p.carries, &p.range));
    for k in 0..COLS {
        let mut e = terms(fs, &cd, r, k)?;
        if k > 0 {
            e += point(p.carries[k - 1])? - Scalar::from(OFFSET) * pc().B;
        }
        e -= Scalar::from(65536u64) * (point(p.carries[k])? - Scalar::from(OFFSET) * pc().B);
        verify_zero(e, &p.openings[k], &hash(&(ctx, k)))?;
    }
    Ok(())
}
