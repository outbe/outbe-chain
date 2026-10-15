//! Primitive feasibility check, not a production DKG or authorization implementation.
use commonware_codec::Encode;
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::deal_anonymous,
    primitives::{
        ops::{self, threshold},
        sharing::Mode,
        variant::MinSig,
    },
};
use commonware_parallel::Sequential;
use commonware_utils::{Faults, N3f1, TestRng, NZU32};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    // Anonymous deterministic test shares prove only the primitive's capability.
    // Production requires a distributed ceremony and authenticated output.
    let (old, shares) =
        deal_anonymous::<MinSig, N3f1>(TestRng::new(42), Mode::NonZeroCounter, NZU32!(4));
    let (new, _) =
        deal_anonymous::<MinSig, N3f1>(TestRng::new(99), Mode::NonZeroCounter, NZU32!(4));
    let namespace = b"radicle.repository.bls.v1";
    let subject = b"p0-native-subject-fixture";
    assert_eq!(N3f1::quorum(4), 3);
    assert_eq!(old.required(), 3);
    let partials: Vec<_> = shares
        .iter()
        .map(|share| threshold::sign_message::<MinSig>(share, namespace, subject))
        .collect();
    for partial in &partials {
        threshold::verify_message::<MinSig>(&old, namespace, subject, partial).unwrap();
        assert!(threshold::verify_message::<MinSig>(&new, namespace, subject, partial).is_err());
    }
    assert!(threshold::recover::<MinSig, _>(&old, &partials[..2], &Sequential).is_err());
    let duplicates = [
        partials[0].clone(),
        partials[0].clone(),
        partials[1].clone(),
    ];
    assert!(threshold::recover::<MinSig, _>(&old, &duplicates, &Sequential).is_err());
    let mut wrong_index = partials[0].clone();
    wrong_index.index = partials[1].index;
    assert!(threshold::verify_message::<MinSig>(&old, namespace, subject, &wrong_index).is_err());
    let signature = threshold::recover::<MinSig, _>(&old, &partials[..3], &Sequential).unwrap();
    for omitted in 0..4 {
        let subset: Vec<_> = partials
            .iter()
            .enumerate()
            .filter_map(|(i, p)| (i != omitted).then_some(p))
            .collect();
        assert_eq!(
            signature,
            threshold::recover::<MinSig, _>(&old, subset, &Sequential).unwrap()
        );
    }
    ops::verify_message::<MinSig>(old.public(), namespace, subject, &signature).unwrap();
    assert!(ops::verify_message::<MinSig>(new.public(), namespace, subject, &signature).is_err());
    assert!(
        ops::verify_message::<MinSig>(old.public(), b"wrong-domain", subject, &signature).is_err()
    );
    assert!(
        ops::verify_message::<MinSig>(old.public(), namespace, b"another-subject", &signature)
            .is_err()
    );

    // Mere metadata/participant-list changes cannot invalidate this old signature.
    ops::verify_message::<MinSig>(old.public(), namespace, subject, &signature).unwrap();
    assert_ne!(old.public(), new.public());
    println!("PASS: 3-of-4, four subsets, t-1, duplicates, wrong index, fresh-key rejection, domain/subject isolation");
    println!("OBSERVED: old signature still verifies under old key; current-authority policy remains necessary");
    println!("public_key={}", hex(&old.public().encode()));
    println!("new_public_key={}", hex(&new.public().encode()));
    println!("signature={}", hex(&signature.encode()));
    assert_eq!(old.public().encode().len(), 96);
    assert_eq!(signature.encode().len(), 48);
}
