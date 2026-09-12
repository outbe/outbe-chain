//! R1CS version of pinned outbe-poseidon a6066ce Poseidon2 sponge.
use crate::poseidon2_constants::BN254_CONFIG as CFG;
use ark_bn254::Fr;
use ark_r1cs_std::{fields::fp::FpVar, fields::FieldVar};
use ark_relations::r1cs::SynthesisError;

fn external(s: &mut [FpVar<Fr>; 4]) {
    let t0 = &s[0] + &s[1];
    let t1 = &s[2] + &s[3];
    let t2 = &s[1] * Fr::from(2u64) + &t1;
    let t3 = &s[3] * Fr::from(2u64) + &t0;
    let t4 = &t1 * Fr::from(4u64) + &t3;
    let t5 = &t0 * Fr::from(4u64) + &t2;
    *s = [&t3 + &t5, t5, &t2 + &t4, t4];
}
fn pow5(x: &FpVar<Fr>) -> Result<FpVar<Fr>, SynthesisError> {
    Ok(x.square()?.square()? * x)
}
pub fn permutation(mut s: [FpVar<Fr>; 4]) -> Result<[FpVar<Fr>; 4], SynthesisError> {
    external(&mut s);
    let first = CFG.rounds_f as usize / 2;
    let partial_end = first + CFG.rounds_p as usize;
    for r in 0..(CFG.rounds_f + CFG.rounds_p) as usize {
        if !(first..partial_end).contains(&r) {
            for (i, v) in s.iter_mut().enumerate() {
                *v = pow5(&(&*v + CFG.round_constant[r][i]))?;
            }
            external(&mut s);
        } else {
            s[0] = pow5(&(&s[0] + CFG.round_constant[r][0]))?;
            let sum = s.iter().fold(FpVar::zero(), |acc, x| acc + x);
            for (i, v) in s.iter_mut().enumerate() {
                *v = &*v * CFG.internal_matrix_diagonal[i] + &sum;
            }
        }
    }
    Ok(s)
}
pub fn hash(input: &[FpVar<Fr>]) -> Result<FpVar<Fr>, SynthesisError> {
    let mut s = [
        FpVar::zero(),
        FpVar::zero(),
        FpVar::zero(),
        FpVar::constant(Fr::from(input.len() as u64) * Fr::from(1u128 << 64)),
    ];
    let mut blocks = input.chunks_exact(3);
    for block in &mut blocks {
        for i in 0..3 {
            s[i] += &block[i];
        }
        s = permutation(s)?;
    }
    for (i, x) in blocks.remainder().iter().enumerate() {
        s[i] += x;
    }
    Ok(permutation(s)?[0].clone())
}
pub fn native_hash(input: &[Fr]) -> Fr {
    use ark_r1cs_std::R1CSVar;
    hash(
        &input
            .iter()
            .copied()
            .map(FpVar::constant)
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .value()
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::MontFp;
    use ark_r1cs_std::{alloc::AllocVar, eq::EqGadget};
    use ark_relations::r1cs::ConstraintSystem;
    #[test]
    fn noir_known_answers_constrained() {
        for (n, expected) in [
            (
                2,
                MontFp!("0x038682aa1cb5ae4e0a3f13da432a95c77c5c111f6f030faf9cad641ce1ed7383"),
            ),
            (
                3,
                MontFp!("0x16f5da1a6b40e7d71bcdf29687e7908cdf74da44c09058fe36a0a99e269c6972"),
            ),
        ] {
            let cs = ConstraintSystem::new_ref();
            let vars = (1..=n)
                .map(|i| FpVar::new_witness(cs.clone(), || Ok(Fr::from(i as u64))).unwrap())
                .collect::<Vec<_>>();
            hash(&vars)
                .unwrap()
                .enforce_equal(&FpVar::constant(expected))
                .unwrap();
            assert!(cs.is_satisfied().unwrap());
        }
    }
}
