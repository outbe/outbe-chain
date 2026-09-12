//! Bounded integer limbs. Every limb/carry equation is below the BN254 modulus.
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use ark_r1cs_std::{
    alloc::{AllocVar, AllocationMode},
    boolean::Boolean,
    eq::EqGadget,
    fields::{fp::FpVar, FieldVar},
};
use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};
use num_bigint::BigUint;
use num_traits::{One, Zero};

pub fn bits(
    cs: ConstraintSystemRef<Fr>,
    value: &BigUint,
    width: usize,
) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    (0..width)
        .map(|i| Boolean::new_witness(cs.clone(), || Ok(value.bit(i as u64))))
        .collect()
}
pub fn less(a: &[Boolean<Fr>], b: &[Boolean<Fr>]) -> Result<Boolean<Fr>, SynthesisError> {
    assert_eq!(a.len(), b.len());
    let mut result = Boolean::FALSE;
    // At each more-significant unequal position, that position replaces the lower result.
    for (a, b) in a.iter().zip(b) {
        result = (a ^ b).select(&((!a) & b), &result)?;
    }
    Ok(result)
}
pub fn less_constant(a: &[Boolean<Fr>], b: &BigUint) -> Result<Boolean<Fr>, SynthesisError> {
    less(
        a,
        &(0..a.len())
            .map(|i| Boolean::constant(b.bit(i as u64)))
            .collect::<Vec<_>>(),
    )
}
pub fn fr_integer(x: Fr) -> BigUint {
    BigUint::from_bytes_le(&x.into_bigint().to_bytes_le())
}
pub fn bounded_fp(
    cs: ConstraintSystemRef<Fr>,
    value: &BigUint,
    width: usize,
    mode: AllocationMode,
) -> Result<(FpVar<Fr>, Vec<Boolean<Fr>>), SynthesisError> {
    assert!(width < 254);
    let var = FpVar::new_variable(
        cs.clone(),
        || Ok(Fr::from_le_bytes_mod_order(&value.to_bytes_le())),
        mode,
    )?;
    let bits = bits(cs, value, width)?;
    Boolean::le_bits_to_fp(&bits)?.enforce_equal(&var)?;
    Ok((var, bits))
}

#[derive(Clone)]
pub struct UInt {
    pub limbs: Vec<FpVar<Fr>>,
    pub bits: Vec<Boolean<Fr>>,
    pub value: BigUint,
}
impl UInt {
    pub fn alloc(
        cs: ConstraintSystemRef<Fr>,
        value: BigUint,
        width: usize,
        mode: AllocationMode,
    ) -> Result<Self, SynthesisError> {
        assert!(width > 0 && width <= 1024);
        let mut limbs = Vec::new();
        let mut all_bits = Vec::new();
        let mask = (BigUint::one() << 64usize) - BigUint::one();
        for i in 0..width.div_ceil(64) {
            let v = (&value >> (i * 64)) & &mask;
            let (limb, mut bs) = bounded_fp(cs.clone(), &v, (width - i * 64).min(64), mode)?;
            limbs.push(limb);
            all_bits.append(&mut bs);
        }
        Ok(Self {
            limbs,
            bits: all_bits,
            value,
        })
    }
    pub fn constant(value: u64) -> Self {
        Self {
            limbs: vec![FpVar::constant(Fr::from(value))],
            bits: (0..64)
                .map(|i| Boolean::constant(value & (1u64 << i) != 0))
                .collect(),
            value: BigUint::from(value),
        }
    }
    fn limb(&self, i: usize) -> FpVar<Fr> {
        self.limbs.get(i).cloned().unwrap_or_else(FpVar::zero)
    }
    pub fn from_bounded_fp(var: FpVar<Fr>, bs: Vec<Boolean<Fr>>, value: BigUint) -> Self {
        Self {
            limbs: vec![var],
            bits: bs,
            value,
        }
    }
    pub fn enforce_nonzero(&self) -> Result<(), SynthesisError> {
        let sum = self.limbs.iter().fold(FpVar::zero(), |a, b| a + b);
        sum.is_eq(&FpVar::zero())?.enforce_equal(&Boolean::FALSE)
    }
    pub fn enforce_equal(&self, other: &Self) -> Result<(), SynthesisError> {
        for i in 0..self.limbs.len().max(other.limbs.len()) {
            self.limb(i).enforce_equal(&other.limb(i))?;
        }
        Ok(())
    }
    pub fn padded_bits(&self, width: usize) -> Vec<Boolean<Fr>> {
        assert!(width >= self.bits.len());
        let mut b = self.bits.clone();
        b.resize(width, Boolean::FALSE);
        b
    }
    pub fn mul(
        &self,
        other: &Self,
        cs: ConstraintSystemRef<Fr>,
        width: usize,
    ) -> Result<Self, SynthesisError> {
        let result = Self::alloc(
            cs.clone(),
            &self.value * &other.value,
            width,
            AllocationMode::Witness,
        )?;
        let base = Fr::from(1u128 << 64);
        let mut carry = FpVar::zero();
        let mut carry_value = BigUint::zero();
        let mask = (BigUint::one() << 64usize) - BigUint::one();
        // Wider output limbs are part of the statement too: they must be zero,
        // not merely range-constrained witnesses chosen by the prover.
        let columns = (self.limbs.len() + other.limbs.len()).max(result.limbs.len());
        for k in 0..columns {
            let mut sum = carry;
            let mut val = carry_value;
            for i in 0..self.limbs.len() {
                if k >= i && k - i < other.limbs.len() {
                    sum += &self.limbs[i] * &other.limbs[k - i];
                    val += ((&self.value >> (64 * i)) & &mask)
                        * ((&other.value >> (64 * (k - i))) & &mask);
                }
            }
            carry_value = &val >> 64usize;
            let (next, _) = bounded_fp(cs.clone(), &carry_value, 72, AllocationMode::Witness)?;
            sum.enforce_equal(&(result.limb(k) + &next * base))?;
            carry = next;
        }
        carry.enforce_equal(&FpVar::zero())?;
        Ok(result)
    }
    pub fn add(
        &self,
        other: &Self,
        cs: ConstraintSystemRef<Fr>,
        width: usize,
    ) -> Result<Self, SynthesisError> {
        let result = Self::alloc(
            cs.clone(),
            &self.value + &other.value,
            width,
            AllocationMode::Witness,
        )?;
        let base = Fr::from(1u128 << 64);
        let mask = (BigUint::one() << 64usize) - BigUint::one();
        let mut carry = FpVar::zero();
        let mut cv = BigUint::zero();
        for i in 0..self
            .limbs
            .len()
            .max(other.limbs.len())
            .max(result.limbs.len())
        {
            let sum = self.limb(i) + other.limb(i) + carry;
            cv = (((&self.value >> (64 * i)) & &mask) + ((&other.value >> (64 * i)) & &mask) + cv)
                >> 64usize;
            let (next, _) = bounded_fp(cs.clone(), &cv, 1, AllocationMode::Witness)?;
            sum.enforce_equal(&(result.limb(i) + &next * base))?;
            carry = next;
        }
        carry.enforce_equal(&FpVar::zero())?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;
    #[test]
    fn integer_products_and_overflow() {
        for (a, b, width, valid) in [
            (u64::MAX, u64::MAX, 128, true),
            (u64::MAX, 2, 64, false),
            (0, 13, 64, true),
        ] {
            let cs = ConstraintSystem::new_ref();
            let x = UInt::alloc(cs.clone(), a.into(), 64, AllocationMode::Witness).unwrap();
            let y = UInt::alloc(cs.clone(), b.into(), 64, AllocationMode::Witness).unwrap();
            x.mul(&y, cs.clone(), width).unwrap();
            assert_eq!(cs.is_satisfied().unwrap(), valid);
        }
    }
    #[test]
    fn integer_order() {
        for (a, b, want) in [
            (0, 1, true),
            (3, 3, false),
            (255, 0, false),
            (127, 128, true),
        ] {
            let cs = ConstraintSystem::new_ref();
            let a = bits(cs.clone(), &BigUint::from(a as u32), 8).unwrap();
            let b = bits(cs.clone(), &BigUint::from(b as u32), 8).unwrap();
            less(&a, &b)
                .unwrap()
                .enforce_equal(&Boolean::constant(want))
                .unwrap();
            assert!(cs.is_satisfied().unwrap());
        }
    }
}
