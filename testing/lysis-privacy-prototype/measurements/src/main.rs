// Research-only arithmetic/VSS measurements. Not a production protocol.
use bulletproofs::r1cs::{
    ConstraintSystem, LinearCombination as LC, Prover, R1CSError, R1CSProof, Variable, Verifier,
};
use bulletproofs::{BulletproofGens, PedersenGens, RangeProof};
use curve25519_dalek::{
    ristretto::{CompressedRistretto, RistrettoPoint},
    scalar::Scalar,
    traits::VartimeMultiscalarMul,
};
use merlin::Transcript;
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{One, Zero};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde_json::{json, Value};
use std::time::Instant;

fn scalar(v: &BigUint) -> Scalar {
    let b = v.to_bytes_le();
    assert!(b.len() <= 32);
    let mut out = [0u8; 32];
    out[..b.len()].copy_from_slice(&b);
    Scalar::from_bytes_mod_order(out)
}
fn bi(v: &BigUint) -> BigInt {
    BigInt::from_biguint(Sign::Plus, v.clone())
}
fn words(v: &BigUint, n: usize) -> Vec<u64> {
    let mut out = v.to_u64_digits();
    assert!(out.len() <= n);
    out.resize(n, 0);
    out
}
fn range<CS: ConstraintSystem>(
    cs: &mut CS,
    mut x: LC,
    value: Option<&BigUint>,
    bits: usize,
) -> Result<(), R1CSError> {
    let mut weight = Scalar::ONE;
    for i in 0..bits {
        let input = value.map(|v| {
            let b = Scalar::from(v.bit(i as u64) as u64);
            (b, Scalar::ONE - b)
        });
        let (b, not_b, product) = cs.allocate_multiplier(input)?;
        cs.constrain(product.into());
        cs.constrain(b + not_b - Scalar::ONE);
        x = x - b * weight;
        weight += weight;
    }
    cs.constrain(x);
    Ok(())
}
fn allocate_words<CS: ConstraintSystem>(
    cs: &mut CS,
    value: Option<&BigUint>,
    n: usize,
) -> Result<Vec<Variable>, R1CSError> {
    let w = value.map(|v| words(v, n));
    (0..n)
        .map(|i| {
            let v = cs.allocate(w.as_ref().map(|a| Scalar::from(a[i])))?;
            let a = w.as_ref().map(|a| BigUint::from(a[i]));
            range(cs, v.into(), a.as_ref(), 64)?;
            Ok(v)
        })
        .collect()
}
// Prove sum(sign * private_words * public_coefficient) = 0 over INTEGERS.
// Each limb equality is bounded far below the scalar field order. Carries
// use a signed offset and 71-bit range; no modulo-q uint256 alias is allowed.
fn relation<CS: ConstraintSystem>(
    cs: &mut CS,
    terms: &[(Vec<Variable>, Option<BigUint>, BigInt)],
) -> Result<(), R1CSError> {
    let coeffs: Vec<Vec<u64>> = terms
        .iter()
        .map(|t| t.2.magnitude().to_u64_digits())
        .collect();
    let w: Vec<Option<Vec<u64>>> = terms
        .iter()
        .map(|t| t.1.as_ref().map(|v| words(v, t.0.len())))
        .collect();
    let n = terms
        .iter()
        .zip(&coeffs)
        .map(|(t, c)| t.0.len() + c.len())
        .max()
        .unwrap();
    let base = BigUint::one() << 64;
    let base_s = scalar(&base);
    let offset = BigInt::one() << 70usize;
    let offset_s = scalar(&offset.to_biguint().unwrap());
    let known = terms.iter().all(|t| t.1.is_some());
    let mut carry = BigInt::zero();
    let mut carry_lc = LC::default();
    for k in 0..n {
        let mut lc = carry_lc.clone();
        let mut numer = carry.clone();
        for (i, t) in terms.iter().enumerate() {
            let negative = t.2.sign() == Sign::Minus;
            for j in 0..t.0.len() {
                if k >= j && k - j < coeffs[i].len() {
                    let c = coeffs[i][k - j];
                    let s = if negative {
                        -Scalar::from(c)
                    } else {
                        Scalar::from(c)
                    };
                    lc = lc + t.0[j] * s;
                    if known {
                        let a = BigInt::from(w[i].as_ref().unwrap()[j]) * BigInt::from(c);
                        numer += if negative { -a } else { a };
                    }
                }
            }
        }
        let next = if known {
            numer / bi(&base)
        } else {
            BigInt::zero()
        };
        let next_lc = if k + 1 == n {
            LC::default()
        } else {
            let shifted = if known {
                Some((&next + &offset).to_biguint().expect("carry lower bound"))
            } else {
                None
            };
            let var = cs.allocate(shifted.as_ref().map(scalar))?;
            range(cs, var.into(), shifted.as_ref(), 71)?;
            LC::from(var) - offset_s
        };
        cs.constrain(lc - next_lc.clone() * base_s);
        carry = next;
        carry_lc = next_lc;
    }
    Ok(())
}
fn floor_relation<CS: ConstraintSystem>(
    cs: &mut CS,
    a: &[Variable],
    av: Option<&BigUint>,
    b: &[Variable],
    bv: Option<&BigUint>,
    p: &BigUint,
    d: &BigUint,
) -> Result<(), R1CSError> {
    assert!(!d.is_zero());
    let remainder = av.map(|x| (x * p) % d);
    let complement = remainder.as_ref().map(|r| d - BigUint::one() - r);
    let n = ((d.bits() + 63) / 64).max(1) as usize;
    let r = allocate_words(cs, remainder.as_ref(), n)?;
    let c = allocate_words(cs, complement.as_ref(), n)?;
    // A constant word vector is constrained explicitly, so its value is not a free witness.
    let dm = d - BigUint::one();
    let dv = allocate_words(cs, Some(&dm), n)?;
    for (v, w) in dv.iter().zip(words(&dm, n)) {
        cs.constrain(*v - Scalar::from(w));
    }
    relation(
        cs,
        &[
            (r.clone(), remainder.clone(), BigInt::one()),
            (c, complement, BigInt::one()),
            (dv, Some(dm), -BigInt::one()),
        ],
    )?;
    relation(
        cs,
        &[
            (a.to_vec(), av.cloned(), bi(p)),
            (b.to_vec(), bv.cloned(), -bi(d)),
            (r, remainder, -BigInt::one()),
        ],
    )
}
fn nonzero<CS: ConstraintSystem>(
    cs: &mut CS,
    a: &[Variable],
    av: Option<&BigUint>,
) -> Result<(), R1CSError> {
    let sum: LC = a.iter().fold(LC::default(), |s, v| s + *v);
    let sumv = av.map(|v| {
        words(v, a.len())
            .iter()
            .fold(Scalar::ZERO, |s, w| s + Scalar::from(*w))
    });
    let inv = cs.allocate(sumv.map(|s| s.invert()))?;
    let (_, _, one) = cs.multiply(sum, inv.into());
    cs.constrain(one - Scalar::ONE);
    Ok(())
}
#[derive(Clone)]
enum Op {
    Issue { p: BigUint, d: BigUint },
    Claim { f: BigUint, p: BigUint },
    Withdraw { x: BigUint },
}
impl Op {
    fn label(&self) -> &'static str {
        match self {
            Self::Issue { .. } => "issue_nominal_uint256",
            Self::Claim { .. } => "claim_nod_into_private_gratis_uint256",
            Self::Withdraw { .. } => "withdraw_private_gratis_to_public_coen_uint256",
        }
    }
    fn domain(&self, nonce: u64) -> Transcript {
        let mut t = Transcript::new(b"OUTBE-RESEARCH-ONLY-v1");
        t.append_message(b"operation", self.label().as_bytes());
        t.append_message(b"context", b"chain=research;day=1;nod=fixture;policy=v1");
        t.append_u64(b"owner_fixture", nonce >> 32);
        t.append_u64(b"account_nonce", nonce & 0xffff_ffff);
        match self {
            Self::Issue { p, d } => {
                t.append_message(b"numerator", &p.to_bytes_le());
                t.append_message(b"denominator", &d.to_bytes_le());
            }
            Self::Claim { f, p } => {
                t.append_message(b"fraction", &f.to_bytes_le());
                t.append_message(b"price", &p.to_bytes_le());
            }
            Self::Withdraw { x } => t.append_message(b"public_withdrawal", &x.to_bytes_le()),
        }
        t
    }
}
fn circuit<CS: ConstraintSystem>(
    cs: &mut CS,
    op: &Op,
    vars: &[Vec<Variable>],
    values: Option<&[BigUint]>,
) -> Result<(), R1CSError> {
    let val = |i: usize| values.map(|v| &v[i]);
    for (i, row) in vars.iter().enumerate() {
        for (j, x) in row.iter().enumerate() {
            let word = val(i).map(|v| BigUint::from(words(v, 4)[j]));
            range(cs, (*x).into(), word.as_ref(), 64)?;
        }
    }
    let m = BigUint::from(1_000_000u64);
    match op {
        Op::Issue { p, d } => {
            floor_relation(cs, &vars[0], val(0), &vars[1], val(1), p, d)?;
            nonzero(cs, &vars[0], val(0))?;
            nonzero(cs, &vars[1], val(1))?;
        }
        Op::Claim { f, p } => {
            floor_relation(cs, &vars[0], val(0), &vars[1], val(1), f, &m)?;
            floor_relation(cs, &vars[1], val(1), &vars[2], val(2), p, &m)?;
            nonzero(cs, &vars[1], val(1))?;
            nonzero(cs, &vars[2], val(2))?;
            relation(
                cs,
                &[
                    (vars[3].clone(), val(3).cloned(), BigInt::one()),
                    (vars[1].clone(), val(1).cloned(), BigInt::one()),
                    (vars[4].clone(), val(4).cloned(), -BigInt::one()),
                ],
            )?;
        }
        Op::Withdraw { x } => {
            // Public native mint has to fit uint256 too.
            assert!((x * BigUint::from(1_000_000_000_000u64)).bits() <= 256);
            let xv = allocate_words(cs, Some(x), 4)?;
            for (v, w) in xv.iter().zip(words(x, 4)) {
                cs.constrain(*v - Scalar::from(w));
            }
            relation(
                cs,
                &[
                    (vars[0].clone(), val(0).cloned(), BigInt::one()),
                    (vars[1].clone(), val(1).cloned(), -BigInt::one()),
                    (xv, Some(x.clone()), -BigInt::one()),
                ],
            )?;
        }
    }
    Ok(())
}
struct ProofData {
    proof: R1CSProof,
    commitments: Vec<Vec<CompressedRistretto>>,
    blinds: Vec<Vec<Scalar>>,
    prove_ms: f64,
    multipliers: usize,
}
fn prove(
    pc: &PedersenGens,
    bp: &BulletproofGens,
    op: &Op,
    values: &[BigUint],
    nonce: u64,
    rng: &mut ChaCha20Rng,
    first_blinds: Option<&[Scalar]>,
) -> ProofData {
    let t0 = Instant::now();
    let mut transcript = op.domain(nonce);
    let mut prover = Prover::new(pc, &mut transcript);
    let mut commitments = vec![];
    let mut vars = vec![];
    let mut blinds = vec![];
    for (i, value) in values.iter().enumerate() {
        let mut cs = vec![];
        let mut vs = vec![];
        let mut rs = vec![];
        for (j, word) in words(value, 4).iter().enumerate() {
            let r = if i == 0 {
                first_blinds
                    .map(|a| a[j])
                    .unwrap_or_else(|| Scalar::random(rng))
            } else {
                Scalar::random(rng)
            };
            let (c, v) = prover.commit(Scalar::from(*word), r);
            cs.push(c);
            vs.push(v);
            rs.push(r);
        }
        commitments.push(cs);
        vars.push(vs);
        blinds.push(rs);
    }
    circuit(&mut prover, op, &vars, Some(values)).unwrap();
    let multipliers = prover.metrics().multipliers;
    let proof = prover.prove(bp).unwrap();
    ProofData {
        proof,
        commitments,
        blinds,
        prove_ms: t0.elapsed().as_secs_f64() * 1000.,
        multipliers,
    }
}
fn verify(pc: &PedersenGens, bp: &BulletproofGens, op: &Op, data: &ProofData, nonce: u64) -> bool {
    let mut transcript = op.domain(nonce);
    let mut verifier = Verifier::new(&mut transcript);
    let vars: Vec<Vec<Variable>> = data
        .commitments
        .iter()
        .map(|r| r.iter().map(|c| verifier.commit(*c)).collect())
        .collect();
    circuit(&mut verifier, op, &vars, None).unwrap();
    verifier.verify(&data.proof, pc, bp).is_ok()
}
fn stat(xs: &[f64]) -> Value {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    json!({"samples":v.len(),"median_ms":(v[(v.len()-1)/2]+v[v.len()/2])/2.,"mean_ms":v.iter().sum::<f64>()/v.len() as f64,"min_ms":v[0],"max_ms":v[v.len()-1]})
}
fn proof_bench(
    pc: &PedersenGens,
    bp: &BulletproofGens,
    op: &Op,
    values: &[BigUint],
    rng: &mut ChaCha20Rng,
) -> Value {
    let mut ps = vec![];
    let mut vs = vec![];
    let mut bytes = 0;
    let mut cs = 0;
    let mut mult = 0;
    for i in 0..5 {
        let data = prove(pc, bp, op, values, 42, rng, None);
        let t = Instant::now();
        assert!(verify(pc, bp, op, &data, 42));
        let elapsed = t.elapsed().as_secs_f64() * 1000.;
        assert!(!verify(pc, bp, op, &data, 43), "wrong nonce accepted");
        if i > 0 {
            ps.push(data.prove_ms);
            vs.push(elapsed);
        }
        bytes = data.proof.to_bytes().len();
        cs = data.commitments.len() * 4 * 32;
        mult = data.multipliers;
    }
    json!({"operation":op.label(),"proof":stat(&ps),"verify":stat(&vs),"proof_bytes":bytes,"all_statement_commitments_bytes":cs,"multipliers":mult,"scope":"arithmetic + public-context binding; no source authorization, PayNote membership, PoW, VSS linkage or chain execution"})
}
#[derive(Clone)]
struct Deal {
    shares: Vec<Vec<Scalar>>,
    blinds: Vec<Vec<Scalar>>,
    coeff: Vec<Vec<RistrettoPoint>>,
}
fn deal(
    pc: &PedersenGens,
    values: &[Scalar],
    blind: &[Scalar],
    n: usize,
    t: usize,
    rng: &mut ChaCha20Rng,
) -> Deal {
    let mut shares = vec![vec![Scalar::ZERO; 4]; n];
    let mut blinds = shares.clone();
    let mut coeff = vec![];
    for l in 0..4 {
        let mut a = vec![values[l]];
        let mut b = vec![blind[l]];
        for _ in 1..t {
            a.push(Scalar::random(rng));
            b.push(Scalar::random(rng));
        }
        coeff.push(a.iter().zip(&b).map(|(a, b)| pc.commit(*a, *b)).collect());
        for j in 0..n {
            let x = Scalar::from((j + 1) as u64);
            shares[j][l] = a.iter().rev().fold(Scalar::ZERO, |v, c| v * x + c);
            blinds[j][l] = b.iter().rev().fold(Scalar::ZERO, |v, c| v * x + c);
        }
    }
    Deal {
        shares,
        blinds,
        coeff,
    }
}
fn check_deal(pc: &PedersenGens, d: &Deal, j: usize) -> bool {
    let x = Scalar::from((j + 1) as u64);
    let mut powers = vec![Scalar::ONE];
    for k in 1..d.coeff[0].len() {
        powers.push(powers[k - 1] * x);
    }
    (0..4).all(|l| {
        pc.commit(d.shares[j][l], d.blinds[j][l])
            == RistrettoPoint::vartime_multiscalar_mul(&powers, &d.coeff[l])
    })
}
fn add_deal(a: &mut Deal, b: &Deal) {
    for j in 0..a.shares.len() {
        for l in 0..4 {
            a.shares[j][l] += b.shares[j][l];
            a.blinds[j][l] += b.blinds[j][l];
        }
    }
    for l in 0..4 {
        for k in 0..a.coeff[l].len() {
            a.coeff[l][k] += b.coeff[l][k];
        }
    }
}
fn lagrange(t: usize) -> Vec<Scalar> {
    // For fixed qualified coordinates 1..t: lambda_i = (-1)^(i-1) binomial(t,i).
    let mut inverses: Vec<Scalar> = (1..=t).map(|i| Scalar::from(i as u64)).collect();
    Scalar::batch_invert(&mut inverses);
    let mut choose = Scalar::ONE;
    (1..=t)
        .map(|i| {
            choose = choose * Scalar::from((t - i + 1) as u64) * inverses[i - 1];
            if i % 2 == 1 {
                choose
            } else {
                -choose
            }
        })
        .collect()
}
fn open(d: &Deal, t: usize) -> (Vec<Scalar>, Vec<Scalar>) {
    let weights = lagrange(t);
    let mut a = vec![Scalar::ZERO; 4];
    let mut b = a.clone();
    for j in 0..t {
        for l in 0..4 {
            a[l] += weights[j] * d.shares[j][l];
            b[l] += weights[j] * d.blinds[j][l];
        }
    }
    (a, b)
}
fn reconstructed_integer(a: &[Scalar]) -> BigUint {
    a.iter().enumerate().fold(BigUint::zero(), |s, (i, a)| {
        s + (BigUint::from_bytes_le(&a.to_bytes()) << (64 * i))
    })
}
fn reshare(
    pc: &PedersenGens,
    old: &Deal,
    old_t: usize,
    n: usize,
    t: usize,
    rng: &mut ChaCha20Rng,
) -> Deal {
    let weights = lagrange(old_t);
    let mut combined = None;
    for j in 0..old_t {
        let a: Vec<_> = old.shares[j].iter().map(|v| v * weights[j]).collect();
        let b: Vec<_> = old.blinds[j].iter().map(|v| v * weights[j]).collect();
        let fresh = deal(pc, &a, &b, n, t, rng);
        let x = Scalar::from((j + 1) as u64);
        let mut powers = vec![Scalar::ONE];
        for k in 1..old.coeff[0].len() {
            powers.push(powers[k - 1] * x);
        }
        for l in 0..4 {
            let expected =
                RistrettoPoint::vartime_multiscalar_mul(&powers, &old.coeff[l]) * weights[j];
            assert_eq!(fresh.coeff[l][0], expected);
        }
        for receiver in 0..n {
            assert!(check_deal(pc, &fresh, receiver));
        }
        if let Some(sum) = combined.as_mut() {
            add_deal(sum, &fresh);
        } else {
            combined = Some(fresh);
        }
    }
    let fresh = combined.unwrap();
    for l in 0..4 {
        assert_eq!(old.coeff[l][0], fresh.coeff[l][0]);
    }
    fresh
}
fn vss_bench(pc: &PedersenGens, n: usize, t: usize, rng: &mut ChaCha20Rng) -> Value {
    let av: Vec<_> = (0..4).map(|_| Scalar::from(rng.next_u64())).collect();
    let bv: Vec<_> = (0..4).map(|_| Scalar::random(rng)).collect();
    let mut generation = vec![];
    let mut verification = vec![];
    let mut opening = vec![];
    let mut accumulation = vec![];
    for _ in 0..8 {
        let clock = Instant::now();
        let mut d = deal(pc, &av, &bv, n, t, rng);
        generation.push(clock.elapsed().as_secs_f64() * 1000.);
        let clock = Instant::now();
        assert!(check_deal(pc, &d, 0));
        verification.push(clock.elapsed().as_secs_f64() * 1000.);
        d.shares[0][0] += Scalar::ONE;
        assert!(!check_deal(pc, &d, 0));
        d.shares[0][0] -= Scalar::ONE;
        let another = d.clone();
        let clock = Instant::now();
        add_deal(&mut d, &another);
        accumulation.push(clock.elapsed().as_secs_f64() * 1000.);
        let clock = Instant::now();
        let (a, b) = open(&d, t);
        for l in 0..4 {
            assert_eq!(a[l], av[l] + av[l]);
            assert_eq!(b[l], bv[l] + bv[l]);
            assert_eq!(pc.commit(a[l], b[l]), d.coeff[l][0]);
        }
        opening.push(clock.elapsed().as_secs_f64() * 1000.);
    }
    json!({"n":n,"threshold":t,"deal_generation_all_recipients":stat(&generation),"verify_one_recipient":stat(&verification),"aggregate_all_recipient_shares_and_public_coeffs":stat(&accumulation),"open_and_verify_aggregate":stat(&opening),"private_bytes_per_recipient":256,"private_bytes_all_recipients":256*n,"coefficient_commitment_bytes":128*t,"extra_coeff_bytes_excluding_existing_amount_commitments":128*(t-1),"aggregate_private_state_per_recipient_bytes":256,"aggregate_public_coefficients_bytes":128*t,"opening_raw_share_bytes":256*t,"scope":"Pedersen VSS math only; authenticated transport, qualified-set agreement, source/range proofs, replica availability not included"})
}
fn full_flow(pc: &PedersenGens, bp: &BulletproofGens, rng: &mut ChaCha20Rng) -> Value {
    let m = BigUint::from(1_000_000u64);
    let a = BigUint::from(400u64) * &m;
    let b = BigUint::from(600u64) * &m;
    let op = Op::Issue {
        p: &m * &m,
        d: &m * &m,
    };
    let first = prove(pc, bp, &op, &[a.clone(), a.clone()], 1, rng, None);
    let second = prove(
        pc,
        bp,
        &op,
        &[b.clone(), b.clone()],
        (1u64 << 32) | 1,
        rng,
        None,
    );
    assert!(verify(pc, bp, &op, &first, 1) && verify(pc, bp, &op, &second, (1u64 << 32) | 1));
    let av: Vec<_> = words(&a, 4).iter().map(|w| Scalar::from(*w)).collect();
    let bv: Vec<_> = words(&b, 4).iter().map(|w| Scalar::from(*w)).collect();
    let mut aggregate = deal(pc, &av, &first.blinds[1], 16, 11, rng);
    let second_deal = deal(pc, &bv, &second.blinds[1], 16, 11, rng);
    for l in 0..4 {
        assert_eq!(aggregate.coeff[l][0].compress(), first.commitments[1][l]);
        assert_eq!(second_deal.coeff[l][0].compress(), second.commitments[1][l]);
    }
    add_deal(&mut aggregate, &second_deal);
    let clock = Instant::now();
    let moved = reshare(pc, &aggregate, 11, 16, 11, rng);
    let handoff_ms = clock.elapsed().as_secs_f64() * 1000.;
    let (sum, blinding) = open(&moved, 11);
    let total = reconstructed_integer(&sum);
    assert_eq!(total, &a + &b);
    for l in 0..4 {
        assert_eq!(pc.commit(sum[l], blinding[l]), aggregate.coeff[l][0]);
    }
    let budget = BigUint::from(100u64) * &m;
    let f = &budget * &m / &total;
    let p = BigUint::from(2u64) * &m;
    let g = &a * &f / &m;
    let c = &g * &p / &m;
    let old = BigUint::from(100u64) * &m;
    let new = &old + &g;
    let claim_op = Op::Claim {
        f: f.clone(),
        p: p.clone(),
    };
    let claim = prove(
        pc,
        bp,
        &claim_op,
        &[a.clone(), g.clone(), c.clone(), old.clone(), new.clone()],
        2,
        rng,
        Some(&first.blinds[1]),
    );
    assert_eq!(claim.commitments[0], first.commitments[1]);
    assert!(verify(pc, bp, &claim_op, &claim, 2));
    // Arithmetic adversarial checks. Authentication/nullifier state is outside this harness.
    let bad_op = Op::Claim {
        f: &f + BigUint::one(),
        p: p.clone(),
    };
    assert!(!verify(pc, bp, &bad_op, &claim, 2));
    let mut invalid = vec![a, g.clone(), c.clone(), old, new.clone() + BigUint::one()];
    let bad = prove(pc, bp, &claim_op, &invalid, 2, rng, None);
    assert!(!verify(pc, bp, &claim_op, &bad, 2));
    invalid[4] = new.clone();
    invalid[1] = &g + BigUint::one();
    let bad = prove(pc, bp, &claim_op, &invalid, 2, rng, None);
    assert!(!verify(pc, bp, &claim_op, &bad, 2));
    let x = BigUint::from(7u64) * &m;
    let after = &new - &x;
    let withdraw_op = Op::Withdraw { x: x.clone() };
    let withdrawal = prove(
        pc,
        bp,
        &withdraw_op,
        &[new.clone(), after.clone()],
        3,
        rng,
        Some(&claim.blinds[4]),
    );
    assert_eq!(withdrawal.commitments[0], claim.commitments[4]);
    assert!(verify(pc, bp, &withdraw_op, &withdrawal, 3));
    json!({"valid":true,"n":16,"threshold":11,"old_to_new_committee_handoff_all_dealers_all_recipients_ms":handoff_ms,
 "reshare_raw_private_bytes":11*16*256,"reshare_raw_public_coeff_bytes":11*11*128,
 "public_trace":{"day_nominal":total.to_string(),"lysis_budget":budget.to_string(),"fraction":f.to_string(),"entry_price":p.to_string(),"withdraw_gratis_minor":x.to_string(),"coen_native_amount":(&x*BigUint::from(1_000_000_000_000u64)).to_string()},
 "fixture_private_values_debug_only":{"nominal_owner_1":400_000_000u64,"gratis_load":g.to_string(),"cost":c.to_string(),"balance_after_claim":new.to_string(),"balance_after_withdraw":after.to_string()},
 "negative_checks":["wrong nonce rejected","wrong fraction rejected","balance overcredit rejected","wrong floor result rejected","bad VSS share rejected in profile benches"],
 "not_covered":["real offer authorization","dynamic qualified-set agreement","source/Fidelity proofs","PayNote spend/membership","Nod eligibility/PoW/nullifier state","real chain execution","proactive secure erasure","network handoff"]})
}
fn main() {
    let pc = PedersenGens::default();
    let bp = BulletproofGens::new(16384, 1);
    let mut rng = ChaCha20Rng::from_seed([19u8; 32]);
    let flow = full_flow(&pc, &bp, &mut rng);
    eprintln!("full arithmetic/VSS flow passed");
    let m = BigUint::from(1_000_000u64);
    let identity = Op::Issue {
        p: &m * &m,
        d: &m * &m,
    };
    let maximum = (BigUint::one() << 256usize) - BigUint::one();
    let endpoint = prove(
        &pc,
        &bp,
        &identity,
        &[maximum.clone(), maximum],
        9,
        &mut rng,
        None,
    );
    assert!(verify(&pc, &bp, &identity, &endpoint, 9));
    let zero = prove(
        &pc,
        &bp,
        &identity,
        &[BigUint::zero(), BigUint::zero()],
        9,
        &mut rng,
        None,
    );
    assert!(!verify(&pc, &bp, &identity, &zero, 9));
    eprintln!("uint256 maximum accepted; zero nominal rejected");
    let a = (BigUint::one() << 252usize) + BigUint::from(12345u64);
    let p = &m * &m;
    let d = &m * &m;
    let mut results = vec![proof_bench(
        &pc,
        &bp,
        &Op::Issue {
            p: p.clone(),
            d: d.clone(),
        },
        &[a.clone(), a.clone()],
        &mut rng,
    )];
    eprintln!("issue proof benchmark completed");
    let f = BigUint::from(150_000u64);
    let price = BigUint::from(1_700_000u64);
    let g = &a * &f / &m;
    let c = &g * &price / &m;
    let old = BigUint::one() << 249usize;
    let new = &old + &g;
    results.push(proof_bench(
        &pc,
        &bp,
        &Op::Claim { f, p: price },
        &[a, g, c, old, new.clone()],
        &mut rng,
    ));
    eprintln!("claim proof benchmark completed");
    let x = BigUint::from(7_000_000u64);
    let after = &new - &x;
    results.push(proof_bench(
        &pc,
        &bp,
        &Op::Withdraw { x: x.clone() },
        &[new, after],
        &mut rng,
    ));
    eprintln!("withdraw proof benchmark completed");
    let mut vss = vec![];
    for (n, t) in [(16, 11), (128, 86)] {
        vss.push(vss_bench(&pc, n, t, &mut rng));
        eprintln!("VSS {n}/{t} completed");
    }
    let mut ranges = vec![];
    for count in [4usize, 8] {
        let gens = BulletproofGens::new(64, count);
        let values: Vec<_> = (0..count).map(|_| rng.next_u64()).collect();
        let blinds: Vec<_> = (0..count).map(|_| Scalar::random(&mut rng)).collect();
        let mut pt = vec![];
        let mut vt = vec![];
        let mut size = 0;
        for _ in 0..4 {
            let mut tr = Transcript::new(b"range-bench");
            let t = Instant::now();
            let (proof, commits) =
                RangeProof::prove_multiple(&gens, &pc, &mut tr, &values, &blinds, 64).unwrap();
            pt.push(t.elapsed().as_secs_f64() * 1000.);
            let mut tr = Transcript::new(b"range-bench");
            let t = Instant::now();
            proof
                .verify_multiple(&gens, &pc, &mut tr, &commits, 64)
                .unwrap();
            vt.push(t.elapsed().as_secs_f64() * 1000.);
            size = proof.to_bytes().len();
        }
        ranges.push(json!({"limbs64":count,"proof_bytes":size,"commitment_bytes":count*32,"prove":stat(&pt),"verify":stat(&vt)}));
    }
    println!("{}",serde_json::to_string_pretty(&json!({"kind":"research_only_component_measurements","arch":std::env::consts::ARCH,"versions":{"bulletproofs":"5.0.0 + three CtOption decoder compatibility edits","curve25519_dalek":"4.1.3"},"r1cs_status":"experimental yoloproofs; custom gadgets not audited","full_flow":flow,"arithmetic_proofs":results,"vss":vss,"range_proofs":ranges,"uint256_boundary_checks":{"maximum_identity_accepted":true,"zero_nominal_rejected":true}})).unwrap());
}
