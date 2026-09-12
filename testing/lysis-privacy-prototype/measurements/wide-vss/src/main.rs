// Research-only P-384 Pedersen VSS. No input ZK proof or network protocol.
// Deterministic fixture RNG: never use this executable to protect real secrets.
use num_bigint::BigUint;
use num_traits::One;
use p384::{
    elliptic_curve::{
        ff::{Field, PrimeField},
        hash2curve::{ExpandMsgXmd, GroupDigest},
        sec1::ToEncodedPoint,
        Group,
    },
    FieldBytes, NistP384, ProjectivePoint as Point, Scalar,
};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde_json::{json, Value};
use sha2::Sha384;
use std::time::Instant;

type Pair = [Scalar; 2];
const DST: &[u8] = b"OUTBE-RESEARCH-VSS-v1-P384_XMD:SHA-384_SSWU_RO_";
fn integer(s: Scalar) -> BigUint {
    BigUint::from_bytes_be(&s.to_repr())
}
fn scalar(v: &BigUint) -> Scalar {
    let bytes = v.to_bytes_be();
    assert!(bytes.len() <= 48);
    let mut repr = FieldBytes::default();
    repr[48 - bytes.len()..].copy_from_slice(&bytes);
    Option::<Scalar>::from(Scalar::from_repr(repr)).expect("canonical scalar, no modular reduction")
}
fn commit(h: Point, pair: Pair) -> Point {
    Point::GENERATOR * pair[0] + h * pair[1]
}
// Variable-time multiplication of PUBLIC points by PUBLIC member coordinates.
fn public_small_mul(mut p: Point, mut x: u32) -> Point {
    let mut result = Point::IDENTITY;
    while x != 0 {
        if x & 1 != 0 {
            result += p;
        }
        x >>= 1;
        if x != 0 {
            p = p.double();
        }
    }
    result
}
fn eval_points(coeff: &[Point], index: usize) -> Point {
    let x = u32::try_from(index + 1).unwrap();
    coeff
        .iter()
        .rev()
        .fold(Point::IDENTITY, |acc, c| public_small_mul(acc, x) + c)
}
#[derive(Clone)]
struct Deal {
    shares: Vec<Pair>,
    coeff: Vec<Point>,
    context: [u8; 32],
}
fn deal(
    h: Point,
    secret: Pair,
    n: usize,
    t: usize,
    context: [u8; 32],
    rng: &mut ChaCha20Rng,
) -> Deal {
    assert!(t > 0 && t <= n);
    let mut polynomials = vec![secret];
    for _ in 1..t {
        polynomials.push([Scalar::random(&mut *rng), Scalar::random(&mut *rng)]);
    }
    let coeff = polynomials.iter().map(|a| commit(h, *a)).collect();
    let shares = (0..n)
        .map(|j| {
            let x = Scalar::from((j + 1) as u64);
            polynomials
                .iter()
                .rev()
                .fold([Scalar::ZERO; 2], |a, c| [a[0] * x + c[0], a[1] * x + c[1]])
        })
        .collect();
    Deal {
        shares,
        coeff,
        context,
    }
}
fn valid_share(h: Point, d: &Deal, index: usize, pair: Pair, expected_context: [u8; 32]) -> bool {
    d.context == expected_context
        && index < d.shares.len()
        && commit(h, pair) == eval_points(&d.coeff, index)
}
fn aggregate(deals: &[Deal], context: [u8; 32]) -> Deal {
    let mut a = deals[0].clone();
    for d in &deals[1..] {
        assert_eq!(a.shares.len(), d.shares.len());
        assert_eq!(a.coeff.len(), d.coeff.len());
        for (a, b) in a.shares.iter_mut().zip(&d.shares) {
            a[0] += b[0];
            a[1] += b[1];
        }
        for (a, b) in a.coeff.iter_mut().zip(&d.coeff) {
            *a += b;
        }
    }
    // Binding of this context to a canonical admitted-set root is a network protocol obligation.
    a.context = context;
    a
}
fn weights(indices: &[usize]) -> Option<Vec<Scalar>> {
    let mut sorted = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != indices.len() {
        return None;
    }
    Some(
        indices
            .iter()
            .map(|i| {
                let xi = Scalar::from((i + 1) as u64);
                let mut num = Scalar::ONE;
                let mut den = Scalar::ONE;
                for j in indices {
                    if i != j {
                        let xj = Scalar::from((j + 1) as u64);
                        num *= xj;
                        den *= xj - xi;
                    }
                }
                num * Option::<Scalar>::from(den.invert()).unwrap()
            })
            .collect(),
    )
}
fn open(h: Point, d: &Deal, indices: &[usize], expected_context: [u8; 32]) -> Option<Pair> {
    if indices.len() < d.coeff.len() {
        return None;
    }
    let w = weights(indices)?;
    let mut secret = [Scalar::ZERO; 2];
    for (&i, w) in indices.iter().zip(w) {
        let pair = *d.shares.get(i)?;
        if !valid_share(h, d, i, pair, expected_context) {
            return None;
        }
        secret[0] += pair[0] * w;
        secret[1] += pair[1] * w;
    }
    if commit(h, secret) != d.coeff[0] {
        return None;
    }
    Some(secret)
}
fn reshare(
    h: Point,
    old: &Deal,
    indices: &[usize],
    n: usize,
    t: usize,
    context: [u8; 32],
    rng: &mut ChaCha20Rng,
) -> Deal {
    assert!(indices.len() >= old.coeff.len());
    let w = weights(indices).unwrap();
    let mut deals = Vec::new();
    for (&i, lambda) in indices.iter().zip(w) {
        assert!(valid_share(h, old, i, old.shares[i], old.context));
        let secret = [old.shares[i][0] * lambda, old.shares[i][1] * lambda];
        let d = deal(h, secret, n, t, context, rng);
        assert_eq!(d.coeff[0], eval_points(&old.coeff, i) * lambda);
        for j in 0..n {
            assert!(valid_share(h, &d, j, d.shares[j], context));
        }
        deals.push(d);
    }
    let result = aggregate(&deals, context);
    assert_eq!(result.coeff[0], old.coeff[0]);
    result
}
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    (v[(v.len() - 1) / 2] + v[v.len() / 2]) / 2.
}
fn elapsed(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.
}
fn profile(h: Point, n: usize, t: usize, rng: &mut ChaCha20Rng) -> Value {
    let max = (BigUint::one() << 256usize) - BigUint::one();
    let a = scalar(&(&max / BigUint::from(8u32)));
    let b = scalar(&((&max / BigUint::from(8u32)) + BigUint::from(1234u32)));
    let secret = [a, Scalar::random(&mut *rng)];
    let mut generations = Vec::new();
    let mut verifications = Vec::new();
    let mut openings = Vec::new();
    let mut last = None;
    let indices: Vec<_> = (0..n).rev().take(t).collect();
    for sample in 0..5 {
        let start = Instant::now();
        let d = deal(h, secret, n, t, [1; 32], rng);
        let gen = elapsed(start);
        assert_eq!(d.coeff[0], commit(h, secret));
        let start = Instant::now();
        assert!(valid_share(h, &d, n / 2, d.shares[n / 2], [1; 32]));
        let check = elapsed(start);
        let other_blinding = Scalar::random(&mut *rng);
        let e = deal(h, [b, other_blinding], n, t, [2; 32], rng);
        let total = aggregate(&[d.clone(), e.clone()], [3; 32]);
        let start = Instant::now();
        let result = open(h, &total, &indices, [3; 32]).unwrap();
        let op = elapsed(start);
        assert_eq!(integer(result[0]), integer(a) + integer(b));
        assert_eq!(result[1], secret[1] + other_blinding);
        if sample > 0 {
            generations.push(gen);
            verifications.push(check);
            openings.push(op);
        }
        last = Some(total);
    }
    let total = last.unwrap();
    let mut corrupt = total.clone();
    corrupt.shares[indices[0]][0] += Scalar::ONE;
    assert!(open(h, &corrupt, &indices, [3; 32]).is_none());
    assert!(open(h, &total, &indices, [4; 32]).is_none());
    let mut duplicates = indices.clone();
    duplicates[1] = duplicates[0];
    assert!(open(h, &total, &duplicates, [3; 32]).is_none());
    assert!(open(h, &total, &indices[..t - 1], [3; 32]).is_none());
    let opened = open(h, &total, &indices, [3; 32]).unwrap();
    assert_ne!(
        commit(h, [opened[0] + Scalar::ONE, opened[1]]),
        total.coeff[0]
    );
    let mut handoff = Value::Null;
    if n == 16 {
        let start = Instant::now();
        let fresh = reshare(h, &total, &indices, n, t, [9; 32], rng);
        let time = elapsed(start);
        let again = open(h, &fresh, &indices, [9; 32]).unwrap();
        assert_eq!(again, opened);
        // Old-epoch shares must not be accepted as fresh-epoch shares.
        let stale_in_new = valid_share(h, &fresh, 0, total.shares[0], [9; 32]);
        assert!(!stale_in_new);
        handoff = json!({"all_old_dealers_all_new_recipients_sequential_ms":time,"private_share_bytes":t*n*96,"public_coefficient_bytes":t*t*49,"old_epoch_share_rejected":true});
    }
    let f = (n - 1) / 3;
    json!({"n":n,"threshold":t,"assumed_max_byzantine_f":f,
      "own_share_acknowledgments_needed_without_repair":t+f,
      "deal_generation_all_recipients_median_ms":median(&mut generations),
      "verify_one_recipient_median_ms":median(&mut verifications),
      "open_including_each_contribution_verification_median_ms":median(&mut openings),
      "retained_samples":4,"scalar_bytes":48,"compressed_commitment_bytes":49,
      "private_share_pair_bytes":96,"all_recipient_private_bytes":96*n,
      "public_coefficients_bytes":49*t,"extra_coefficients_excluding_existing_nominal_commitment_bytes":49*(t-1),
      "one_aggregate_private_state_per_member_bytes":96,"opening_raw_share_pairs_bytes":96*t,
      "handoff":handoff,"negative_checks_passed":["bad share","wrong expected context","duplicate index","insufficient contributions","wrong sum"]})
}
fn main() {
    let h = NistP384::hash_from_bytes::<ExpandMsgXmd<Sha384>>(
        &[b"Pedersen blinding generator for Outbe aggregate research; not production parameters"],
        &[DST],
    )
    .unwrap();
    assert_ne!(h, Point::IDENTITY);
    assert_ne!(h, Point::GENERATOR);
    let q = integer(-Scalar::ONE) + BigUint::one();
    let max = (BigUint::one() << 256usize) - BigUint::one();
    let worst = &max * BigUint::from(1_000_000_000u64);
    assert!(q > worst);
    assert_eq!(integer(scalar(&max)), max);
    assert_eq!(integer(scalar(&worst)), worst);
    let mut rng = ChaCha20Rng::from_seed([38u8; 32]);
    let mut profiles = Vec::new();
    for (n, t) in [(16, 6), (16, 11), (128, 43)] {
        profiles.push(profile(h, n, t, &mut rng));
        eprintln!("wide VSS {n}/{t} passed");
    }
    // Integer/field capacity stress, not one billion independent deals or a throughput test.
    let seed_deal = deal(
        h,
        [scalar(&max), Scalar::random(&mut rng)],
        16,
        6,
        [5; 32],
        &mut rng,
    );
    let k = Scalar::from(1_000_000_000u64);
    let scaled = Deal {
        shares: seed_deal
            .shares
            .iter()
            .map(|p| [p[0] * k, p[1] * k])
            .collect(),
        coeff: seed_deal.coeff.iter().map(|p| *p * k).collect(),
        context: [6; 32],
    };
    let opened = open(h, &scaled, &[0, 2, 5, 7, 11, 15], [6; 32]).unwrap();
    assert_eq!(integer(opened[0]), worst);
    println!("{}",serde_json::to_string_pretty(&json!({
      "kind":"wide_field_vss_component_only_no_admission_zk_or_network",
      "curve":"NIST P-384","scalar_order_hex":q.to_str_radix(16),"scalar_order_bits":q.bits(),
      "blinding_generator_sec1_hex":h.to_affine().to_encoded_point(true).as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),
      "hash_to_curve_dst":String::from_utf8_lossy(DST),"library":"RustCrypto p384 0.13.1",
      "fixed_seed_fixture_only":true,"single_process_holds_all_simulated_parties":true,
      "public_numeric_aggregate":"one S, plus random aggregate blinding R; no limb totals",
      "integer_capacity":{"max_uint256_roundtrip":true,"billion_times_uint256_max_exact":true,
         "worst_sum_bits":worst.bits(),"q_exceeds_worst_sum":true,
         "uint256_total_policy_would_reject_worst_sum":worst>max},
      "profiles":profiles,
      "not_implemented":["source/range proof for P384 commitment","Ristretto to P384 equality bridge",
        "authentication/signatures or network receipts","consensus agreement on admitted input root",
        "malicious-network DKG/reshare liveness","proactive erasure","late-grouping data transfer",
        "private PayNote/Fidelity","exact-claim proof on this group"]
    })).unwrap());
}
