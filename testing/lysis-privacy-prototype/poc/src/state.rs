//! Owner-proved integer transitions. Four independent 64-bit Pedersen openings
//! represent uint256; a balance is never reduced modulo a scalar field.
use crate::{
    crypto::*,
    vss::{point, point_hex},
};
use ark_bn254::Fr;
use ark_ed_on_bn254::{constraints::EdwardsVar, Fr as Scalar};
use ark_ff::{PrimeField, UniformRand};
use ark_r1cs_std::{
    alloc::{AllocVar, AllocationMode},
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
    groups::CurveVar,
};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystemRef, SynthesisError};
use num_bigint::BigUint;
use num_traits::One;
use outbe_p_link_measurements::{
    integer::{bits, less_constant, UInt},
    poseidon2::{hash, native_hash},
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub value: String,
    pub blinds: Vec<String>,
}
impl Note {
    pub fn fresh(v: BigUint) -> Result<Self> {
        if v.bits() > 256 {
            return Err("uint256 note overflow".into());
        }
        Ok(Self {
            value: v.to_string(),
            blinds: (0..4)
                .map(|_| scalar_integer(Scalar::rand(&mut OsRng)).to_string())
                .collect(),
        })
    }
    pub fn zero() -> Self {
        Self {
            value: "0".into(),
            blinds: vec!["0".into(); 4],
        }
    }
    pub fn commitments(&self) -> Result<Vec<String>> {
        if self.blinds.len() != 4 || integer(&self.value)?.bits() > 256 {
            return Err("invalid note width".into());
        }
        let n = integer(&self.value)?;
        let mask = (BigUint::one() << 64usize) - 1u32;
        (0..4)
            .map(|i| {
                point_hex(commit(
                    &((&n >> (64 * i)) & &mask),
                    scalar(&integer(&self.blinds[i])?)?,
                )?)
            })
            .collect()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Public {
    pub kind: String,
    pub context: String,
    pub notes: Vec<Vec<String>>,
    pub source: String,
    pub fraction: String,
    pub price: String,
    pub amount: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub public: Public,
    pub notes: Vec<Note>,
    pub nominal: String,
    pub blinder: String,
}
fn count(kind: &str) -> Result<usize> {
    match kind {
        "claim" => Ok(5),
        "move" => Ok(4),
        "withdraw" => Ok(2),
        "mint" | "pledge" => Ok(3),
        _ => Err("unknown state relation".into()),
    }
}
impl Public {
    pub fn inputs(&self) -> Result<Vec<Fr>> {
        if self.notes.len() != count(&self.kind)?
            || integer(&self.fraction)?.bits() > 256
            || integer(&self.price)?.bits() > 256
            || integer(&self.amount)?.bits() > 256
        {
            return Err("invalid public state shape/range".into());
        }
        let mut v = vec![field_from_hex(&self.context)?];
        for s in [&self.fraction, &self.price, &self.amount] {
            let n = integer(s)?;
            let mask = (BigUint::one() << 64usize) - 1u32;
            for i in 0usize..4 {
                v.push(Fr::from_le_bytes_mod_order(
                    &((&n >> (64 * i)) & &mask).to_bytes_le(),
                ));
            }
        }
        let p = point(&self.source)?;
        v.extend([p.x, p.y]);
        for note in &self.notes {
            if note.len() != 4 {
                return Err("note commitment count".into());
            }
            for s in note {
                let p = point(s)?;
                v.extend([p.x, p.y]);
            }
        }
        let mut domain = vec![Fr::from(match self.kind.as_str() {
            "claim" => 4101,
            "move" => 4102,
            "withdraw" => 4103,
            "pledge" => 4105,
            _ => 4104,
        } as u64)];
        domain.extend(&v);
        v.push(native_hash(&domain));
        Ok(v)
    }
}
fn opening(
    cs: ConstraintSystemRef<Fr>,
    n: &UInt,
    blind: &str,
    p: &[FpVar<Fr>],
) -> std::result::Result<(), SynthesisError> {
    let r = integer(blind).map_err(|_| SynthesisError::Unsatisfiable)?;
    if r >= scalar_modulus() {
        return Err(SynthesisError::Unsatisfiable);
    }
    let rb = bits(cs.clone(), &r, 251)?;
    less_constant(&rb, &scalar_modulus())?.enforce_equal(&Boolean::TRUE)?;
    let (g, h) = generators();
    let gv = EdwardsVar::new_constant(cs.clone(), g)?;
    let hv = EdwardsVar::new_constant(cs, h)?;
    let c = gv.scalar_mul_le(n.bits.iter())? + hv.scalar_mul_le(rb.iter())?;
    c.x.enforce_equal(&p[0])?;
    c.y.enforce_equal(&p[1])
}
impl ConstraintSynthesizer<Fr> for Transition {
    fn generate_constraints(
        self,
        cs: ConstraintSystemRef<Fr>,
    ) -> std::result::Result<(), SynthesisError> {
        let bad = || SynthesisError::Unsatisfiable;
        let values = self.public.inputs().map_err(|_| bad())?;
        let vs = values
            .iter()
            .map(|f| FpVar::new_input(cs.clone(), || Ok(*f)))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut domain = vec![FpVar::constant(Fr::from(match self.public.kind.as_str() {
            "claim" => 4101,
            "move" => 4102,
            "withdraw" => 4103,
            "pledge" => 4105,
            _ => 4104,
        } as u64))];
        domain.extend(vs[..vs.len() - 1].iter().cloned());
        hash(&domain)?.enforce_equal(vs.last().unwrap())?;
        if self.notes.len() != count(&self.public.kind).map_err(|_| bad())? {
            return Err(bad());
        }
        let mut ns = Vec::new();
        for (j, note) in self.notes.iter().enumerate() {
            if note.blinds.len() != 4 {
                return Err(bad());
            }
            let value = integer(&note.value).map_err(|_| bad())?;
            if value.bits() > 256 {
                return Err(bad());
            }
            let n = UInt::alloc(cs.clone(), value, 256, AllocationMode::Witness)?;
            for i in 0..4 {
                let limb = UInt::from_bounded_fp(
                    n.limbs[i].clone(),
                    n.bits[i * 64..i * 64 + 64].to_vec(),
                    (&n.value >> (i * 64)) & ((BigUint::one() << 64usize) - 1u32),
                );
                opening(
                    cs.clone(),
                    &limb,
                    &note.blinds[i],
                    &vs[15 + j * 8 + i * 2..15 + j * 8 + i * 2 + 2],
                )?;
            }
            ns.push(n);
        }
        let f = UInt::alloc(
            cs.clone(),
            integer(&self.public.fraction).map_err(|_| bad())?,
            256,
            AllocationMode::Witness,
        )?;
        let p = UInt::alloc(
            cs.clone(),
            integer(&self.public.price).map_err(|_| bad())?,
            256,
            AllocationMode::Witness,
        )?;
        let amount = UInt::alloc(
            cs.clone(),
            integer(&self.public.amount).map_err(|_| bad())?,
            256,
            AllocationMode::Witness,
        )?;
        for i in 0..4 {
            f.limbs[i].enforce_equal(&vs[1 + i])?;
            p.limbs[i].enforce_equal(&vs[5 + i])?;
            amount.limbs[i].enforce_equal(&vs[9 + i])?;
        }
        match self.public.kind.as_str() {
            "claim" => {
                let nominal = integer(&self.nominal).map_err(|_| bad())?;
                if nominal.bits() > 104 {
                    return Err(bad());
                }
                let a = UInt::alloc(cs.clone(), nominal, 104, AllocationMode::Witness)?;
                a.enforce_nonzero()?;
                f.enforce_nonzero()?;
                p.enforce_nonzero()?;
                opening(cs.clone(), &a, &self.blinder, &vs[13..15])?;
                let af = a.mul(&f, cs.clone(), 256)?;
                let g = af.mul(&UInt::constant(1_000_000), cs.clone(), 256)?;
                let c = af.mul(&p, cs.clone(), 256)?;
                // old Gratis, old payment asset -> new Gratis, payment change,
                // private backed escrow note. Each source is consumed atomically.
                ns[0].add(&g, cs.clone(), 256)?.enforce_equal(&ns[2])?;
                ns[3].add(&c, cs.clone(), 256)?.enforce_equal(&ns[1])?;
                c.enforce_equal(&ns[4])?;
            }
            "move" => {
                ns[0]
                    .add(&ns[1], cs.clone(), 257)?
                    .enforce_equal(&ns[2].add(&ns[3], cs.clone(), 257)?)?;
            }
            "withdraw" => {
                ns[1].add(&amount, cs.clone(), 256)?.enforce_equal(&ns[0])?;
                amount.enforce_nonzero()?;
            }
            "pledge" => {
                ns[1].add(&amount, cs.clone(), 256)?.enforce_equal(&ns[0])?;
                ns[2].enforce_equal(&amount)?;
                amount.enforce_nonzero()?;
            }
            "mint" => {
                ns[0]
                    .add(
                        &ns[1].mul(&UInt::constant(1_000_000_000_000), cs.clone(), 256)?,
                        cs.clone(),
                        256,
                    )?
                    .enforce_equal(&ns[2])?;
                ns[1].enforce_nonzero()?;
            }
            _ => return Err(bad()),
        }
        Ok(())
    }
}
pub fn fixture(kind: &str) -> Result<Transition> {
    let a = BigUint::from(123456u64);
    let f = 500000u64;
    let price = BigUint::from(1_000_000u64);
    let g = &a * f * 1_000_000u64;
    let c = &a * f * &price;
    let r = Scalar::rand(&mut OsRng);
    let vals = match kind {
        "claim" => vec![1u32.into(), &c + 7u32, &g + 1u32, 7u32.into(), c],
        "move" => vec![7u32.into(), 0u32.into(), 4u32.into(), 3u32.into()],
        "withdraw" => vec![7u32.into(), 4u32.into()],
        "pledge" => vec![7u32.into(), 4u32.into(), 3u32.into()],
        "mint" => vec![
            7u32.into(),
            3u32.into(),
            BigUint::from(3_000_000_000_007u64),
        ],
        _ => return Err("unknown relation".into()),
    };
    let notes = vals
        .into_iter()
        .map(Note::fresh)
        .collect::<Result<Vec<_>>>()?;
    Ok(Transition {
        public: Public {
            kind: kind.into(),
            context: field_hex(Fr::from(1001u64)),
            notes: notes.iter().map(Note::commitments).collect::<Result<_>>()?,
            source: point_hex(commit(&a, r)?)?,
            fraction: f.to_string(),
            price: price.to_string(),
            amount: "3".into(),
        },
        notes,
        nominal: a.to_string(),
        blinder: scalar_integer(r).to_string(),
    })
}
