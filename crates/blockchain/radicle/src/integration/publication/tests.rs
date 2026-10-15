use super::*;
use crate::manager::{FinalizedBlock, FinalizedPublicationReceipt};
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::{Participant, TryCollect as _};
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use radicle::crypto::{BlsPublicKey, RepositorySubject};

fn oid_string(byte: u8) -> String {
    hex::encode([byte; 20])
}
fn object(kind: ObjectKind, byte: u8) -> ObjectCommitment {
    ObjectCommitment {
        kind,
        oid: oid_string(byte),
        sha256: hex::encode([byte; 32]),
    }
}
fn owner_ref(name: &str, byte: u8) -> OwnerRef {
    OwnerRef {
        name: name.into(),
        oid: oid_string(byte),
    }
}
fn update(name: &str, old: u8, new: u8) -> RefUpdate {
    RefUpdate {
        name: name.into(),
        old: oid_string(old),
        new: oid_string(new),
    }
}

fn empty_manifest() -> PublicationManifest {
    PublicationManifest::new(vec![], vec![], vec![], vec![]).unwrap()
}

#[test]
fn canonical_manifest_has_exact_sorted_wire_bytes_and_rejects_noncanonical_inputs() {
    let manifest = empty_manifest();
    let bytes = manifest.to_bytes().unwrap();
    assert_eq!(
        bytes,
        br#"{"objects":[],"ownerSnapshot":[],"refs":[],"subjects":[],"version":1}"#
    );
    assert_eq!(PublicationManifest::from_bytes(&bytes).unwrap(), manifest);
    for input in [
        br#"{"version":1,"objects":[],"ownerSnapshot":[],"refs":[],"subjects":[]}"#.as_slice(),
        br#"{"objects":[],"ownerSnapshot":[],"refs":[],"subjects":[],"version":1,"version":1}"#
            .as_slice(),
        br#"{"objects":[],"ownerSnapshot":[],"refs":[],"subjects":[],"version":1,"witness":[]}"#
            .as_slice(),
        br#"{"objects":[],"ownerSnapshot":[],"refs":[],"subjects":[],"version":2}"#.as_slice(),
    ] {
        assert!(PublicationManifest::from_bytes(input).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(b'\n');
    assert!(PublicationManifest::from_bytes(&trailing).is_err());
    let sorted = PublicationManifest::new(
        vec![
            object(ObjectKind::Tree, 3),
            object(ObjectKind::Commit, 2),
            object(ObjectKind::Blob, 1),
        ],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    assert_eq!(
        sorted.objects.iter().map(|o| o.kind).collect::<Vec<_>>(),
        [ObjectKind::Blob, ObjectKind::Commit, ObjectKind::Tree]
    );
    let mut unsorted = serde_json::to_value(&sorted).unwrap();
    unsorted["objects"].as_array_mut().unwrap().reverse();
    let mut encoded = Vec::new();
    unsorted
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut encoded,
            CanonicalFormatter::new(),
        ))
        .unwrap();
    assert!(PublicationManifest::from_bytes(&encoded).is_err());
}

#[test]
fn manifest_rejects_duplicates_bad_hex_refs_subjects_and_silent_unicode_changes() {
    let invalid = |objects, refs, subjects, snapshot| {
        assert!(PublicationManifest::new(objects, refs, subjects, snapshot).is_err())
    };
    invalid(
        vec![object(ObjectKind::Blob, 1), object(ObjectKind::Blob, 1)],
        vec![],
        vec![],
        vec![],
    );
    invalid(
        vec![object(ObjectKind::Blob, 1), object(ObjectKind::Tree, 1)],
        vec![],
        vec![],
        vec![],
    );
    let mut bad = object(ObjectKind::Blob, 1);
    bad.sha256 = "AA".repeat(32);
    invalid(vec![bad], vec![], vec![], vec![]);
    invalid(vec![object(ObjectKind::Blob, 0)], vec![], vec![], vec![]);
    invalid(
        vec![],
        vec![update("refs/heads/main", 1, 1)],
        vec![],
        vec![],
    );
    invalid(
        vec![],
        vec![
            update("refs/heads/main", 0, 1),
            update("refs/heads/main", 0, 2),
        ],
        vec![],
        vec![],
    );
    invalid(vec![], vec![], vec![], vec![owner_ref("main", 1)]);
    invalid(
        vec![],
        vec![],
        vec![],
        vec![owner_ref("refs/namespaces/other/refs/heads/main", 1)],
    );
    invalid(
        vec![],
        vec![],
        vec![],
        vec![owner_ref("refs/heads/a..b", 1)],
    );
    invalid(
        vec![],
        vec![],
        vec![],
        vec![owner_ref("refs/rad/sigrefs", 1)],
    );
    invalid(
        vec![],
        vec![],
        vec![],
        vec![owner_ref("refs/heads/main", 0)],
    );
    invalid(
        vec![],
        vec![],
        vec![],
        vec![
            owner_ref("refs/heads/main", 1),
            owner_ref("refs/heads/main", 2),
        ],
    );
    invalid(
        vec![],
        vec![],
        vec![NativeSubject {
            ordinal: 0,
            envelope: "00".repeat(269),
        }],
        vec![],
    );
    invalid(
        vec![],
        vec![],
        vec![],
        vec![owner_ref("refs/heads/e\u{301}", 1)],
    );
    assert!(
        PublicationManifest::new(vec![], vec![], vec![], vec![owner_ref("refs/heads/é", 1)])
            .is_ok()
    );
}

#[test]
fn manifest_enforces_the_exact_one_mib_limit_before_raw_decode() {
    assert!(matches!(
        PublicationManifest::from_bytes(&vec![b' '; MAX_MANIFEST_BYTES + 1]),
        Err(Error::Size)
    ));
    let mut manifest =
        PublicationManifest::new(vec![], vec![], vec![], vec![owner_ref("refs/heads/a", 1)])
            .unwrap();
    let base = manifest.to_bytes().unwrap().len();
    manifest.owner_snapshot[0]
        .name
        .push_str(&"a".repeat(MAX_MANIFEST_BYTES - base));
    let bytes = manifest.to_bytes().unwrap();
    assert_eq!(bytes.len(), MAX_MANIFEST_BYTES);
    assert_eq!(PublicationManifest::from_bytes(&bytes).unwrap(), manifest);
    manifest.owner_snapshot[0].name.push('a');
    assert!(matches!(manifest.to_bytes(), Err(Error::Size)));
}

pub(crate) fn bootstrap_fixture() -> (PublicationBody, PublicationManifest) {
    let players = (1..5)
        .map(|i| bls12381::PrivateKey::from_seed(i).public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg_for_participants(players).unwrap();
    let providers: Vec<_> = dkg
        .shares
        .iter()
        .map(|s| VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(s.clone())))
        .collect();
    let owner = BlsPublicKey::from_bytes(
        dkg.polynomial
            .public()
            .encode()
            .as_ref()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let body = PublicationBody {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        repo_id: [5; 20].into(),
        descriptor_hash: B256::repeat_byte(8),
        owner_bls_public: (*owner.as_bytes()).into(),
        players_commitment: B256::repeat_byte(9),
        epoch: 2,
        material_version: 7,
        material_hash: B256::repeat_byte(10),
        finalized_anchor_height: 0,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 1,
        previous_receipt_hash: B256::ZERO,
        action: PublicationAction::Bootstrap,
        proposal_id: Default::default(),
        patch_id: Default::default(),
        revision_id: Default::default(),
        expected_target_head: Default::default(),
        package_manifest_hash: B256::ZERO,
    };
    // Codec fixtures have real threshold signatures; their payloads are not a native Git package.
    let subjects = (0..3)
        .map(|ordinal| {
            let context = native_policy_context(&body, ordinal);
            let subject = match ordinal {
                0 => RepositorySubject::identity(body.repo_id.0, context, [5; 20]),
                1 => RepositorySubject::cob(body.repo_id.0, context, [6; 20], b"fixture"),
                _ => RepositorySubject::signed_refs(body.repo_id.0, context, [0; 20], b"fixture")
                    .unwrap(),
            };
            let partials: Vec<_> = providers
                .iter()
                .enumerate()
                .take(3)
                .map(|(i, p)| {
                    p.sign_repository(7, Participant::from_usize(i), &dkg.polynomial, &subject)
                        .unwrap()
                })
                .collect();
            let signature = providers[0]
                .recover_repository(7, &dkg.polynomial, &subject, &partials)
                .unwrap();
            let envelope = BlsEnvelope::attach(
                subject,
                owner,
                signature.encode().as_ref().try_into().unwrap(),
            )
            .unwrap();
            NativeSubject {
                ordinal,
                envelope: envelope.to_hex(),
            }
        })
        .collect();
    let snapshot = vec![
        owner_ref("refs/heads/main", 1),
        owner_ref("refs/rad/root", 2),
    ];
    let manifest = PublicationManifest::new(
        vec![object(ObjectKind::Commit, 3)],
        vec![
            update("refs/heads/main", 0, 1),
            update("refs/rad/root", 0, 2),
            update("refs/rad/sigrefs", 0, 3),
        ],
        subjects,
        snapshot,
    )
    .unwrap();
    (body, manifest)
}

fn receipt(
    mut body: PublicationBody,
    manifest: &PublicationManifest,
) -> FinalizedPublicationReceipt {
    body.package_manifest_hash = manifest.hash().unwrap();
    FinalizedPublicationReceipt {
        block: FinalizedBlock {
            number: 10 + body.sequence,
            hash: B256::repeat_byte(12),
        },
        receipt: outbe_radicleregistry::publication::PublicationReceipt {
            accepted_height: body.sequence,
            receipt_hash: body.receipt_hash(),
            body,
        },
    }
}

#[test]
fn checkpoint_snapshot_replays_bind_material_without_losing_the_native_sigrefs() {
    let (body, manifest) = bootstrap_fixture();
    let initial = receipt(body, &manifest);
    let mut snapshot = CheckpointOwnerSnapshot::bootstrap(initial.clone(), &manifest).unwrap();
    assert_eq!(snapshot.sigrefs().to_string(), oid_string(3));
    let expected_refs = snapshot.owner_refs().clone();
    for sequence in 2..=4 {
        let bind =
            PublicationManifest::new(vec![], vec![], vec![], manifest.owner_snapshot.clone())
                .unwrap();
        let mut body = snapshot.checkpoint().receipt().body.clone();
        body.sequence = sequence;
        body.previous_receipt_hash = snapshot.checkpoint().receipt().receipt_hash;
        body.action = PublicationAction::BindMaterial;
        body.epoch += 1;
        body.material_version += 1;
        let next = receipt(body.clone(), &bind);
        let mut skipped = next.clone();
        skipped.receipt.body.sequence += 1;
        assert!(snapshot.advance(skipped, &bind).is_err());
        let mut wrong_previous = next.clone();
        wrong_previous.receipt.body.previous_receipt_hash = B256::repeat_byte(99);
        assert!(snapshot.advance(wrong_previous, &bind).is_err());
        let changed = PublicationManifest::new(
            vec![],
            vec![],
            vec![],
            vec![
                owner_ref("refs/heads/main", 9),
                owner_ref("refs/rad/root", 2),
            ],
        )
        .unwrap();
        assert!(snapshot.advance(receipt(body, &changed), &changed).is_err());
        snapshot.advance(next, &bind).unwrap();
        assert_eq!(snapshot.sigrefs().to_string(), oid_string(3));
        assert_eq!(snapshot.owner_refs(), &expected_refs);
        assert_eq!(snapshot.checkpoint().receipt().body.sequence, sequence);
    }
}

#[test]
fn checkpoint_snapshot_rejects_wrong_receipt_job_subject_owner_and_partial_refs() {
    let (body, manifest) = bootstrap_fixture();
    let initial = receipt(body.clone(), &manifest);
    let mut altered = manifest.clone();
    altered.owner_snapshot[0].oid = oid_string(9);
    assert!(CheckpointOwnerSnapshot::bootstrap(initial.clone(), &altered).is_err());
    let mut wrong = body.clone();
    wrong.genesis_hash = B256::repeat_byte(8);
    assert!(CheckpointOwnerSnapshot::bootstrap(receipt(wrong, &manifest), &manifest).is_err());
    let mut wrong_owner = body.clone();
    wrong_owner.owner_bls_public[0] ^= 1;
    assert!(
        CheckpointOwnerSnapshot::bootstrap(receipt(wrong_owner, &manifest), &manifest).is_err()
    );
    let mut partial = manifest.clone();
    partial.refs.retain(|r| r.name != "refs/heads/main");
    assert!(CheckpointOwnerSnapshot::bootstrap(receipt(body.clone(), &partial), &partial).is_err());
    let mut wrong_kind = manifest.clone();
    wrong_kind.subjects.swap(0, 1);
    assert!(wrong_kind.to_bytes().is_err());
    let mut missing_commit = manifest.clone();
    missing_commit.objects.clear();
    assert!(CheckpointOwnerSnapshot::bootstrap(
        receipt(body.clone(), &missing_commit),
        &missing_commit
    )
    .is_err());
    let mut bad_old = manifest.clone();
    bad_old.refs[0].old = oid_string(9);
    assert!(CheckpointOwnerSnapshot::bootstrap(receipt(body, &bad_old), &bad_old).is_err());
}
