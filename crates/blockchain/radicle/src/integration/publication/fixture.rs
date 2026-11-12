//! Bootstrap publication fixture shared by radicle tests and runtime approval tests.
use super::{
    native_policy_context, NativeSubject, ObjectCommitment, ObjectKind, OwnerRef,
    PublicationManifest, RefUpdate,
};
use alloy_primitives::B256;
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381;
use commonware_cryptography::Signer as _;
use commonware_utils::{Participant, TryCollect as _};
use outbe_primitives::radicle_publication::{PublicationAction, PublicationBody};
use outbe_simplex::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};

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

pub fn bootstrap_fixture() -> (PublicationBody, PublicationManifest) {
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
