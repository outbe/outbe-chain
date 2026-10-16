use radicle_crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

fn main() {
    let path = std::env::args().nth(1).expect("public vectors path");
    let row: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        row["scenario"],
        "interactive-dkg-then-membership-reshare-repository-signing"
    );
    let owner = BlsPublicKey::from_bytes(
        hex::decode(row["owner_public"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    for (name, context) in [("historical", 7), ("current", 8)] {
        let actual = RepositorySubject::cob(
            [1; 20],
            [context; 32],
            [3; 20],
            b"native Review Accept bytes",
        );
        assert_eq!(
            hex::encode(actual.as_bytes()),
            row[format!("{name}_subject")].as_str().unwrap()
        );
        let envelope =
            BlsEnvelope::from_hex(row[format!("{name}_envelope")].as_str().unwrap()).unwrap();
        envelope.verify(&actual, owner.into()).unwrap();
        let wrong_context =
            RepositorySubject::cob([1; 20], [99; 32], [3; 20], b"native Review Accept bytes");
        assert!(envelope.verify(&wrong_context, owner.into()).is_err());
        let wrong_payload =
            RepositorySubject::cob([1; 20], [context; 32], [3; 20], b"another operation");
        assert!(envelope.verify(&wrong_payload, owner.into()).is_err());
        let mut tampered = envelope.to_bytes();
        tampered[268] ^= 1;
        if let Ok(envelope) = BlsEnvelope::from_bytes(&tampered) {
            assert!(envelope.verify(&actual, owner.into()).is_err());
        }
    }
    println!("PASS: fresh native verifier accepts historical/current envelopes and rejects wrong context, changed payload and tampered signatures; no Share or Commonware recovery/provider API used");
}
