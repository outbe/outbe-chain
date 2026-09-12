//! Sequential source-opening certificates, entirely on Ristretto. Hidden state
//! digests bind the source witness and intermediate Edwards representatives.
//! Node acceptance requires the source proof AND every indexed step proof.
use crate::{
    crypto,
    edwards_gadget::{fixed_mul, Fq, Point, PointVar, FV},
    link::LinkCircuit,
};
use ark_bn254::Fr;
use ark_ff::{BigInteger, Field, PrimeField};
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    convert::ToBitsGadget,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use curve25519_dalek::ristretto::RistrettoPoint;
use num_bigint::BigUint;
use outbe_p_link_measurements::{
    integer::{bits, less_constant},
    poseidon2::{hash, native_hash},
};
pub const STEPS: usize = 12;
const WITNESS_DOMAIN: u64 = 0x525357495401;
const STATE_DOMAIN: u64 = 0x525353544101;

pub fn compressed_limbs(p: RistrettoPoint) -> Vec<Fr> {
    p.compress()
        .as_bytes()
        .chunks(16)
        .map(Fr::from_le_bytes_mod_order)
        .collect()
}
fn limbs(v: &BigUint, n: usize) -> Vec<Fr> {
    let mask = (BigUint::from(1u8) << 64usize) - 1u8;
    (0..n)
        .map(|i| Fr::from_le_bytes_mod_order(&((v >> (64 * i)) & &mask).to_bytes_le()))
        .collect()
}
pub fn context(c: &LinkCircuit) -> Fr {
    let v = c.public.inputs().unwrap();
    v[v.len() - 2]
}
pub fn witness_digest(c: &LinkCircuit) -> Fr {
    let mut v = vec![Fr::from(WITNESS_DOMAIN), context(c)];
    v.extend(limbs(&c.private.nominal, 2));
    v.extend(limbs(&crypto::scalar_integer(c.private.blinder), 4));
    v.push(c.private.salt);
    native_hash(&v)
}
fn bit_limbs(bs: &[Boolean<Fr>]) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    bs.chunks(64).map(Boolean::le_bits_to_fp).collect()
}
pub fn bind_witness(
    cs: ConstraintSystemRef<Fr>,
    a: &[Boolean<Fr>],
    r: &BigUint,
    salt: Fr,
    ctx: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    if a.len() != 104 {
        return Err(SynthesisError::Unsatisfiable);
    }
    let rb = bits(cs.clone(), r, 253)?;
    less_constant(&rb, &crypto::scalar_modulus())?.enforce_equal(&Boolean::TRUE)?;
    let mut v = vec![FpVar::constant(Fr::from(WITNESS_DOMAIN)), ctx.clone()];
    v.extend(bit_limbs(a)?);
    v.extend(bit_limbs(&rb)?);
    v.push(FpVar::new_witness(cs, || Ok(salt))?);
    hash(&v)
}
fn point_limbs(p: Point) -> Vec<Fr> {
    let mut v = limbs(&BigUint::from_bytes_le(&p.x.into_bigint().to_bytes_le()), 4);
    v.extend(limbs(
        &BigUint::from_bytes_le(&p.y.into_bigint().to_bytes_le()),
        4,
    ));
    v
}
fn state_digest(ctx: Fr, d: Fr, index: usize, p: Point, salt: Fr) -> Fr {
    let mut v = vec![Fr::from(STATE_DOMAIN), ctx, d, Fr::from(index as u64)];
    v.extend(point_limbs(p));
    v.push(salt);
    native_hash(&v)
}
fn schedule(index: usize) -> (bool, usize, usize) {
    assert!(index < STEPS);
    if index < 4 {
        (false, index * 32, 32.min(104 - index * 32))
    } else {
        let i = index - 4;
        (true, i * 32, 32.min(253 - i * 32))
    }
}
fn base(index: usize) -> Point {
    let (is_r, offset, _) = schedule(index);
    let pc = bulletproofs::PedersenGens::default();
    let mut p = Point::decode(
        if is_r { pc.B_blinding } else { pc.B }
            .compress()
            .to_bytes(),
    )
    .unwrap();
    for _ in 0..offset {
        p = p.add(p);
    }
    p
}
fn mul_native(mut p: Point, n: &BigUint, len: usize) -> Point {
    let mut out = Point::zero();
    for i in 0..len {
        if n.bit(i as u64) {
            out = out.add(p)
        }
        p = p.add(p);
    }
    out
}
pub fn states(c: &LinkCircuit) -> Vec<Point> {
    let mut v = vec![Point::zero()];
    let r = crypto::scalar_integer(c.private.blinder);
    for i in 0..STEPS {
        let (is_r, offset, len) = schedule(i);
        let n = if is_r { &r } else { &c.private.nominal };
        v.push(v[i].add(mul_native(base(i), &(n >> offset), len)));
    }
    v
}
pub fn digests(c: &LinkCircuit) -> Vec<Fr> {
    states(c)
        .into_iter()
        .enumerate()
        .map(|(i, p)| state_digest(context(c), c.public.opening_binding, i, p, c.private.salt))
        .collect()
}
#[derive(Clone)]
pub struct Step {
    pub source: LinkCircuit,
    pub index: usize,
    pub before: Point,
    pub digests: Vec<Fr>,
}
impl Step {
    pub fn new(source: LinkCircuit, index: usize) -> Self {
        let before = states(&source)[index];
        let digests = digests(&source);
        Self {
            source,
            index,
            before,
            digests,
        }
    }
    pub fn inputs(&self) -> Vec<Fr> {
        step_inputs(&self.source, self.index, &self.digests)
    }
}
pub fn step_inputs(c: &LinkCircuit, index: usize, digests: &[Fr]) -> Vec<Fr> {
    let mut v = vec![
        context(c),
        c.public.opening_binding,
        digests[index],
        digests[index + 1],
    ];
    if index == STEPS - 1 {
        v.extend(point_limbs(
            Point::decode(c.public.commitment.compress().to_bytes()).unwrap(),
        ));
    }
    v
}
fn point_vars(
    cs: ConstraintSystemRef<Fr>,
    p: Point,
    public: bool,
) -> Result<PointVar, SynthesisError> {
    let q = PointVar {
        x: FV::new_witness(cs.clone(), || Ok(p.x))?,
        y: FV::new_witness(cs.clone(), || Ok(p.y))?,
    };
    if public {
        for (fv, native) in [&q.x, &q.y].into_iter().zip([p.x, p.y]) {
            let bs = fv.to_bits_le()?;
            let ns = limbs(
                &BigUint::from_bytes_le(&native.into_bigint().to_bytes_le()),
                4,
            );
            for (b, n) in bit_limbs(&bs)?.iter().zip(ns) {
                b.enforce_equal(&FpVar::new_input(cs.clone(), || Ok(n))?)?;
            }
        }
    }
    Ok(q)
}
fn hash_state(
    ctx: &FpVar<Fr>,
    d: &FpVar<Fr>,
    index: usize,
    p: &PointVar,
    salt: &FpVar<Fr>,
) -> Result<FpVar<Fr>, SynthesisError> {
    let mut v = vec![
        FpVar::constant(Fr::from(STATE_DOMAIN)),
        ctx.clone(),
        d.clone(),
        FpVar::constant(Fr::from(index as u64)),
    ];
    v.extend(bit_limbs(&p.x.to_bits_le()?)?);
    v.extend(bit_limbs(&p.y.to_bits_le()?)?);
    v.push(salt.clone());
    hash(&v)
}
impl ConstraintSynthesizer<Fr> for Step {
    fn generate_constraints(self, cs: ConstraintSystemRef<Fr>) -> Result<(), SynthesisError> {
        if self.index >= STEPS || self.digests.len() != STEPS + 1 {
            return Err(SynthesisError::Unsatisfiable);
        }
        let ctx = FpVar::new_input(cs.clone(), || Ok(context(&self.source)))?;
        let d = FpVar::new_input(cs.clone(), || Ok(self.source.public.opening_binding))?;
        let prev = FpVar::new_input(cs.clone(), || Ok(self.digests[self.index]))?;
        let next = FpVar::new_input(cs.clone(), || Ok(self.digests[self.index + 1]))?;
        let a = bits(cs.clone(), &self.source.private.nominal, 104)?;
        let r = crypto::scalar_integer(self.source.private.blinder);
        let rb = bits(cs.clone(), &r, 253)?;
        less_constant(&rb, &crypto::scalar_modulus())?.enforce_equal(&Boolean::TRUE)?;
        let salt = FpVar::new_witness(cs.clone(), || Ok(self.source.private.salt))?;
        let mut binding = vec![FpVar::constant(Fr::from(WITNESS_DOMAIN)), ctx.clone()];
        binding.extend(bit_limbs(&a)?);
        binding.extend(bit_limbs(&rb)?);
        binding.push(salt.clone());
        hash(&binding)?.enforce_equal(&d)?;
        let before = if self.index == 0 {
            PointVar::constant(Point::zero())
        } else {
            point_vars(cs.clone(), self.before, false)?
        };
        // All non-initial states are bound to the preceding proof. Also validate the
        // curve locally, making the complete-addition precondition explicit.
        let xx = &before.x * &before.x;
        let yy = &before.y * &before.y;
        (yy.clone() - xx.clone())
            .enforce_equal(&(xx * yy * crate::edwards_gadget::d() + Fq::ONE))?;
        hash_state(&ctx, &d, self.index, &before, &salt)?.enforce_equal(&prev)?;
        let (is_r, offset, len) = schedule(self.index);
        let bs = if is_r { &rb } else { &a };
        let after = before.add(&fixed_mul(base(self.index), &bs[offset..offset + len])?)?;
        hash_state(&ctx, &d, self.index + 1, &after, &salt)?.enforce_equal(&next)?;
        if self.index == STEPS - 1 {
            let target = point_vars(
                cs,
                Point::decode(self.source.public.commitment.compress().to_bytes())
                    .map_err(|_| SynthesisError::Unsatisfiable)?,
                true,
            )?;
            after.enforce_same(&target)?;
        }
        Ok(())
    }
}
