//! Research P-384 Pedersen gadget over the BN254 scalar constraint field.
//!
//! Complete homogeneous addition is adapted from ark-r1cs-std 0.5.0
//! (MIT OR Apache-2.0), short_weierstrass/mod.rs, RCB 2015 Algorithm 1:
//! https://eprint.iacr.org/2015/1060 . The stock ProjectiveVar hardcodes
//! the curve base field as constraint field and cannot represent this gadget.
//! Scalar range/canonicality and public commitment binding belong to the caller.

use ark_bn254::Fr;
use ark_ff::{Fp384, MontBackend, MontConfig, PrimeField};
use ark_r1cs_std::{boolean::Boolean, fields::emulated_fp::EmulatedFpVar, prelude::*};
use ark_relations::r1cs::SynthesisError;
use num_bigint::BigUint;
use p384::{
    elliptic_curve::{
        ff::PrimeField as CryptoPrimeField,
        hash2curve::{ExpandMsgXmd, GroupDigest},
        sec1::ToEncodedPoint,
        Group,
    },
    FieldBytes, NistP384, ProjectivePoint as Point, Scalar,
};
use sha2::Sha384;
use std::sync::OnceLock;

#[derive(MontConfig)]
#[modulus = "39402006196394479212279040100143613805079739270465446667948293404245721771496870329047266088258938001861606973112319"]
#[generator = "19"]
pub struct P384FqConfig;
pub type P384Fq = Fp384<MontBackend<P384FqConfig, 6>>;
pub type FqVar = EmulatedFpVar<P384Fq, Fr>;

const DST: &[u8] = b"OUTBE-RESEARCH-VSS-v1-P384_XMD:SHA-384_SSWU_RO_";
const MESSAGE: &[u8] =
    b"Pedersen blinding generator for Outbe aggregate research; not production parameters";

pub fn blinding_generator() -> Point {
    static H: OnceLock<Point> = OnceLock::new();
    *H.get_or_init(|| {
        let h = NistP384::hash_from_bytes::<ExpandMsgXmd<Sha384>>(&[MESSAGE], &[DST])
            .expect("fixed valid RFC 9380 inputs");
        assert!(!bool::from(h.is_identity()));
        assert_ne!(h, Point::GENERATOR);
        h
    })
}

fn coords(point: Point) -> [P384Fq; 3] {
    if bool::from(point.is_identity()) {
        return [P384Fq::from(0u64), P384Fq::from(1u64), P384Fq::from(0u64)];
    }
    let encoded = point.to_affine().to_encoded_point(false);
    [
        P384Fq::from_be_bytes_mod_order(encoded.x().expect("nonidentity x")),
        P384Fq::from_be_bytes_mod_order(encoded.y().expect("nonidentity y")),
        P384Fq::from(1u64),
    ]
}

/// Homogeneous coordinates: affine x = X / Z, affine y = Y / Z.
/// These are not Jacobian X/Z²,Y/Z³ coordinates. Identity is (0:1:0).
#[derive(Clone, Debug)]
pub struct P384Var {
    pub x: FqVar,
    pub y: FqVar,
    pub z: FqVar,
}

impl P384Var {
    pub fn identity() -> Self {
        Self::from_constant(Point::IDENTITY)
    }

    pub fn from_constant(point: Point) -> Self {
        let [x, y, z] = coords(point);
        Self {
            x: FqVar::Constant(x),
            y: FqVar::Constant(y),
            z: FqVar::Constant(z),
        }
    }

    /// Complete RCB addition for valid P-384 points, including infinity,
    /// doubling, and inverse pairs. All coordinates must originate in validated
    /// points/constant table selection; arbitrary unchecked witnesses are unsafe.
    pub fn add_complete(&self, rhs: &Self) -> Self {
        let a = -P384Fq::from(3u64);
        let b = P384Fq::from_be_bytes_mod_order(&[
            0xb3, 0x31, 0x2f, 0xa7, 0xe2, 0x3e, 0xe7, 0xe4, 0x98, 0x8e, 0x05, 0x6b, 0xe3, 0xf8,
            0x2d, 0x19, 0x18, 0x1d, 0x9c, 0x6e, 0xfe, 0x81, 0x41, 0x12, 0x03, 0x14, 0x08, 0x8f,
            0x50, 0x13, 0x87, 0x5a, 0xc6, 0x56, 0x39, 0x8d, 0x8a, 0x2e, 0xd1, 0x9d, 0x2a, 0x85,
            0xc8, 0xed, 0xd3, 0xec, 0x2a, 0xef,
        ]);
        let b3 = b * P384Fq::from(3u64);
        let xx = &self.x * &rhs.x;
        let yy = &self.y * &rhs.y;
        let zz = &self.z * &rhs.z;
        let xy = (&self.x + &self.y) * (&rhs.x + &rhs.y) - (&xx + &yy);
        let xz = (&self.x + &self.z) * (&rhs.x + &rhs.z) - (&xx + &zz);
        let yz = (&self.y + &self.z) * (&rhs.y + &rhs.z) - (&yy + &zz);
        let a_xz_b3_zz = &xz * a + &zz * b3;
        let yy_minus = &yy - &a_xz_b3_zz;
        let yy_plus = &yy + &a_xz_b3_zz;
        let a_zz = &zz * a;
        let three_xx_a_zz = &xx + &xx + &xx + &a_zz;
        let a_xx_minus_a_zz_b3_xz = (&xx - &a_zz) * a + &xz * b3;
        Self {
            x: &yy_minus * &xy - &yz * &a_xx_minus_a_zz_b3_xz,
            y: &yy_plus * &yy_minus + &three_xx_a_zz * &a_xx_minus_a_zz_b3_xz,
            z: &yy_plus * &yz + &xy * &three_xx_a_zz,
        }
    }
}

fn add_windows(
    mut accumulator: P384Var,
    bits: &[Boolean<Fr>],
    mut base: Point,
) -> Result<P384Var, SynthesisError> {
    for chunk in bits.chunks(2) {
        let window_bits = [
            chunk[0].clone(),
            chunk.get(1).cloned().unwrap_or(Boolean::FALSE),
        ];
        let twice = base.double();
        let table = [
            coords(Point::IDENTITY),
            coords(base),
            coords(twice),
            coords(base + twice),
        ];
        // Every coordinate uses the same constrained bits, so selected triples
        // are complete valid points (not independently selected coordinates).
        let selected = P384Var {
            x: FqVar::two_bit_lookup(&window_bits, &table.map(|c| c[0]))?,
            y: FqVar::two_bit_lookup(&window_bits, &table.map(|c| c[1]))?,
            z: FqVar::two_bit_lookup(&window_bits, &table.map(|c| c[2]))?,
        };
        accumulator = accumulator.add_complete(&selected);
        base = twice.double();
    }
    Ok(accumulator)
}

/// Proves group arithmetic C = aG + rH using LITTLE-ENDIAN constrained bits.
/// Caller must tie a_bits to the nominal arithmetic and enforce 0 <= r < q.
pub fn pedersen(a_bits: &[Boolean<Fr>], r_bits: &[Boolean<Fr>]) -> Result<P384Var, SynthesisError> {
    if a_bits.len() > 256 || r_bits.len() > 384 {
        return Err(SynthesisError::Unsatisfiable);
    }
    let result = add_windows(P384Var::identity(), a_bits, Point::GENERATOR)?;
    add_windows(result, r_bits, blinding_generator())
}

fn scalar(value: &BigUint) -> Result<Scalar, String> {
    let bytes = value.to_bytes_be();
    if bytes.len() > 48 {
        return Err("P-384 scalar exceeds 384 bits".into());
    }
    let mut repr = FieldBytes::default();
    repr[48 - bytes.len()..].copy_from_slice(&bytes);
    Option::<Scalar>::from(Scalar::from_repr(repr))
        .ok_or_else(|| "noncanonical P-384 scalar".into())
}

/// Native reference. The public admission format deliberately requires a
/// nonidentity commitment; a wallet can resample r in the identity case.
pub fn native_commit(a: &BigUint, r: &BigUint) -> Result<[u8; 49], String> {
    if a.bits() > 256 {
        return Err("nominal exceeds uint256".into());
    }
    let point = Point::GENERATOR * scalar(a)? + blinding_generator() * scalar(r)?;
    if bool::from(point.is_identity()) {
        return Err("identity commitment requires resampling blinding".into());
    }
    point
        .to_affine()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .map_err(|_| "unexpected P-384 SEC1 length".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_ff::Field;
    use ark_relations::r1cs::{ConstraintSystem, ConstraintSystemRef};
    use rand::{RngCore, SeedableRng};
    use rand_chacha::ChaCha20Rng;

    fn bits(cs: ConstraintSystemRef<Fr>, value: &BigUint, length: usize) -> Vec<Boolean<Fr>> {
        (0..length)
            .map(|i| Boolean::new_witness(cs.clone(), || Ok(value.bit(i as u64))).unwrap())
            .collect()
    }

    fn enforce_point(gadget: &P384Var, expected: Point) {
        let [x, y, z] = coords(expected);
        if z == P384Fq::from(0u64) {
            gadget.z.enforce_equal(&FqVar::zero()).unwrap();
            gadget.x.enforce_equal(&FqVar::zero()).unwrap();
            gadget.y.enforce_not_equal(&FqVar::zero()).unwrap();
        } else {
            gadget.z.enforce_not_equal(&FqVar::zero()).unwrap();
            gadget.x.enforce_equal(&(&gadget.z * x)).unwrap();
            gadget.y.enforce_equal(&(&gadget.z * y)).unwrap();
        }
    }

    #[test]
    fn complete_addition_handles_identity_doubling_and_inverse() {
        let h = blinding_generator();
        let cases = [
            (Point::IDENTITY, Point::IDENTITY),
            (Point::IDENTITY, Point::GENERATOR),
            (Point::GENERATOR, Point::IDENTITY),
            (Point::GENERATOR, Point::GENERATOR),
            (Point::GENERATOR, -Point::GENERATOR),
            (h, -h),
            (Point::GENERATOR, h),
        ];
        for (p, q) in cases {
            let cs = ConstraintSystem::<Fr>::new_ref();
            let allocate = |point| {
                let [x, y, z] = coords(point);
                P384Var {
                    x: FqVar::new_witness(cs.clone(), || Ok(x)).unwrap(),
                    y: FqVar::new_witness(cs.clone(), || Ok(y)).unwrap(),
                    z: FqVar::new_witness(cs.clone(), || Ok(z)).unwrap(),
                }
            };
            let sum = allocate(p).add_complete(&allocate(q));
            enforce_point(&sum, p + q);
            assert!(cs.is_satisfied().unwrap());
        }
    }

    #[test]
    fn scalar_windows_match_native_zero_one_and_randoms() {
        let mut rng = ChaCha20Rng::from_seed([41; 32]);
        let mut cases = vec![(0u64, 0u64), (1, 0), (0, 1), (1, 1), (255, 127)];
        for _ in 0..3 {
            cases.push((rng.next_u64(), rng.next_u64()));
        }
        for (a, r) in cases {
            let cs = ConstraintSystem::<Fr>::new_ref();
            let a_value = BigUint::from(a);
            let r_value = BigUint::from(r);
            let gadget = pedersen(
                &bits(cs.clone(), &a_value, 64),
                &bits(cs.clone(), &r_value, 64),
            )
            .unwrap();
            let expected = Point::GENERATOR * scalar(&a_value).unwrap()
                + blinding_generator() * scalar(&r_value).unwrap();
            enforce_point(&gadget, expected);
            assert!(cs.is_satisfied().unwrap());
        }
    }

    #[test]
    fn wrong_public_point_is_unsatisfied() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let gadget = pedersen(
            &bits(cs.clone(), &BigUint::from(12u64), 8),
            &bits(cs.clone(), &BigUint::from(43u64), 8),
        )
        .unwrap();
        enforce_point(&gadget, Point::GENERATOR);
        assert!(!cs.is_satisfied().unwrap());
    }

    #[test]
    fn full_width_matches_native_and_reports_constraints() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let a = (BigUint::from(1u64) << 256usize) - BigUint::from(1u64);
        let r = BigUint::parse_bytes(b"ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52972", 16).unwrap();
        let start = std::time::Instant::now();
        let gadget = pedersen(&bits(cs.clone(), &a, 256), &bits(cs.clone(), &r, 384)).unwrap();
        enforce_point(
            &gadget,
            Point::GENERATOR * scalar(&a).unwrap() + blinding_generator() * scalar(&r).unwrap(),
        );
        assert!(cs.is_satisfied().unwrap());
        eprintln!(
            "P-384 full width: {} constraints, {:.3}s synthesis+check",
            cs.num_constraints(),
            start.elapsed().as_secs_f64()
        );
        let _ = gadget.z.value().unwrap().inverse().unwrap();
    }

    #[test]
    fn h_matches_vss_fixture_and_native_rejects_noncanonical_scalar() {
        assert_eq!(
            hex::encode(blinding_generator().to_affine().to_encoded_point(true).as_bytes()),
            "0338678c78c59cf7b985c6cf9c2980028848e48c90a6e47399ac4b488350fd9cdd319a8526d7871792072fa67827813f23"
        );
        let q = BigUint::parse_bytes(b"ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973", 16).unwrap();
        assert!(native_commit(&BigUint::from(1u64), &q).is_err());
        assert!(native_commit(&BigUint::from(0u64), &BigUint::from(0u64)).is_err());
    }
}
