use curve25519_dalek::scalar::Scalar;
use num_bigint::{BigInt, BigUint};
use outbe_ristretto_lifecycle_poc::{self as p, crypto as c, note_link, wide};
use rand::rngs::OsRng;
fn committed(values: &[BigUint]) -> (Vec<p::Bytes>, Vec<Vec<Scalar>>) {
    let rs = (0..values.len())
        .map(|_| {
            (0..16)
                .map(|_| Scalar::random(&mut OsRng))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let fs = values
        .iter()
        .zip(&rs)
        .flat_map(|(v, rs)| {
            p::digits(v)
                .unwrap()
                .into_iter()
                .zip(rs)
                .map(|(v, r)| p::enc(p::pc().commit(Scalar::from(v), *r)))
                .collect::<Vec<_>>()
        })
        .collect();
    (fs, rs)
}
#[test]
fn ristretto_vss_and_full_uint256_note() {
    let q = c::scalar_modulus();
    assert_eq!(q.bits(), 253);
    let n = (BigUint::from(1u8) << 256usize) - 1u8;
    let note = p::state::Note::fresh(n.clone()).unwrap();
    let (f, rs) = committed(&[n]);
    let ctx = p::hash(&"limbs");
    let proof = note_link::prove(&note, &f, &rs[0], &ctx).unwrap();
    note_link::verify(&note.commitments().unwrap(), &f, &proof, &ctx).unwrap();
    let mut bad = note.commitments().unwrap();
    bad[3] = hex::encode(
        (c::point_decode(&bad[3]).unwrap() + p::pc().B)
            .compress()
            .to_bytes(),
    );
    assert!(note_link::verify(&bad, &f, &proof, &ctx).is_err());
    let y = Scalar::random(&mut OsRng);
    let z = Scalar::random(&mut OsRng);
    let (poly, ss) = p::vss::deal("s".into(), 0, y, z, 3).unwrap();
    for s in &ss {
        p::vss::check(&poly, s).unwrap();
    }
    assert_eq!(p::vss::recover(&ss[1..]).unwrap(), (y, z));
    let mut bad = ss[0].clone();
    bad.y = (c::integer(&bad.y).unwrap() + 1u8).to_string();
    assert!(p::vss::check(&poly, &bad).is_err());
}
#[test]
fn wide_conservation_checks_integer_not_scalar() {
    let max = (BigUint::from(1u8) << 256usize) - 1u8;
    let values = vec![max.clone(), max.clone(), max.clone(), max];
    let (fs, rs) = committed(&values);
    let r = wide::Relation {
        coefficients: vec![1.into(), 1.into(), (-1).into(), (-1).into()],
        rhs: 0u8.into(),
    };
    let ctx = p::hash(&"wide");
    let proof = wide::prove(&fs, &values, &rs, &r, &ctx).unwrap();
    wide::verify(&fs, 4, &r, &proof, &ctx).unwrap();
    let mut bad = r.clone();
    bad.rhs = 1u8.into();
    assert!(wide::verify(&fs, 4, &bad, &proof, &ctx).is_err());
    let alias = vec![c::scalar_modulus() + 1u8];
    let (f, b) = committed(&alias);
    let ar = wide::Relation {
        coefficients: vec![1.into()],
        rhs: 1u8.into(),
    };
    assert!(wide::prove(&f, &alias, &b, &ar, &ctx).is_err());
}
#[test]
fn wide_public_multiplication_and_overflow() {
    let a = BigUint::from(123456789u64);
    let factor = (BigUint::from(1u8) << 180usize) + 333u32;
    let out = &a * &factor;
    let values = vec![a.clone(), out];
    let (fs, rs) = committed(&values);
    let r = wide::Relation {
        coefficients: vec![BigInt::from(factor), (-1).into()],
        rhs: 0u8.into(),
    };
    let ctx = p::hash(&"public multiplication");
    let proof = wide::prove(&fs, &values, &rs, &r, &ctx).unwrap();
    wide::verify(&fs, 2, &r, &proof, &ctx).unwrap();
    let r = wide::Relation {
        coefficients: vec![BigInt::from(BigUint::from(1u8) << 511usize), (-1).into()],
        rhs: 0u8.into(),
    };
    assert!(wide::prove(&fs, &values, &rs, &r, &ctx).is_err());
}
#[test]
fn source104_no_alias_and_context_substitution() {
    let a = (BigUint::from(1u8) << 103usize) + 7u8;
    let r = Scalar::random(&mut OsRng);
    let source = p::vss::point_hex(c::commit(&a, r).unwrap()).unwrap();
    let (fs, rs) = committed(&[a.clone()]);
    let ctx = p::hash(&"source104");
    let pr = note_link::prove_source(&source, &fs, &rs[0], &a, r, &ctx).unwrap();
    note_link::verify_source(&source, &fs, &pr, &ctx).unwrap();
    let mut bad = fs.clone();
    bad[7] = p::enc(p::point(fs[7]).unwrap() + p::pc().B);
    assert!(note_link::verify_source(&source, &bad, &pr, &ctx).is_err());
    assert!(note_link::verify_source(&source, &fs, &pr, &p::hash(&"other source")).is_err());
    assert!(
        note_link::prove_source(&source, &fs, &rs[0], &(a + c::scalar_modulus()), r, &ctx).is_err()
    );
}
#[test]
fn source_and_last_step_bind_same_private_value() {
    use ark_bn254::Fr;
    use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
    let good = p::link::fixture(1, 17);
    {
        let cs = ConstraintSystem::<Fr>::new_ref();
        good.clone().generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }
    {
        let step = p::source_opening::Step::new(good.clone(), 11);
        let cs = ConstraintSystem::<Fr>::new_ref();
        step.generate_constraints(cs.clone()).unwrap();
        assert!(cs.is_satisfied().unwrap());
    }
    let mut bad = good.clone();
    bad.private.nominal += 1u8;
    bad.public.opening_binding = p::source_opening::witness_digest(&bad);
    {
        let cs = ConstraintSystem::<Fr>::new_ref();
        bad.clone().generate_constraints(cs.clone()).unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
    let mut wrong = good;
    wrong.public.commitment += p::pc().B;
    wrong.public.opening_binding = p::source_opening::witness_digest(&wrong);
    {
        let cs = ConstraintSystem::<Fr>::new_ref();
        p::source_opening::Step::new(wrong, 11)
            .generate_constraints(cs.clone())
            .unwrap();
        assert!(!cs.is_satisfied().unwrap());
    }
}
