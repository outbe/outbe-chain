//! Experimental native balance backend; not audited or production consensus.
pub mod bridge;
pub mod money;

use bulletproofs::{BulletproofGens, PedersenGens, RangeProof};
use curve25519_dalek::{
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
};
use merlin::Transcript;
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive};
pub use outbe_private_lifecycle_poc::crypto::Result;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::OnceLock};
pub type Bytes = [u8; 32];
pub fn pc() -> &'static PedersenGens {
    static P: OnceLock<PedersenGens> = OnceLock::new();
    P.get_or_init(PedersenGens::default)
}
pub fn bp() -> &'static BulletproofGens {
    static P: OnceLock<BulletproofGens> = OnceLock::new();
    P.get_or_init(|| BulletproofGens::new(16, 128))
}
pub fn point(p: Bytes) -> Result<RistrettoPoint> {
    CompressedRistretto(p)
        .decompress()
        .ok_or_else(|| "invalid Ristretto encoding".into())
}
pub fn scalar(s: Bytes) -> Result<Scalar> {
    Option::<Scalar>::from(Scalar::from_canonical_bytes(s))
        .ok_or_else(|| "noncanonical scalar".into())
}
pub fn enc(p: RistrettoPoint) -> Bytes {
    p.compress().to_bytes()
}
pub fn hash<T: Serialize>(v: &T) -> Bytes {
    Sha256::digest(bincode::serialize(v).expect("serializable transcript")).into()
}
pub fn challenge<T: Serialize>(domain: &[u8], v: &T) -> Scalar {
    Scalar::from_bytes_mod_order(hash(&(domain, v)))
}
pub fn transcript(domain: &'static [u8], context: &Bytes) -> Transcript {
    let mut t = Transcript::new(domain);
    t.append_message(b"context", context);
    t
}
pub fn digits(n: &BigUint) -> Result<Vec<u64>> {
    if n.bits() > 256 {
        return Err("uint256 overflow".into());
    }
    Ok((0usize..16)
        .map(|i| {
            ((n >> (16 * i)) & BigUint::from(65535u32))
                .to_u64()
                .unwrap()
        })
        .collect())
}
pub fn from_digits(ds: &[u64]) -> Result<BigUint> {
    if ds.len() != 16 || ds.iter().any(|v| *v >= 65536) {
        return Err("not normalized uint256 chunks".into());
    }
    Ok(ds
        .iter()
        .enumerate()
        .fold(BigUint::from(0u32), |n, (i, d)| {
            n + (BigUint::from(*d) << (16 * i))
        }))
}
pub fn keygen() -> Scalar {
    loop {
        let s = Scalar::random(&mut OsRng);
        if s != Scalar::ZERO {
            return s;
        }
    }
}
pub fn public_key(s: Scalar) -> Result<Bytes> {
    if s == Scalar::ZERO {
        return Err("zero key".into());
    }
    Ok(enc(s.invert() * pc().B_blinding))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cipher {
    pub key: Bytes,
    pub c: Vec<Bytes>,
    pub d: Vec<Bytes>,
}
impl Cipher {
    pub fn validate(&self) -> Result<()> {
        if self.c.len() != 16 || self.d.len() != 16 || point(self.key)? == RistrettoPoint::default()
        {
            return Err("cipher shape/key".into());
        }
        for p in self.c.iter().chain(&self.d) {
            point(*p)?;
        }
        Ok(())
    }
    pub fn encrypt(n: &BigUint, key: Bytes) -> Result<Self> {
        let pk = point(key)?;
        if pk == RistrettoPoint::default() {
            return Err("identity public key".into());
        }
        let mut c = vec![];
        let mut d = vec![];
        for x in digits(n)? {
            let r = Scalar::random(&mut OsRng);
            c.push(enc(pc().commit(Scalar::from(x), r)));
            d.push(enc(r * pk));
        }
        Ok(Self { key, c, d })
    }
    pub fn public(n: &BigUint, key: Bytes) -> Result<Self> {
        let c = digits(n)?
            .into_iter()
            .map(|v| enc(Scalar::from(v) * pc().B))
            .collect();
        let ct = Self {
            key,
            c,
            d: vec![enc(RistrettoPoint::default()); 16],
        };
        ct.validate()?;
        Ok(ct)
    }
    /// Exact 16-bit lookup, constructed in the measured cold process. No r cache.
    pub fn decrypt(&self, s: Scalar) -> Result<BigUint> {
        self.validate()?;
        if self.key != public_key(s)? {
            return Err("wrong recovery key".into());
        }
        static TABLE: OnceLock<HashMap<Bytes, u64>> = OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let mut m = HashMap::with_capacity(65536);
            let mut p = RistrettoPoint::default();
            for i in 0..65536 {
                m.insert(enc(p), i);
                p += pc().B;
            }
            m
        });
        let mut ds = vec![];
        for (c, d) in self.c.iter().zip(&self.d) {
            let p = enc(point(*c)? - s * point(*d)?);
            ds.push(
                *table
                    .get(&p)
                    .ok_or("cipher not a normalized 16-bit chunk")?,
            );
        }
        from_digits(&ds)
    }
    /// Linear addition returns unnormalized columns. Caller must prove a bounded
    /// normalization before accepting this as a uint256 available balance.
    pub fn add_columns(&self, other: &Self) -> Result<Self> {
        self.validate()?;
        other.validate()?;
        if self.key != other.key {
            return Err("key mismatch".into());
        }
        Ok(Self {
            key: self.key,
            c: self
                .c
                .iter()
                .zip(&other.c)
                .map(|(a, b)| Ok(enc(point(*a)? + point(*b)?)))
                .collect::<Result<_>>()?,
            d: self
                .d
                .iter()
                .zip(&other.d)
                .map(|(a, b)| Ok(enc(point(*a)? + point(*b)?)))
                .collect::<Result<_>>()?,
        })
    }
}

pub fn prove_range(
    values: &[u64],
    blinds: &[Scalar],
    bits: usize,
    context: &Bytes,
) -> Result<(Vec<Bytes>, Vec<u8>)> {
    let mut vs = values.to_vec();
    let mut rs = blinds.to_vec();
    let len = vs.len().next_power_of_two();
    if vs.is_empty() || len > 128 || ![8, 16].contains(&bits) {
        return Err("range profile".into());
    }
    vs.resize(len, 0);
    rs.resize(len, Scalar::ZERO);
    let (proof, cs) = RangeProof::prove_multiple(
        bp(),
        pc(),
        &mut transcript(b"OUTBE-V2-RANGE-1", context),
        &vs,
        &rs,
        bits,
    )?;
    Ok((
        cs.into_iter().map(|p| p.to_bytes()).collect(),
        proof.to_bytes(),
    ))
}
pub fn verify_range(cs: &[Bytes], proof: &[u8], bits: usize, context: &Bytes) -> Result<()> {
    if cs.is_empty() || !cs.len().is_power_of_two() || cs.len() > 128 || ![8, 16].contains(&bits) {
        return Err("range shape".into());
    }
    RangeProof::from_bytes(proof)?.verify_multiple(
        bp(),
        pc(),
        &mut transcript(b"OUTBE-V2-RANGE-1", context),
        &cs.iter()
            .copied()
            .map(CompressedRistretto)
            .collect::<Vec<_>>(),
        bits,
    )?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CipherEquality {
    pub a_key: Bytes,
    pub a: Vec<Bytes>,
    pub z_key: Bytes,
    pub z: Vec<Bytes>,
}
pub fn equality_prove(
    ct: &Cipher,
    s: Scalar,
    fs: &[Bytes],
    rs: &[Scalar],
    context: &Bytes,
) -> Result<CipherEquality> {
    ct.validate()?;
    if public_key(s)? != ct.key || fs.len() != 16 || rs.len() != 16 {
        return Err("cipher equality shape/key".into());
    }
    let k = Scalar::random(&mut OsRng);
    let ws: Vec<_> = (0..16).map(|_| Scalar::random(&mut OsRng)).collect();
    let a_key = enc(k * point(ct.key)?);
    let a =
        ct.d.iter()
            .zip(&ws)
            .map(|(d, w)| Ok(enc(k * point(*d)? - w * pc().B_blinding)))
            .collect::<Result<Vec<_>>>()?;
    let c = challenge(b"OUTBE-V2-CIPHER-EQUALITY-1", &(context, ct, fs, a_key, &a));
    Ok(CipherEquality {
        a_key,
        a,
        z_key: (k + c * s).to_bytes(),
        z: ws
            .iter()
            .zip(rs)
            .map(|(w, r)| (w + c * r).to_bytes())
            .collect(),
    })
}
pub fn equality_verify(
    ct: &Cipher,
    fs: &[Bytes],
    p: &CipherEquality,
    context: &Bytes,
) -> Result<()> {
    ct.validate()?;
    if fs.len() != 16 || p.a.len() != 16 || p.z.len() != 16 {
        return Err("equality shape".into());
    }
    let c = challenge(
        b"OUTBE-V2-CIPHER-EQUALITY-1",
        &(context, ct, fs, p.a_key, &p.a),
    );
    let z = scalar(p.z_key)?;
    if z * point(ct.key)? != point(p.a_key)? + c * pc().B_blinding {
        return Err("key knowledge".into());
    }
    for i in 0..16 {
        if z * point(ct.d[i])? - scalar(p.z[i])? * pc().B_blinding
            != point(p.a[i])? + c * (point(ct.c[i])? - point(fs[i])?)
        {
            return Err("cipher plaintext equality".into());
        }
    }
    Ok(())
}

pub fn max_u256() -> BigUint {
    (BigUint::one() << 256) - 1u32
}
