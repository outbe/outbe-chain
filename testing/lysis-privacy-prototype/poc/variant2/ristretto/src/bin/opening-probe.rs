use ark_bn254::Fr;
use ark_relations::r1cs::ConstraintSystem;
use curve25519_dalek::scalar::Scalar;
use outbe_ristretto_lifecycle_poc::edwards_gadget::{fixed_mul, opening, Point, PointVar};
fn main() {
    let bits: usize = std::env::args()
        .nth(1)
        .unwrap_or("104".into())
        .parse()
        .unwrap();
    let m = (num_bigint::BigUint::from(1u8) << (bits - 1)) + 13u32;
    let partial = std::env::args().any(|a| a == "partial");
    let r = if partial {
        Scalar::from(12345u64)
    } else {
        Scalar::random(&mut rand::rngs::OsRng)
    };
    let mut mb = [0; 32];
    let raw = m.to_bytes_le();
    mb[..raw.len()].copy_from_slice(&raw);
    let c = bulletproofs::PedersenGens::default()
        .commit(Scalar::from_bytes_mod_order(mb), r)
        .compress()
        .to_bytes();
    let cs = ConstraintSystem::<Fr>::new_ref();
    let start = std::time::Instant::now();
    if partial {
        let pc = bulletproofs::PedersenGens::default();
        let bs = outbe_p_link_measurements::integer::bits(cs.clone(), &m, bits).unwrap();
        let rb = outbe_p_link_measurements::integer::bits(
            cs.clone(),
            &num_bigint::BigUint::from_bytes_le(&r.to_bytes()),
            bits,
        )
        .unwrap();
        fixed_mul(Point::decode(pc.B.compress().to_bytes()).unwrap(), &bs)
            .unwrap()
            .add(
                &fixed_mul(
                    Point::decode(pc.B_blinding.compress().to_bytes()).unwrap(),
                    &rb,
                )
                .unwrap(),
            )
            .unwrap()
            .enforce_same(&PointVar::constant(Point::decode(c).unwrap()))
            .unwrap();
    } else {
        opening(
            cs.clone(),
            &m,
            &num_bigint::BigUint::from_bytes_le(&r.to_bytes()),
            bits,
            c,
        )
        .unwrap();
    }
    println!(
        "{}",
        serde_json::json!({"bits":bits,"constraints":cs.num_constraints(),"synthesis_seconds":start.elapsed().as_secs_f64(),"satisfied":cs.is_satisfied().unwrap()})
    );
}
