//! Executable design model. Not the production registry, finality source, or fetch gate.
use commonware_cryptography::{bls12381, sha256::Sha256, Hasher, Signer, Verifier};
use commonware_utils::{Faults, N3f1};
use std::collections::BTreeMap;

const DOMAIN: &[u8] = b"outbe.radicle.publication.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Body {
    chain: u64,
    genesis: [u8; 32],
    repo: [u8; 20],
    epoch: u64,
    sequence: u64,
    previous: [u8; 32],
    manifest: [u8; 32],
    anchor: (u64, [u8; 32]),
}

impl Body {
    // Reduced model of the final design body; omitted fields are not claimed tested.
    fn encode(&self) -> Vec<u8> {
        [
            b"OPB1".as_slice(),
            &self.chain.to_be_bytes(),
            &self.genesis,
            &self.repo,
            &self.epoch.to_be_bytes(),
            &self.sequence.to_be_bytes(),
            &self.previous,
            &self.manifest,
            &self.anchor.0.to_be_bytes(),
            &self.anchor.1,
        ]
        .concat()
    }
}

struct Model {
    epoch: u64,
    members: Vec<bls12381::PublicKey>,
    sequence: u64,
    previous: [u8; 32],
    slots: Vec<Option<(u64, u64, [u8; 32])>>,
    accepted: BTreeMap<[u8; 32], [u8; 32]>,
}

impl Model {
    fn new(epoch: u64, members: Vec<bls12381::PublicKey>) -> Self {
        assert!(!members.is_empty() && members.len() <= 256);
        Self {
            epoch,
            slots: vec![None; members.len()],
            members,
            sequence: 0,
            previous: [0; 32],
            accepted: BTreeMap::new(),
        }
    }

    fn attest(&mut self, body: &Body, index: usize, sig: &bls12381::Signature) -> bool {
        if body.chain != 2026
            || body.genesis != [7; 32]
            || body.repo != [8; 20]
            || body.epoch != self.epoch
            || body.sequence != self.sequence + 1
            || body.previous != self.previous
            || index >= self.members.len()
            || !self.members[index].verify(DOMAIN, &body.encode(), sig)
        {
            return false;
        }
        let hash = Sha256::hash(&[&body.encode()]).0;
        let tuple = (body.epoch, body.sequence, hash);
        if self.slots[index]
            .is_some_and(|(epoch, seq, _)| epoch == body.epoch && seq == body.sequence)
        {
            return false;
        }
        self.slots[index] = Some(tuple);
        if self
            .slots
            .iter()
            .filter(|slot| **slot == Some(tuple))
            .count()
            >= N3f1::quorum(self.members.len()) as usize
        {
            self.accepted.insert(hash, body.manifest);
            self.previous = hash;
            self.sequence = body.sequence;
        }
        true
    }

    fn authorize(&self, receipt: [u8; 32], package: &[u8]) -> bool {
        self.accepted.get(&receipt) == Some(&Sha256::hash(&[package]).0)
    }

    fn activate(&mut self, epoch: u64, keys: &[bls12381::PrivateKey]) {
        assert!(epoch > self.epoch);
        self.epoch = epoch;
        self.members = keys.iter().map(Signer::public_key).collect();
        self.slots.resize(keys.len(), None);
    }
}

pub fn check(package: &[u8]) {
    let old: Vec<_> = (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    let current: Vec<_> = (3..=6).map(bls12381::PrivateKey::from_seed).collect();
    let mut model = Model::new(2, current.iter().map(Signer::public_key).collect());
    let body = Body {
        chain: 2026,
        genesis: [7; 32],
        repo: [8; 20],
        epoch: 2,
        sequence: 1,
        previous: [0; 32],
        manifest: Sha256::hash(&[package]).0,
        anchor: (10, [10; 32]),
    };
    let receipt = Sha256::hash(&[&body.encode()]).0;
    assert!(!model.authorize(receipt, package));
    // An old owner signature is mathematically valid, but retired identity keys
    // cannot manufacture a publication receipt under the current committee.
    assert!(!model.attest(&body, 0, &old[0].sign(DOMAIN, &body.encode())));
    assert!(model.attest(&body, 0, &current[0].sign(DOMAIN, &body.encode())));
    assert!(!model.attest(&body, 0, &current[0].sign(DOMAIN, &body.encode())));
    let mut changed = body.clone();
    changed.manifest[0] ^= 1;
    assert!(!model.attest(&changed, 1, &current[1].sign(DOMAIN, &body.encode())));
    changed = body.clone();
    changed.chain += 1;
    assert!(!model.attest(&changed, 1, &current[1].sign(DOMAIN, &changed.encode())));
    assert!(model.attest(&body, 1, &current[1].sign(DOMAIN, &body.encode())));
    assert!(!model.authorize(receipt, package)); // t-1 cannot authorize.
    assert!(model.attest(&body, 2, &current[2].sign(DOMAIN, &body.encode())));
    assert!(model.authorize(receipt, package));
    assert!(!model.authorize(receipt, b"different native package"));
    let next: Vec<_> = (5..=8).map(bls12381::PrivateKey::from_seed).collect();
    model.activate(3, &next);
    assert!(model.authorize(receipt, package)); // Exact accepted history remains valid.
    let mut late = body.clone();
    late.sequence = 2;
    late.previous = receipt;
    assert!(!model.attest(&late, 0, &current[0].sign(DOMAIN, &late.encode())));
    let late_hash = Sha256::hash(&[&late.encode()]).0;
    assert!(!model.authorize(late_hash, package));
    late.epoch = 3;
    for (i, key) in next.iter().enumerate().take(3) {
        assert!(model.attest(&late, i, &key.sign(DOMAIN, &late.encode())));
    }
    assert!(model.authorize(Sha256::hash(&[&late.encode()]).0, package));
    assert_eq!(model.sequence, 2);
    println!("PASS model: retired identity, duplicate, altered manifest, wrong chain, t-1, current quorum, epoch race, historical exact replay");
    println!(
        "MODEL ONLY: no production Vote/DKG/finality/registry/fetch implementation is claimed"
    );
}

// Witnesses passed here have already been verified against the same real native
// signature in main. They are interchangeable evidence, not committed Git state.
pub fn convergence_check(package: &[u8], witness_a: &[u8], witness_b: &[u8]) {
    assert_ne!(witness_a, witness_b);
    let candidate = |witness: &[u8], latest_finalized: u64| {
        assert!(!witness.is_empty());
        let pinned_anchor = 10;
        assert!(latest_finalized >= pinned_anchor);
        Body {
            chain: 2026,
            genesis: [7; 32],
            repo: [8; 20],
            epoch: 2,
            sequence: 1,
            previous: [0; 32],
            manifest: Sha256::hash(&[package]).0,
            anchor: (pinned_anchor, [10; 32]),
        }
    };
    let a = candidate(witness_a, 11);
    let b = candidate(witness_b, 200);
    assert_eq!(a, b); // Different valid subsets/local tips cannot split the slots.
    let keys: Vec<_> = (3..=6).map(bls12381::PrivateKey::from_seed).collect();
    let mut model = Model::new(2, keys.iter().map(Signer::public_key).collect());
    // Original failing schedule: first two see A, last two see B. The third
    // attestation now seals a receipt; losing a fourth validator is harmless.
    for (i, key) in keys.iter().enumerate().take(3) {
        let body = if i < 2 { &a } else { &b };
        assert!(model.attest(body, i, &key.sign(DOMAIN, &body.encode())));
    }
    assert_eq!(model.sequence, 1);
    assert!(model.authorize(Sha256::hash(&[&a.encode()]).0, package));
    assert!(!model.attest(&b, 3, &keys[3].sign(DOMAIN, &b.encode())));
    println!("PASS convergence model: valid subsets 012/123, staggered 2+2 delivery, different local tips, q=3 without fourth participant; one exact checkpoint");
}
