use curve25519_dalek::scalar::Scalar;
use num_bigint::BigUint;
use outbe_private_lifecycle_poc::state::Note;
use outbe_twisted_poc::*;
use outbe_twisted_poc::{bridge, money};

#[test]
fn exact_u256_carry_borrow_and_field_alias() -> Result<()> {
    let s = keygen();
    let k = public_key(s)?;
    let max = max_u256();
    let one = BigUint::from(1u32);
    let zero = BigUint::from(0u32);
    let values = [max.clone(), zero.clone(), &max - &one, one.clone()];
    let cts = values
        .iter()
        .map(|n| Cipher::encrypt(n, k))
        .collect::<Result<Vec<_>>>()?;
    let ctx = hash(&"full uint256 boundary");
    let (p, _) = money::prove(&cts, &[s; 4], &[1, 1, -1, -1], &zero, &ctx)?;
    money::verify(&cts, &[1, 1, -1, -1], &zero, &ctx, &p)?;
    assert!(money::verify(&cts, &[1, 1, -1, -1], &zero, &hash(&"wrong operation"), &p).is_err());
    let mut bad = p.clone();
    bad.carries[15] = enc(Scalar::from(129u64) * pc().B);
    assert!(money::verify(&cts, &[1, 1, -1, -1], &zero, &ctx, &bad).is_err());
    let overflow = [
        Cipher::encrypt(&max, k)?,
        Cipher::encrypt(&one, k)?,
        Cipher::encrypt(&zero, k)?,
    ];
    assert!(money::prove(&overflow, &[s; 3], &[1, 1, -1], &zero, &ctx).is_err());
    let underflow = [
        Cipher::encrypt(&zero, k)?,
        Cipher::encrypt(&one, k)?,
        Cipher::encrypt(&max, k)?,
    ];
    assert!(money::prove(&underflow, &[s; 3], &[1, -1, -1], &zero, &ctx).is_err());
    let q = (BigUint::from(1u32) << 252usize)
        + BigUint::parse_bytes(b"27742317777372353535851937790883648493", 10).unwrap();
    let alias = [Cipher::encrypt(&one, k)?, Cipher::encrypt(&q, k)?];
    assert!(money::prove(&alias, &[s; 2], &[1, -1], &one, &ctx).is_err());
    assert!(Cipher::encrypt(&(&max + &one), k).is_err());
    Ok(())
}

#[test]
fn two_owners_amount_link_and_key_only_recovery() -> Result<()> {
    let a = keygen();
    let b = keygen();
    let ka = public_key(a)?;
    let kb = public_key(b)?;
    let amount = (BigUint::from(1u32) << 200usize) + 65537u32;
    let av = &amount * 7u32;
    let bv = BigUint::from(65535u32);
    let ctx = hash(&"atomic transfer A to B");
    let (asend, brecv, handles) = money::dual_encrypt(&amount, ka, kb, &ctx)?;
    money::verify_handles(&asend, &brecv, &handles, &ctx)?;
    assert_eq!(brecv.decrypt(b)?, amount);
    assert!(brecv.decrypt(a).is_err());
    let sender = [
        Cipher::encrypt(&av, ka)?,
        Cipher::encrypt(&(&av - &amount), ka)?,
        asend.clone(),
    ];
    let (sp, _) = money::prove(&sender, &[a; 3], &[1, -1, -1], &BigUint::from(0u32), &ctx)?;
    let receiver = [
        Cipher::encrypt(&bv, kb)?,
        brecv.clone(),
        Cipher::encrypt(&(&bv + &amount), kb)?,
    ];
    let (rp, _) = money::prove(&receiver, &[b; 3], &[1, 1, -1], &BigUint::from(0u32), &ctx)?;
    money::verify(&sender, &[1, -1, -1], &BigUint::from(0u32), &ctx, &sp)?;
    money::verify(&receiver, &[1, 1, -1], &BigUint::from(0u32), &ctx, &rp)?;
    let mut bad = brecv;
    bad.d[0] = asend.d[0];
    assert!(money::verify_handles(&asend, &bad, &handles, &ctx).is_err());
    Ok(())
}

#[test]
fn cross_curve_bridge_rejects_different_integer_and_public_mutations() -> Result<()> {
    let n = max_u256() - 12345u32;
    let note = Note::fresh(n.clone())?;
    let s = keygen();
    let ct = Cipher::encrypt(&n, public_key(s)?)?;
    let ctx = hash(&"bridge context");
    let (p, w) = money::certify(&[ct], &[s], &ctx)?;
    let b = bridge::prove(&note, &p.f, &w.blinds[0], &ctx)?;
    bridge::verify(&note.commitments()?, &p.f, &ctx, &b)?;
    let other = Note::fresh(n - 1u32)?;
    // Prover knows valid openings on both sides, but for DIFFERENT integers.
    let fake = bridge::prove(&other, &p.f, &w.blinds[0], &ctx)?;
    assert!(bridge::verify(&other.commitments()?, &p.f, &ctx, &fake).is_err());
    assert!(bridge::verify(&note.commitments()?, &p.f, &hash(&"other note ID"), &b).is_err());
    let mut bad = b.clone();
    bad.bits[0].c0[0] ^= 1;
    assert!(bridge::verify(&note.commitments()?, &p.f, &ctx, &bad).is_err());
    Ok(())
}
