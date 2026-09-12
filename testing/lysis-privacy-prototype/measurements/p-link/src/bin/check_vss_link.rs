//! Check the saved real P_link proof against the exact existing VSS component.
//! This is a local public-fixture test, not production admission or storage.
use ark_bn254::{Bn254, Fr};
use ark_groth16::{prepare_verifying_key, Groth16, Proof, VerifyingKey};
use ark_serialize::CanonicalDeserialize;
use outbe_p_link_measurements::circuit;
use std::path::Path;

#[allow(dead_code)]
mod existing_vss {
    // Reuse the measured component without modifying it or duplicating its math.
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../wide-vss/src/main.rs"
    ));

    pub fn check(a: &BigUint, r: &BigUint, admitted_c: &[u8; 49]) {
        let h = NistP384::hash_from_bytes::<ExpandMsgXmd<Sha384>>(
            &[b"Pedersen blinding generator for Outbe aggregate research; not production parameters"],
            &[DST],
        ).unwrap();
        let secret = [scalar(a), scalar(r)];
        let mut rng = ChaCha20Rng::from_seed([91; 32]); // PUBLIC fixture only.
        let d = deal(h, secret, 16, 6, [21; 32], &mut rng);
        assert_eq!(
            d.coeff[0].to_affine().to_encoded_point(true).as_bytes(),
            admitted_c
        );
        for j in 0..16 {
            assert!(valid_share(h, &d, j, d.shares[j], [21; 32]));
        }
        // VSS consistency alone accepts a different nominal; linkage rejects it.
        let wrong = deal(
            h,
            [secret[0] + Scalar::ONE, secret[1]],
            16,
            6,
            [21; 32],
            &mut rng,
        );
        assert!(valid_share(h, &wrong, 0, wrong.shares[0], [21; 32]));
        assert_ne!(
            wrong.coeff[0].to_affine().to_encoded_point(true).as_bytes(),
            admitted_c
        );
        let members = [0, 2, 5, 8, 12, 15];
        let next = reshare(h, &d, &members, 16, 6, [22; 32], &mut rng);
        assert_eq!(
            next.coeff[0].to_affine().to_encoded_point(true).as_bytes(),
            admitted_c
        );
        assert_eq!(open(h, &next, &members, [22; 32]).unwrap(), secret);
    }
}

fn read<T: CanonicalDeserialize>(dir: &Path, name: &str) -> T {
    let bytes = std::fs::read(dir.join(name)).unwrap();
    let mut remaining = &bytes[..];
    let value = T::deserialize_compressed(&mut remaining).unwrap();
    assert!(remaining.is_empty());
    value
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: check_vss_link ARTIFACT_DIR");
    let dir = Path::new(&dir);
    let vk: VerifyingKey<Bn254> = read(dir, "vk.bin");
    let proof: Proof<Bn254> = read(dir, "proof.bin");
    let inputs: Vec<Fr> = read(dir, "public_inputs.bin");
    let c = circuit::fixture();
    assert_eq!(
        inputs,
        c.public.inputs().unwrap(),
        "exact fixture admission context"
    );
    assert!(Groth16::<Bn254>::verify_proof(&prepare_verifying_key(&vk), &proof, &inputs).unwrap());
    existing_vss::check(
        &c.private.nominal,
        &c.private.nominal_blinding,
        &c.public.nominal_commitment,
    );
    let result = serde_json::json!({
        "real_saved_p_link_verified": true,
        "exact_fixture_public_context_checked": true,
        "existing_wide_vss_component_reused": true,
        "same_c_nominal_at_admission_and_after_reshare": true,
        "valid_vss_for_different_nominal_rejected_by_linkage": true,
        "all_16_share_checks_and_6_member_reshare_open_passed": true,
        "scope": "public-fixture component composition only; no L2 proof/root verification, network admission or consensus"
    });
    let json = serde_json::to_string_pretty(&result).unwrap();
    std::fs::write(dir.join("vss_link.json"), &json).unwrap();
    println!("{json}");
}
