//! Exact integer conservation with bounded signed carries, not one weighted
//! equality modulo q. Every operand is a normalized uint256 ciphertext.
use crate::*;

#[derive(Clone, Serialize, Deserialize)]
pub struct MoneyProof {
    pub f: Vec<Bytes>,
    pub range: Vec<u8>,
    pub equalities: Vec<CipherEquality>,
    pub carries: Vec<Bytes>,
    pub carry_range: Vec<u8>,
    pub a: Vec<Bytes>,
    pub z: Vec<Bytes>,
}
pub struct MoneyWitness {
    pub values: Vec<BigUint>,
    pub blinds: Vec<Vec<Scalar>>,
}

fn validate_relation(n: usize, coeff: &[i32], rhs: &BigUint) -> Result<()> {
    if n == 0
        || n > 8
        || coeff.len() != n
        || coeff.iter().any(|c| ![-1, 0, 1].contains(c))
        || rhs.bits() > 256
    {
        return Err("money relation profile".into());
    }
    Ok(())
}
pub fn prove(
    cts: &[Cipher],
    keys: &[Scalar],
    coeff: &[i32],
    rhs: &BigUint,
    context: &Bytes,
) -> Result<(MoneyProof, MoneyWitness)> {
    validate_relation(cts.len(), coeff, rhs)?;
    if keys.len() != cts.len() {
        return Err("key count".into());
    }
    let values = cts
        .iter()
        .zip(keys)
        .map(|(c, k)| c.decrypt(*k))
        .collect::<Result<Vec<_>>>()?;
    let ds = values.iter().map(digits).collect::<Result<Vec<_>>>()?;
    let blinds: Vec<Vec<Scalar>> = (0..cts.len())
        .map(|_| (0..16).map(|_| Scalar::random(&mut OsRng)).collect())
        .collect();
    let range_context = hash(&(b"money-range", context, cts, coeff, rhs.to_bytes_le()));
    let (f, range) = prove_range(
        &ds.iter().flatten().copied().collect::<Vec<_>>(),
        &blinds.iter().flatten().copied().collect::<Vec<_>>(),
        16,
        &range_context,
    )?;
    let eq_context = hash(&(b"money-equalities", &range_context, &f, &range));
    let equalities = cts
        .iter()
        .zip(keys)
        .enumerate()
        .map(|(i, (c, k))| {
            equality_prove(
                c,
                *k,
                &f[i * 16..(i + 1) * 16],
                &blinds[i],
                &hash(&(eq_context, i as u64)),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    // t_0=t_16=0; interior t_i=u_i-128, with u_i range-proved in [0,256).
    let mut carry = 0i64;
    let mut us = vec![];
    let rd = digits(rhs)?;
    for i in 0..16 {
        let total = ds
            .iter()
            .zip(coeff)
            .map(|(d, c)| d[i] as i64 * *c as i64)
            .sum::<i64>()
            + carry
            - rd[i] as i64;
        if total % 65536 != 0 {
            return Err("integer conservation failed".into());
        }
        carry = total / 65536;
        if !(-128..128).contains(&carry) || (i == 15 && carry != 0) {
            return Err("integer overflow/underflow".into());
        }
        us.push((carry + 128) as u64);
    }
    let mut cr: Vec<_> = (0..16).map(|_| Scalar::random(&mut OsRng)).collect();
    cr[15] = Scalar::ZERO;
    let carry_context = hash(&(b"money-carries", eq_context, &equalities));
    let (carries, carry_range) = prove_range(&us, &cr, 8, &carry_context)?;
    let mut openings = vec![];
    for i in 0..16 {
        let mut r = Scalar::ZERO;
        for j in 0..cts.len() {
            if coeff[j] == 1 {
                r += blinds[j][i]
            } else if coeff[j] == -1 {
                r -= blinds[j][i]
            }
        }
        if i > 0 {
            r += cr[i - 1]
        }
        r -= Scalar::from(65536u64) * cr[i];
        openings.push(r);
    }
    let ws: Vec<_> = (0..16).map(|_| Scalar::random(&mut OsRng)).collect();
    let a = ws
        .iter()
        .map(|w| enc(w * pc().B_blinding))
        .collect::<Vec<_>>();
    let c = challenge(
        b"OUTBE-V2-CARRY-EQUALITY-1",
        &(
            context,
            cts,
            coeff,
            rhs.to_bytes_le(),
            &f,
            &range,
            &equalities,
            &carries,
            &carry_range,
            &a,
        ),
    );
    let z = ws
        .iter()
        .zip(&openings)
        .map(|(w, r)| (w + c * r).to_bytes())
        .collect();
    Ok((
        MoneyProof {
            f,
            range,
            equalities,
            carries,
            carry_range,
            a,
            z,
        },
        MoneyWitness { values, blinds },
    ))
}
pub fn verify(
    cts: &[Cipher],
    coeff: &[i32],
    rhs: &BigUint,
    context: &Bytes,
    p: &MoneyProof,
) -> Result<()> {
    validate_relation(cts.len(), coeff, rhs)?;
    if p.f.len() != (cts.len() * 16).next_power_of_two()
        || p.equalities.len() != cts.len()
        || p.carries.len() != 16
        || p.a.len() != 16
        || p.z.len() != 16
    {
        return Err("money proof shape".into());
    }
    // Padding is a specified public zero, never an unconstrained extra operand.
    for x in &p.f[cts.len() * 16..] {
        if *x != enc(RistrettoPoint::default()) {
            return Err("range padding".into());
        }
    }
    let range_context = hash(&(b"money-range", context, cts, coeff, rhs.to_bytes_le()));
    verify_range(&p.f, &p.range, 16, &range_context)?;
    let eq_context = hash(&(b"money-equalities", &range_context, &p.f, &p.range));
    for (i, ct) in cts.iter().enumerate() {
        equality_verify(
            ct,
            &p.f[i * 16..(i + 1) * 16],
            &p.equalities[i],
            &hash(&(eq_context, i as u64)),
        )?;
    }
    let carry_context = hash(&(b"money-carries", eq_context, &p.equalities));
    verify_range(&p.carries, &p.carry_range, 8, &carry_context)?;
    if p.carries[15] != enc(Scalar::from(128u64) * pc().B) {
        return Err("nonzero final carry".into());
    }
    let c = challenge(
        b"OUTBE-V2-CARRY-EQUALITY-1",
        &(
            context,
            cts,
            coeff,
            rhs.to_bytes_le(),
            &p.f,
            &p.range,
            &p.equalities,
            &p.carries,
            &p.carry_range,
            &p.a,
        ),
    );
    let rd = digits(rhs)?;
    for i in 0..16 {
        let mut e = -Scalar::from(rd[i]) * pc().B;
        for j in 0..cts.len() {
            if coeff[j] == 1 {
                e += point(p.f[j * 16 + i])?
            } else if coeff[j] == -1 {
                e -= point(p.f[j * 16 + i])?
            }
        }
        if i > 0 {
            e += point(p.carries[i - 1])? - Scalar::from(128u64) * pc().B;
        }
        e -= Scalar::from(65536u64) * (point(p.carries[i])? - Scalar::from(128u64) * pc().B);
        if scalar(p.z[i])? * pc().B_blinding != point(p.a[i])? + c * e {
            return Err("local integer carry relation".into());
        }
    }
    Ok(())
}

/// Ciphertext/BP/equality proof with no monetary equation, used ONLY underneath
/// the independently verified exact public-coefficient relations and source links.
pub fn certify(
    cts: &[Cipher],
    keys: &[Scalar],
    context: &Bytes,
) -> Result<(MoneyProof, MoneyWitness)> {
    // Each operand appears with coefficient zero. This proves decryptability,
    // 16-bit normalization and key knowledge; it authorizes no credit by itself.
    prove(
        cts,
        keys,
        &vec![0; cts.len()],
        &BigUint::from(0u32),
        context,
    )
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HandlesProof {
    pub a: Vec<Bytes>,
    pub b: Vec<Bytes>,
    pub z: Vec<Bytes>,
}
pub fn dual_encrypt(
    n: &BigUint,
    sender: Bytes,
    receiver: Bytes,
    context: &Bytes,
) -> Result<(Cipher, Cipher, HandlesProof)> {
    let s = point(sender)?;
    let r = point(receiver)?;
    if s == RistrettoPoint::default() || r == RistrettoPoint::default() {
        return Err("zero key".into());
    }
    let mut c = vec![];
    let mut ds = vec![];
    let mut dr = vec![];
    let mut rs = vec![];
    let mut ws = vec![];
    for v in digits(n)? {
        let x = Scalar::random(&mut OsRng);
        rs.push(x);
        ws.push(Scalar::random(&mut OsRng));
        c.push(enc(pc().commit(Scalar::from(v), x)));
        ds.push(enc(x * s));
        dr.push(enc(x * r));
    }
    let left = Cipher {
        key: sender,
        c: c.clone(),
        d: ds,
    };
    let right = Cipher {
        key: receiver,
        c,
        d: dr,
    };
    let a = ws.iter().map(|w| enc(w * s)).collect::<Vec<_>>();
    let b = ws.iter().map(|w| enc(w * r)).collect::<Vec<_>>();
    let ch = challenge(
        b"OUTBE-V2-DUAL-HANDLES-1",
        &(context, &left, &right, &a, &b),
    );
    let z = ws
        .iter()
        .zip(&rs)
        .map(|(w, x)| (w + ch * x).to_bytes())
        .collect();
    Ok((left, right, HandlesProof { a, b, z }))
}
pub fn verify_handles(
    left: &Cipher,
    right: &Cipher,
    p: &HandlesProof,
    context: &Bytes,
) -> Result<()> {
    left.validate()?;
    right.validate()?;
    if left.c != right.c || p.a.len() != 16 || p.b.len() != 16 || p.z.len() != 16 {
        return Err("dual handle shape/commitments".into());
    }
    let ch = challenge(
        b"OUTBE-V2-DUAL-HANDLES-1",
        &(context, left, right, &p.a, &p.b),
    );
    for i in 0..16 {
        let z = scalar(p.z[i])?;
        if z * point(left.key)? != point(p.a[i])? + ch * point(left.d[i])?
            || z * point(right.key)? != point(p.b[i])? + ch * point(right.d[i])?
        {
            return Err("dual handle equality".into());
        }
    }
    Ok(())
}
