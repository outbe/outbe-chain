//! Foreign-field Ristretto opening for source proofs; no Baby-Jubjub points.
//! Public compressed points are decoded canonically by the verifier. Internal
//! Edwards representatives are compared using RFC 9496 quotient equality.
use ark_bn254::Fr;
use ark_ff::{AdditiveGroup, BigInteger, Field, Fp256, MontBackend, MontConfig, PrimeField};
use ark_r1cs_std::{
    alloc::AllocVar,
    boolean::Boolean,
    eq::EqGadget,
    fields::{emulated_fp::EmulatedFpVar, FieldVar},
    select::CondSelectGadget,
};
use ark_relations::r1cs::{ConstraintSystemRef, SynthesisError};
use curve25519_dalek::ristretto::CompressedRistretto;

#[derive(MontConfig)]
#[modulus = "57896044618658097711785492504343953926634992332820282019728792003956564819949"]
#[generator = "2"]
pub struct FqConfig;
pub type Fq = Fp256<MontBackend<FqConfig, 4>>;
pub type FV = EmulatedFpVar<Fq, Fr>;

pub fn d() -> Fq {
    -Fq::from(121665u64) / Fq::from(121666u64)
}
fn abs(x: Fq) -> Fq {
    if x.into_bigint().is_odd() {
        -x
    } else {
        x
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: Fq,
    pub y: Fq,
}
impl Point {
    pub fn zero() -> Self {
        Self {
            x: Fq::ZERO,
            y: Fq::ONE,
        }
    }
    pub fn decode(bytes: [u8; 32]) -> Result<Self, String> {
        if CompressedRistretto(bytes).decompress().is_none() {
            return Err("invalid Ristretto".into());
        }
        let s = Fq::from_le_bytes_mod_order(&bytes);
        let ss = s.square();
        let u1 = Fq::ONE - ss;
        let u2 = Fq::ONE + ss;
        let v = -d() * u1.square() - u2.square();
        let inv = (v * u2.square()).inverse().ok_or("Ristretto inverse")?;
        let invsqrt = abs(inv.sqrt().ok_or("Ristretto square root")?);
        let dx = invsqrt * u2;
        let dy = invsqrt * dx * v;
        Ok(Self {
            x: abs(Fq::from(2u64) * s * dx),
            y: u1 * dy,
        })
    }
    pub fn add(self, other: Self) -> Self {
        let xx = self.x * other.x;
        let yy = self.y * other.y;
        let t = d() * xx * yy;
        Self {
            x: (self.x * other.y + self.y * other.x) / (Fq::ONE + t),
            y: (yy + xx) / (Fq::ONE - t),
        }
    }
    pub fn same_ristretto(self, other: Self) -> bool {
        self.x * other.y == self.y * other.x || self.y * other.y == self.x * other.x
    }
}

#[derive(Clone)]
pub struct PointVar {
    pub x: FV,
    pub y: FV,
}
impl PointVar {
    pub fn constant(p: Point) -> Self {
        Self {
            x: FV::constant(p.x),
            y: FV::constant(p.y),
        }
    }
    pub fn add(&self, other: &Self) -> Result<Self, SynthesisError> {
        let xx = &self.x * &other.x;
        let yy = &self.y * &other.y;
        let t = xx.clone() * yy.clone() * d();
        // Complete a=-1 Edwards formula. Constrain quotients with mul_equals
        // rather than proving an inverse. Denominators are nonzero for valid
        // points on this complete curve; the accumulator starts at identity.
        let xn = &self.x * &other.y + &self.y * &other.x;
        let yn = yy + xx;
        let xd = t.clone() + Fq::ONE;
        let yd = FV::constant(Fq::ONE) - t;
        let cs = self.x.cs().or(other.x.cs());
        use ark_r1cs_std::R1CSVar;
        let x = FV::new_witness(cs.clone(), || Ok(xn.value()? / xd.value()?))?;
        let y = FV::new_witness(cs, || Ok(yn.value()? / yd.value()?))?;
        x.mul_equals(&xd, &xn)?;
        y.mul_equals(&yd, &yn)?;
        Ok(Self { x, y })
    }
    pub fn select(bit: &Boolean<Fr>, a: &Self, b: &Self) -> Result<Self, SynthesisError> {
        Ok(Self {
            x: FV::conditionally_select(bit, &a.x, &b.x)?,
            y: FV::conditionally_select(bit, &a.y, &b.y)?,
        })
    }
    pub fn enforce_same(&self, other: &Self) -> Result<(), SynthesisError> {
        let a = (&self.x * &other.y).is_eq(&(&self.y * &other.x))?;
        let b = (&self.y * &other.y).is_eq(&(&self.x * &other.x))?;
        (&a | &b).enforce_equal(&Boolean::TRUE)
    }
}
pub fn fixed_mul(base: Point, bits: &[Boolean<Fr>]) -> Result<PointVar, SynthesisError> {
    let mut acc = PointVar::constant(Point::zero());
    let mut b = base;
    for chunk in bits.chunks(4) {
        let mut native = Point::zero();
        let mut table = vec![];
        for _ in 0..(1 << chunk.len()) {
            table.push(PointVar::constant(native));
            native = native.add(b);
        }
        for bit in chunk {
            table = table
                .chunks(2)
                .map(|v| PointVar::select(bit, &v[1], &v[0]))
                .collect::<Result<Vec<_>, _>>()?;
        }
        acc = acc.add(&table[0])?;
        for _ in 0..4 {
            b = b.add(b);
        }
    }
    Ok(acc)
}
pub fn opening(
    cs: ConstraintSystemRef<Fr>,
    m: &num_bigint::BigUint,
    r: &num_bigint::BigUint,
    bits: usize,
    c: [u8; 32],
) -> Result<(), SynthesisError> {
    use bulletproofs::PedersenGens;
    use outbe_p_link_measurements::integer::{bits as alloc_bits, less_constant};
    let pc = PedersenGens::default();
    let mb = alloc_bits(cs.clone(), m, bits)?;
    let rb = alloc_bits(cs, r, 253)?;
    let order = (num_bigint::BigUint::from(1u8) << 252usize)
        + num_bigint::BigUint::parse_bytes(b"27742317777372353535851937790883648493", 10).unwrap();
    less_constant(&rb, &order)?.enforce_equal(&Boolean::TRUE)?;
    let a = fixed_mul(Point::decode(pc.B.compress().to_bytes()).unwrap(), &mb)?;
    let b = fixed_mul(
        Point::decode(pc.B_blinding.compress().to_bytes()).unwrap(),
        &rb,
    )?;
    a.add(&b)?.enforce_same(&PointVar::constant(
        Point::decode(c).map_err(|_| SynthesisError::Unsatisfiable)?,
    ))
}
