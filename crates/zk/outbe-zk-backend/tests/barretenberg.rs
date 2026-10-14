//! Real barretenberg prove→verify round-trips.
//!
//! Generates actual UltraHonkKeccak proofs from genuine witnesses and verifies
//! them through the FFI backend — the full on-device path. Run with
//! `cargo test -p outbe-zk-backend --test barretenberg`.

#![allow(clippy::print_stdout)] // reports contention measurements from the real native session

use ark_ff::{BigInteger, PrimeField, UniformRand};

use outbe_zk::demo_tribute::{self, demo_tribute_domain, DemoTributeProvable};
use outbe_zk::error::Error;
use outbe_zk::noir::demo_tribute::DemoTribute;
use outbe_zk::primitive::{hash, signature};
use outbe_zk::protocol::entity::{Entity, Owned};
use outbe_zk::protocol::imt::Imt;
use outbe_zk::protocol::key::{NftSecret, Signer};
use outbe_zk::protocol::zk::{CircuitId, ProofGenerator, ProofVerifier};
use outbe_zk::Fr;
use outbe_zk::INCLUSION_DEPTH;
use outbe_zk_backend::barretenberg::{Barretenberg, RawVerifier};

struct TestNft {
    id: Fr,
    owner: Fr,
    fields: Vec<Fr>,
}

impl Entity for TestNft {
    fn id_seed(&self) -> Result<Fr, Error> {
        Ok(self.id)
    }
    fn encode_id_body(&self, _out: &mut Vec<Fr>) -> Result<(), Error> {
        Ok(())
    }
    fn encode_body(&self, out: &mut Vec<Fr>) -> Result<(), Error> {
        out.extend_from_slice(&self.fields);
        Ok(())
    }
}
impl Owned for TestNft {
    fn owner(&self) -> Result<Fr, Error> {
        Ok(self.owner)
    }
}

#[test]
fn demo_tribute_prove_verify_round_trip() {
    let mut rng = ark_std::test_rng();
    let (sk, pk) = signature::keypair(&mut rng);
    let nonce = Fr::rand(&mut rng);
    let owner = hash::derive_owner(&pk, nonce).unwrap();
    let binding = hash::binding(&[3u8; 20], &[4u8; 32], 99, 0xdead).unwrap();
    let td = TestNft {
        id: owner,
        owner,
        fields: vec![Fr::from(978u64), Fr::from(100u64)],
    };
    let signer = Signer::from_secret(NftSecret::new(sk), nonce).unwrap();
    let tree = Imt::new(demo_tribute_domain(), Fr::from(0u64), INCLUSION_DEPTH).unwrap();
    let path = tree.empty_inclusion_path(0);
    let (witness, public) = td
        .derive_demo_tribute_witness(&mut rng, &signer, binding, &path)
        .unwrap();

    let input = [Fr::from(1u64), Fr::from(2u64)];
    let expected = hash::poseidon2(&input).unwrap();
    let hash_worker = || {
        let mut maximum = std::time::Duration::ZERO;
        let mut gate_wait = std::time::Duration::ZERO;
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            let waiting = std::time::Instant::now();
            let waited = outbe_barretenberg::with_api(|_| {
                Ok::<_, outbe_barretenberg::RuntimeError>(waiting.elapsed())
            })
            .unwrap();
            gate_wait = gate_wait.max(waited);
            let start = std::time::Instant::now();
            assert_eq!(hash::poseidon2(&input).unwrap(), expected);
            maximum = maximum.max(start.elapsed());
        }
        (started.elapsed(), maximum, gate_wait)
    };
    let proof = std::thread::scope(|scope| {
        let hashes = scope.spawn(hash_worker);
        let start = std::time::Instant::now();
        let proof =
            ProofGenerator::<DemoTribute>::generate(&Barretenberg::default(), &witness, &public)
                .expect("bb prove");
        let elapsed = start.elapsed();
        let (hash_elapsed, maximum, gate_wait) = hashes.join().unwrap();
        println!("concurrent prove: {elapsed:?}; 10000 hashes {hash_elapsed:?}; max hash latency {maximum:?}; max gate acquisition {gate_wait:?}");
        proof
    });
    std::thread::scope(|scope| {
        let hashes = scope.spawn(hash_worker);
        let start = std::time::Instant::now();
        assert!(
            ProofVerifier::<DemoTribute>::verify(&Barretenberg::default(), &public, &proof)
                .unwrap()
        );
        let elapsed = start.elapsed();
        let (hash_elapsed, maximum, gate_wait) = hashes.join().unwrap();
        println!("concurrent verify: {elapsed:?}; 10000 hashes {hash_elapsed:?}; max hash latency {maximum:?}; max gate acquisition {gate_wait:?}");
    });

    // A backend-rejected proof must release the gate and leave hashing usable.
    let mut truncated = proof.clone();
    truncated.proof.pop();
    let invalid =
        ProofVerifier::<DemoTribute>::verify(&Barretenberg::default(), &public, &truncated);
    match invalid {
        Ok(verified) => assert!(!verified),
        Err(error) => assert!(!error.is_infrastructure()),
    }
    assert_eq!(hash::poseidon2(&input).unwrap(), expected);

    // User-controlled proof words with the same valid, trusted registry VK.
    // Never classify a malformed proof as a local infrastructure failure.
    let mut infrastructure_failures = Vec::new();
    for (label, word, all_words) in [
        ("all-ff first word", vec![255; 32], false),
        ("all-ff every word", vec![255; 32], true),
        (
            "scalar modulus first word",
            Fr::MODULUS.to_bytes_be(),
            false,
        ),
        (
            "canonical p-1 first word",
            (-Fr::from(1u64)).into_bigint().to_bytes_be(),
            false,
        ),
        (
            "canonical p-1 every word",
            (-Fr::from(1u64)).into_bigint().to_bytes_be(),
            true,
        ),
        ("zero every word", vec![0; 32], true),
    ] {
        let mut malformed = proof.clone();
        if all_words {
            malformed.proof.fill(word);
        } else {
            malformed.proof[0] = word;
        }
        let typed =
            ProofVerifier::<DemoTribute>::verify(&Barretenberg::default(), &public, &malformed);
        println!("malformed {label}, typed: {typed:?}");
        match typed {
            Ok(verified) => assert!(!verified, "{label}"),
            Err(Error::CryptoBackend(error)) => {
                infrastructure_failures.push(format!("{label}, typed: {error}"))
            }
            Err(error) => assert!(matches!(error, Error::Proof(_)), "{label}: {error:?}"),
        }
        let blob = demo_tribute::encode_combined_proof(public.clone(), malformed.proof).unwrap();
        let raw = Barretenberg::default().verify_combined(DemoTribute::VK_BYTES, &blob);
        println!("malformed {label}, raw: {raw:?}");
        match raw {
            Ok(verified) => assert!(!verified, "{label}"),
            Err(Error::CryptoBackend(error)) => {
                infrastructure_failures.push(format!("{label}, raw: {error}"))
            }
            Err(error) => assert!(matches!(error, Error::Proof(_)), "{label}: {error:?}"),
        }
    }

    for len in [0, 1, 31, 33] {
        let mut malformed = proof.clone();
        malformed.proof[0] = vec![0; len];
        let error =
            ProofVerifier::<DemoTribute>::verify(&Barretenberg::default(), &public, &malformed)
                .unwrap_err();
        assert!(
            matches!(error, Error::Proof(_)),
            "word length {len}: {error:?}"
        );
    }

    let combined =
        demo_tribute::encode_combined_proof(public.clone(), proof.proof.clone()).unwrap();
    for removed in [1, 32] {
        let blob = &combined[..combined.len() - removed];
        match Barretenberg::default().verify_combined(DemoTribute::VK_BYTES, blob) {
            Ok(verified) => assert!(!verified),
            Err(error) => assert!(
                matches!(error, Error::Proof(_)),
                "removed {removed}: {error:?}"
            ),
        }
    }
    let mut malformed_public = combined.clone();
    malformed_public[4..36].fill(255);
    assert!(matches!(
        Barretenberg::default().verify_combined(DemoTribute::VK_BYTES, &malformed_public),
        Err(Error::Proof(_))
    ));
    assert!(Barretenberg::default()
        .verify_combined(DemoTribute::VK_BYTES, &combined)
        .unwrap());
    assert_eq!(hash::poseidon2(&input).unwrap(), expected);
    assert_eq!(combined.len(), demo_tribute::COMBINED_LEN);
    let decoded = demo_tribute::decode_public_inputs(&combined).unwrap();
    assert_eq!(decoded.derived_owner, owner);
    assert_eq!(decoded.binding_hash, binding);
    assert!(
        infrastructure_failures.is_empty(),
        "malformed user proof reached infrastructure channel: {infrastructure_failures:?}"
    );
}
