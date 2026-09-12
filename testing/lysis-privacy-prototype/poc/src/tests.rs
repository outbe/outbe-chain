use crate::{
    crypto::*,
    state::{self, Note, Transition},
    vss::*,
};
use ark_bn254::Fr;
use ark_ed_on_bn254::Fr as Scalar;
use ark_ff::{UniformRand, Zero};
use ark_relations::r1cs::{ConstraintSynthesizer, ConstraintSystem};
use num_bigint::BigUint;
use rand::rngs::OsRng;

fn satisfied(t: Transition) -> bool {
    let cs = ConstraintSystem::<Fr>::new_ref();
    t.generate_constraints(cs.clone()).is_ok() && cs.is_satisfied().unwrap()
}

#[test]
fn uint256_boundaries_and_no_field_aliases() {
    let max = (BigUint::from(1u32) << 256usize) - 1u32;
    let mut t = state::fixture("withdraw").unwrap();
    t.notes = vec![
        Note::fresh(max.clone()).unwrap(),
        Note::fresh(&max - 1u32).unwrap(),
    ];
    t.public.notes = t.notes.iter().map(|n| n.commitments().unwrap()).collect();
    t.public.amount = "1".into();
    assert!(satisfied(t.clone()));
    t.notes[0].value = (&max + (BigUint::from(1u32) << 256usize)).to_string();
    assert!(!satisfied(t));
    let mut t = state::fixture("claim").unwrap();
    t.public.price = max.to_string();
    assert!(!satisfied(t));
    let mut t = state::fixture("mint").unwrap();
    t.notes[1] = Note::fresh(max).unwrap();
    t.public.notes[1] = t.notes[1].commitments().unwrap();
    assert!(!satisfied(t));
}

#[test]
fn noncanonical_openings_and_context() {
    let mut t = state::fixture("claim").unwrap();
    t.notes[0].blinds[0] =
        (integer(&t.notes[0].blinds[0]).unwrap() + (BigUint::from(1u32) << 251usize)).to_string();
    assert!(!satisfied(t));
    let mut t = state::fixture("claim").unwrap();
    t.nominal = (integer(&t.nominal).unwrap() + (BigUint::from(1u32) << 104usize)).to_string();
    assert!(!satisfied(t));
    assert!(integer("01").is_err());
    assert!(integer("+1").is_err());
    assert!(field_from_hex("01").is_err());
    assert!(decode::<ark_ed_on_bn254::EdwardsAffine>(&[255u8; 32]).is_err());
}

#[test]
fn vss_bad_shares_and_actual_coordinate_recovery() {
    let y = Scalar::from(123456789u64);
    let z = Scalar::rand(&mut OsRng);
    let (p, ss) = deal("source".into(), 0, y, z, 3).unwrap();
    for s in &ss {
        check(&p, s).unwrap();
    }
    assert_eq!(recover(&[ss[0].clone(), ss[2].clone()]).unwrap(), (y, z));
    let mut corrupt = ss[0].clone();
    corrupt.y = scalar_integer(scalar(&integer(&corrupt.y).unwrap()).unwrap() + Scalar::from(1u32))
        .to_string();
    assert!(check(&p, &corrupt).is_err());
    assert!(recover(&[ss[0].clone()]).is_err());
    assert!(recover(&[ss[0].clone(), ss[0].clone()]).is_err());
    let xs = [1, 3];
    let mut new_y = Scalar::zero();
    let mut new_z = Scalar::zero();
    for (i, x) in xs.iter().enumerate() {
        let l = lagrange(&xs, i).unwrap();
        let old = &ss[(*x - 1) as usize];
        let (q, rs) = deal(
            "source".into(),
            1,
            l * scalar(&integer(&old.y).unwrap()).unwrap(),
            l * scalar(&integer(&old.z).unwrap()).unwrap(),
            3,
        )
        .unwrap();
        check(&q, &rs[1]).unwrap();
        new_y += scalar(&integer(&rs[1].y).unwrap()).unwrap();
        new_z += scalar(&integer(&rs[1].z).unwrap()).unwrap();
    }
    // Shares at the new coordinate are hidden evaluations, not the old value.
    assert_ne!((new_y, new_z), (y, z));
}

#[test]
fn encrypted_shares_bind_recipient_context_and_authenticator() {
    let dir = std::env::temp_dir().join(format!("outbe-vss-test-{}", rand::random::<u64>()));
    keygen(&dir, "holder".into()).unwrap();
    let k = keys(&dir).unwrap();
    let e = seal(&k.public, "epoch-root", b"private local share").unwrap();
    assert_eq!(
        unseal(&k, &e, "epoch-root").unwrap(),
        b"private local share"
    );
    assert!(unseal(&k, &e, "other-epoch").is_err());
    let mut bad = e.clone();
    let mut bytes = hex::decode(&bad.ciphertext).unwrap();
    bytes[0] ^= 1;
    bad.ciphertext = hex::encode(bytes);
    assert!(unseal(&k, &bad, "epoch-root").is_err());
    let sig = sign(&k, &e).unwrap();
    verify_signature(&k.public, &e, &sig).unwrap();
    assert!(verify_signature(&k.public, &bad, &sig).is_err());
    let mut bad = k.public.clone();
    bad.encryption = "00".repeat(32);
    assert!(seal(&bad, "epoch-root", b"share").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
