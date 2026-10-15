use super::*;
use crate::integration::publication::review::{
    prepare_review_publication, verify_review_publication,
};
use crate::integration::publication::{
    native_policy_context, CheckpointOwnerSnapshot, NativeSubject, ObjectCommitment, ObjectKind,
    OwnerRef, PublicationManifest, RefUpdate,
};
use crate::integration::signing::witness::{RepositoryWitnessBundle, MAX_WITNESS_BYTES};
use crate::integration::signing::{RepositoryPartial, RepositorySigningMaterial};
use crate::manager::FinalizedBlock;
use alloy_primitives::{B256, U256};
use commonware_codec::Encode as _;
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    Signer as _,
};
use commonware_utils::{Participant, TryCollect as _};
use outbe_consensus::{bls::bootstrap_dkg_for_participants, hybrid::VrfMaterialProvider};
use outbe_radicleregistry::PatchApprovalRecord;
use radicle::{
    cob::{
        identity::{self, Identity},
        patch::{Patch, Patches},
        store::access::WriteAs,
    },
    crypto::{
        BlsEnvelope, BlsPublicKey, RepositorySubject, Seed, Signer as _, SigningKey, SubjectKind,
    },
    identity::{Doc, Project, RepoId, Visibility},
    storage::{
        publication::{self, EntryAuthorization, EntryRequest, PublicationPolicy},
        WriteStorage as _,
    },
};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Receipts(Mutex<Vec<(git::Oid, SubjectKind, [u8; 32])>>);

impl Receipts {
    fn accept(&self, entry: git::Oid, kind: SubjectKind, context: [u8; 32]) {
        self.0.lock().unwrap().push((entry, kind, context));
    }
}

impl PublicationPolicy for Receipts {
    fn id(&self) -> &str {
        "test.review-receipts"
    }
    fn authorize_entry(
        &self,
        request: &EntryRequest,
    ) -> Result<EntryAuthorization, publication::Error> {
        let entries = self.0.lock().unwrap();
        let (_, _, context) = entries
            .iter()
            .find(|(id, kind, _)| *id == request.entry && *kind == request.kind)
            .ok_or(publication::Error::Denied)?;
        Ok(EntryAuthorization::accepted(
            request.clone(),
            *context,
            [17; 32],
        ))
    }
}

fn sign(
    providers: &[VrfMaterialProvider<MinSig>],
    polynomial: &commonware_cryptography::bls12381::primitives::sharing::Sharing<MinSig>,
    owner: BlsPublicKey,
    subject: RepositorySubject,
) -> BlsEnvelope {
    let partials: Vec<_> = providers
        .iter()
        .enumerate()
        .take(3)
        .map(|(index, provider)| {
            provider
                .sign_repository(7, Participant::from_usize(index), polynomial, &subject)
                .unwrap()
        })
        .collect();
    let signature = providers[0]
        .recover_repository(7, polynomial, &subject, &partials)
        .unwrap();
    BlsEnvelope::attach(
        subject,
        owner,
        signature.encode().to_vec().try_into().unwrap(),
    )
    .unwrap()
}

fn code(repo: &Repository, parent: Option<git::Oid>, contents: &[u8]) -> git::Oid {
    let blob = repo.raw().blob(contents).unwrap();
    let mut tree = repo.raw().treebuilder(None).unwrap();
    tree.insert("README", blob, 0o100644).unwrap();
    let tree = repo.raw().find_tree(tree.write().unwrap()).unwrap();
    let author = git::raw::Signature::new(
        "Author",
        "author@example.test",
        &git::raw::Time::new(1700000000, 0),
    )
    .unwrap();
    let parents: Vec<_> = parent
        .into_iter()
        .map(|id| repo.raw().find_commit(id.into()).unwrap())
        .collect();
    repo.raw()
        .commit(
            None,
            &author,
            &author,
            "Code",
            &tree,
            &parents.iter().collect::<Vec<_>>(),
        )
        .unwrap()
        .into()
}

fn native_object_commitments(repo: &Repository, roots: &[git::Oid]) -> Vec<ObjectCommitment> {
    let mut queue = roots.to_vec();
    let mut seen = std::collections::BTreeSet::new();
    let mut objects = vec![];
    let odb = repo.raw().odb().unwrap();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let object = odb.read(id.into()).unwrap();
        let kind = match object.kind() {
            git::raw::ObjectType::Commit => {
                queue.push(repo.raw().find_commit(id.into()).unwrap().tree_id().into());
                ObjectKind::Commit
            }
            git::raw::ObjectType::Tree => {
                queue.extend(
                    repo.raw()
                        .find_tree(id.into())
                        .unwrap()
                        .iter()
                        .map(|e| git::Oid::from(e.id())),
                );
                ObjectKind::Tree
            }
            git::raw::ObjectType::Blob => ObjectKind::Blob,
            _ => unreachable!(),
        };
        objects.push(ObjectCommitment {
            kind,
            oid: id.to_string(),
            sha256: hex::encode(Sha256::hash(&[object.data()]).0),
        });
    }
    objects
}

fn assert_independent_witnesses(
    verifier: &RepositorySigningMaterial,
    actual: &RepositorySubject,
    envelope: &BlsEnvelope,
    contributions: &[(bls12381::PublicKey, RepositoryPartial)],
) {
    let frames: Vec<_> = contributions.iter().map(|(_, f)| f.clone()).collect();
    verifier.verify_witness(actual, envelope, &frames).unwrap();
    verifier
        .verify_witness(actual, envelope, &frames[..3])
        .unwrap();
    verifier
        .verify_witness(actual, envelope, &frames[1..])
        .unwrap();
    assert!(verifier
        .verify_witness(actual, envelope, &frames[..2])
        .is_err());
    assert!(verifier.verify_witness(actual, envelope, &[]).is_err());
    let mut duplicate = frames.clone();
    duplicate[1] = duplicate[0].clone();
    assert!(verifier
        .verify_witness(actual, envelope, &duplicate)
        .is_err());
    let mut unsorted = frames.clone();
    unsorted.swap(0, 1);
    assert!(verifier
        .verify_witness(actual, envelope, &unsorted)
        .is_err());
    let mut invalid_last = frames.clone();
    let mut bytes = invalid_last.last().unwrap().to_bytes();
    bytes[0] ^= 1;
    *invalid_last.last_mut().unwrap() = RepositoryPartial::from_bytes(&bytes).unwrap();
    assert!(verifier
        .verify_witness(actual, envelope, &invalid_last)
        .is_err());
    let mut outsider = frames.clone();
    let mut bytes = outsider.last().unwrap().to_bytes();
    bytes[120..124].copy_from_slice(&u32::MAX.to_be_bytes());
    *outsider.last_mut().unwrap() = RepositoryPartial::from_bytes(&bytes).unwrap();
    assert!(verifier
        .verify_witness(actual, envelope, &outsider)
        .is_err());
    let mut oversized = frames;
    oversized.push(oversized[0].clone());
    assert!(verifier
        .verify_witness(actual, envelope, &oversized)
        .is_err());
}

fn assert_witness_bundle(
    material: &RepositorySigningMaterial,
    actual: &[(RepositorySubject, BlsEnvelope)],
    subjects: Vec<Vec<RepositoryPartial>>,
) -> Vec<u8> {
    fn encode(value: &serde_json::Value) -> Vec<u8> {
        let mut bytes = vec![];
        serde::Serialize::serialize(
            value,
            &mut serde_json::Serializer::with_formatter(
                &mut bytes,
                radicle::canonical::formatter::CanonicalFormatter::new(),
            ),
        )
        .unwrap();
        bytes
    }
    let bundle =
        RepositoryWitnessBundle::new(material.canonical_output(), subjects.clone()).unwrap();
    let bytes = bundle.to_bytes().unwrap();
    let decoded = RepositoryWitnessBundle::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.public_output(), material.canonical_output());
    decoded.verify(material, actual).unwrap();
    assert!(decoded.verify(material, &actual[..1]).is_err());
    let mut wrong_subject = actual.to_vec();
    wrong_subject.swap(0, 1);
    assert!(decoded.verify(material, &wrong_subject).is_err());
    let mut unsorted = subjects.clone();
    unsorted[0].reverse();
    assert_eq!(
        RepositoryWitnessBundle::new(material.canonical_output(), unsorted)
            .unwrap()
            .to_bytes()
            .unwrap(),
        bytes
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let reject = |value| {
        assert!(RepositoryWitnessBundle::from_bytes(&encode(&value)).is_err());
    };
    let mut invalid = value.clone();
    invalid["version"] = 2.into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["extra"] = true.into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["extra"] = true.into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][1]["ordinal"] = 0.into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][1]["ordinal"] = 2.into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"]
        .as_array_mut()
        .unwrap()
        .swap(0, 1);
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"][1] = invalid["subjects"][0]["partials"][0].clone();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"] = serde_json::json!([]);
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"][0] = "00".into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"][0] = "zz".repeat(268).into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"] =
        vec![value["subjects"][0]["partials"][0].clone(); 257].into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"] = vec![value["subjects"][0].clone(); 4].into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["publicOutput"] = "".into();
    reject(invalid);
    let mut insufficient = subjects.clone();
    insufficient[1].truncate(2);
    assert!(
        RepositoryWitnessBundle::new(material.canonical_output(), insufficient)
            .unwrap()
            .verify(material, actual)
            .is_err()
    );
    let mut changed_output = material.canonical_output();
    changed_output[0] ^= 1;
    assert!(
        RepositoryWitnessBundle::new(changed_output, subjects.clone())
            .unwrap()
            .verify(material, actual)
            .is_err()
    );
    let mut invalid = value.clone();
    invalid["publicOutput"] = value["publicOutput"]
        .as_str()
        .unwrap()
        .to_uppercase()
        .into();
    reject(invalid);
    let mut invalid = value.clone();
    invalid["subjects"][0]["partials"][0] = value["subjects"][0]["partials"][0]
        .as_str()
        .unwrap()
        .to_uppercase()
        .into();
    reject(invalid);
    for invalid in [
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.as_slice(), b"\n"].concat(),
        bytes
            .strip_suffix(b"}")
            .unwrap()
            .iter()
            .copied()
            .chain(b",\"version\":1}".iter().copied())
            .collect(),
    ] {
        assert!(RepositoryWitnessBundle::from_bytes(&invalid).is_err());
    }
    // The exact-size codec fixture is structural; its padded Output grants no authority.
    let mut boundary = value.clone();
    boundary["publicOutput"] = "".into();
    boundary["subjects"].as_array_mut().unwrap().truncate(1);
    boundary["subjects"][0]["partials"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    let overhead = encode(&boundary).len();
    boundary["publicOutput"] = "00".repeat((MAX_WITNESS_BYTES - overhead) / 2).into();
    let mut boundary = encode(&boundary);
    assert_eq!(boundary.len(), MAX_WITNESS_BYTES);
    let boundary_bundle = RepositoryWitnessBundle::from_bytes(&boundary).unwrap();
    assert!(boundary_bundle.verify(material, actual).is_err());
    boundary.push(b' ');
    assert!(RepositoryWitnessBundle::from_bytes(&boundary).is_err());
    assert!(
        RepositoryWitnessBundle::new(vec![0; MAX_WITNESS_BYTES / 2], subjects.clone()).is_err()
    );
    assert!(RepositoryWitnessBundle::new(
        material.canonical_output(),
        vec![subjects[0].clone(); 4]
    )
    .is_err());
    assert!(RepositoryWitnessBundle::new(
        material.canonical_output(),
        vec![vec![subjects[0][0].clone(); 257]]
    )
    .is_err());
    bytes
}

#[test]
fn review_context_matches_frozen_p0_native_vectors() {
    let decision = PatchVotePayloadV1::accept(
        "0x35807d21ae0c4c27437366dd171855c93636bf95"
            .parse()
            .unwrap(),
        [0x33; 20].into(),
        [0x33; 20].into(),
        "0xc91bf7c8862b574ce7d117ea1c89c0912313d6ee"
            .parse()
            .unwrap(),
    );
    let approval = FinalizedPatchApproval {
        block: FinalizedBlock {
            number: 10,
            hash: B256::repeat_byte(10),
        },
        approval: PatchApprovalRecord {
            proposal_id: U256::from_be_bytes([1; 32]),
            payload: String::from_utf8(decision.encode().unwrap()).unwrap(),
            approved_height: 10,
            approved_timestamp: 1700000000,
        },
    };
    let context = ReviewSigningContext {
        chain_id: 2026,
        genesis_hash: [7; 32],
        epoch: 2,
        material_version: 1,
        material_hash: [9; 32],
        previous_receipt_hash: [0; 32],
    };
    assert_eq!(
        hex::encode(context.policy_context(&decision, &approval, 0)),
        "82f679799d0b38175e3f8f6a2263f216d43e721cce04f1de980b6bf87afa7f90"
    );
    assert_eq!(
        hex::encode(context.policy_context(&decision, &approval, 1)),
        "a1466aba6d62a4610e59c1b3ffc4a0fc17db42114d11b90497933049d36e9c24"
    );
}

#[test]
fn exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts() {
    let temp = tempfile::tempdir().unwrap();
    let mut identities: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    identities.sort_by_key(|key| key.public_key().encode());
    let players = identities
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = bootstrap_dkg_for_participants(players).unwrap();
    let owner = BlsPublicKey::from_bytes(
        dkg.polynomial
            .public()
            .encode()
            .to_vec()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    let providers: Vec<_> = dkg
        .shares
        .iter()
        .map(|share| VrfMaterialProvider::new(7, dkg.polynomial.clone(), Some(share.clone())))
        .collect();
    let authority = crate::manager::FinalizedRepositoryAuthority {
        block: FinalizedBlock {
            number: 12,
            hash: B256::repeat_byte(12),
        },
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        epoch: 2,
        committee: outbe_validatorset::state::CommitteeSnapshot {
            committee: identities
                .iter()
                .enumerate()
                .map(|(i, key)| outbe_validatorset::state::CommitteeEntry {
                    address: alloy_primitives::Address::repeat_byte(i as u8 + 1),
                    consensus_pubkey: key.public_key().encode().as_ref().try_into().unwrap(),
                })
                .collect(),
            vrf_material_version: 7,
            vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
            vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
                &dkg.polynomial,
            ),
        },
    };
    let bootstrap_material =
        RepositorySigningMaterial::new(authority.clone(), dkg.output.clone(), owner).unwrap();
    let doc = Doc::initial(
        Project::new(
            "governments".parse().unwrap(),
            "Network repository".into(),
            git::fmt::refname!("main"),
        )
        .unwrap(),
        RepositoryKey::from(owner).into(),
        Visibility::Public,
    );
    let (blob, bytes) = doc.encode().unwrap();
    let rid: RepoId = blob.into();
    let mut bootstrap_body = outbe_primitives::radicle_publication::PublicationBody {
        chain_id: 2026,
        genesis_hash: B256::repeat_byte(7),
        repo_id: rid.into_sha1().unwrap().into(),
        descriptor_hash: B256::repeat_byte(4),
        owner_bls_public: (*owner.as_bytes()).into(),
        players_commitment: bootstrap_material.players_commitment().into(),
        epoch: 2,
        material_version: 7,
        material_hash: bootstrap_material.material_hash().into(),
        finalized_anchor_height: 0,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 1,
        previous_receipt_hash: B256::ZERO,
        action: outbe_primitives::radicle_publication::PublicationAction::Bootstrap,
        proposal_id: U256::ZERO,
        patch_id: Default::default(),
        revision_id: Default::default(),
        expected_target_head: Default::default(),
        package_manifest_hash: B256::ZERO,
    };
    let document_context = native_policy_context(&bootstrap_body, 0);
    let identity_context = native_policy_context(&bootstrap_body, 1);
    let initial_refs_context = native_policy_context(&bootstrap_body, 2);
    let author = SigningKey::from_seed(Seed::new([41; 32]));
    let storage = radicle::Storage::open(
        temp.path().join("storage"),
        git::UserInfo {
            alias: radicle::node::Alias::new("Collector"),
            key: *author.public_key(),
        },
    )
    .unwrap();
    let receipts = Arc::new(Receipts::default());
    storage
        .manage_repository(rid, owner.into(), receipts.clone())
        .unwrap();
    let repo = storage.create(rid).unwrap();
    let document_signature = sign(
        &providers,
        &dkg.polynomial,
        owner,
        RepositorySubject::identity(
            rid.into_sha1().unwrap(),
            document_context,
            blob.into_sha1().unwrap(),
        ),
    );
    let tx = Transaction::<Identity, Repository>::new(
        identity::TYPENAME.clone(),
        vec![identity::Action::Revision {
            title: cob::Title::new("Initial revision").unwrap(),
            description: String::new(),
            blob,
            parent: None,
            signature: document_signature.clone().into(),
        }],
        vec![cob::Embed::<cob::Uri>::store("radicle.json", &bytes, repo.raw()).unwrap()],
    );
    let unsigned = tx
        .prepare_native(
            &repo,
            None,
            vec![],
            "Bootstrap repository\n".into(),
            "Outbe Network <network@outbe.invalid> 1700000000 +0000"
                .parse()
                .unwrap(),
        )
        .unwrap();
    let signature = sign(
        &providers,
        &dkg.polynomial,
        owner,
        RepositorySubject::cob(
            rid.into_sha1().unwrap(),
            identity_context,
            unsigned.tree(),
            unsigned.as_bytes(),
        ),
    );
    let identity = cob::attach_native(
        repo.raw(),
        unsigned,
        rid.into_sha1().unwrap(),
        identity_context,
        &signature,
    )
    .unwrap()
    .id;
    receipts.accept(identity, SubjectKind::Cob, identity_context);
    receipts.accept(identity, SubjectKind::Identity, document_context);
    let owner = RepositoryKey::from(owner);
    object::Storage::update(
        &repo,
        &owner,
        &identity::TYPENAME,
        &identity.into(),
        &identity,
    )
    .unwrap();
    repo.raw()
        .reference_symbolic(
            git::refs::storage::id(&owner).as_str(),
            git::refs::storage::cob(&owner, &identity::TYPENAME, &identity.into()).as_str(),
            false,
            "Network identity",
        )
        .unwrap();
    repo.set_remote_identity_root_to(&owner, identity).unwrap();
    repo.set_identity_head_to(identity).unwrap();
    let base = code(&repo, None, b"base");
    let head = code(&repo, Some(base), b"approved revision");
    repo.raw()
        .reference(
            git::fmt::Qualified::from(git::fmt::lit::refs_heads(git::fmt::refname!("main")))
                .with_namespace((&owner).into())
                .as_str(),
            base.into(),
            false,
            "Network main",
        )
        .unwrap();
    repo.set_canonical_symbolic_refs("Network canonical refs")
        .unwrap();
    repo.set_default_branch_to_canonical_head().unwrap();
    let accepted_refs = repo.references_of(&owner).unwrap();
    let initial_refs = PreparedSignedRefs::new(
        accepted_refs.clone(),
        rid,
        owner,
        initial_refs_context,
        None,
        Committer::new(
            "Outbe Network <network@outbe.invalid> 1700000000 +0000"
                .parse()
                .unwrap(),
        ),
        "Update signed refs\n".into(),
    )
    .unwrap();
    let initial_signature = sign(
        &providers,
        &dkg.polynomial,
        match owner {
            RepositoryKey::Bls12381G2(key) => key,
            _ => unreachable!(),
        },
        initial_refs.subject().unwrap(),
    );
    let previous_sigrefs = initial_refs
        .attach(repo.raw(), initial_signature.clone())
        .unwrap()
        .oid();
    receipts.accept(
        previous_sigrefs,
        SubjectKind::SignedRefs,
        initial_refs_context,
    );
    repo.raw()
        .reference(
            git::refs::storage::sigrefs(&owner).as_str(),
            previous_sigrefs.into(),
            false,
            "Bootstrap signed refs",
        )
        .unwrap();
    let mut patches = Patches::open(&repo, WriteAs::new(&author)).unwrap();
    let mut cache = cob::cache::NoCache;
    let mut patch = patches
        .create(
            cob::Title::new("Proposed patch").unwrap(),
            "Exact subject",
            patch::MergeTarget::default(),
            base,
            head,
            &[],
            &mut cache,
        )
        .unwrap();
    let patch_id = *patch.id();
    let revision = patch.latest().0;
    let mut approval = FinalizedPatchApproval {
        block: FinalizedBlock {
            number: 12,
            hash: B256::repeat_byte(12),
        },
        approval: PatchApprovalRecord {
            proposal_id: U256::from(5),
            payload: String::from_utf8(
                PatchVotePayloadV1::accept(
                    rid.into_sha1().unwrap().into(),
                    (*patch_id).into_sha1().unwrap().into(),
                    revision.into_inner().into_sha1().unwrap().into(),
                    base.into_sha1().unwrap().into(),
                )
                .encode()
                .unwrap(),
            )
            .unwrap(),
            approved_height: 10,
            approved_timestamp: 1700000010,
        },
    };
    let owner_key = match owner {
        RepositoryKey::Bls12381G2(key) => key,
        _ => unreachable!(),
    };
    let mut missing_polynomial = authority.clone();
    missing_polynomial.committee.vrf_public_polynomial_hash = B256::ZERO;
    assert!(
        RepositorySigningMaterial::new(missing_polynomial, dkg.output.clone(), owner_key).is_err()
    );
    let mut wrong_order = authority.clone();
    wrong_order.committee.committee.reverse();
    assert!(RepositorySigningMaterial::new(wrong_order, dkg.output.clone(), owner_key).is_err());
    let material = RepositorySigningMaterial::new(
        authority.clone(),
        dkg.output.clone(),
        match owner {
            RepositoryKey::Bls12381G2(key) => key,
            _ => unreachable!(),
        },
    )
    .unwrap();
    let public_output = material.canonical_output();
    let independent = RepositorySigningMaterial::from_canonical_output(
        authority.clone(),
        &public_output,
        owner_key,
    )
    .unwrap();
    assert_eq!(independent.material_hash(), material.material_hash());
    for invalid in [
        vec![],
        public_output[..public_output.len() - 1].to_vec(),
        [public_output.as_slice(), &[0]].concat(),
        vec![0; 1024 * 1024 + 1],
    ] {
        assert!(RepositorySigningMaterial::from_canonical_output(
            authority.clone(),
            &invalid,
            owner_key
        )
        .is_err());
    }
    let owner_snapshot: Vec<_> = accepted_refs
        .iter()
        .map(|(name, id)| OwnerRef {
            name: name.to_string(),
            oid: id.to_string(),
        })
        .collect();
    let mut updates: Vec<_> = owner_snapshot
        .iter()
        .map(|r| RefUpdate {
            name: r.name.clone(),
            old: git::Oid::ZERO_SHA1.to_string(),
            new: r.oid.clone(),
        })
        .collect();
    updates.push(RefUpdate {
        name: radicle::storage::refs::SIGREFS_BRANCH.to_string(),
        old: git::Oid::ZERO_SHA1.to_string(),
        new: previous_sigrefs.to_string(),
    });
    let manifest = PublicationManifest::new(
        native_object_commitments(&repo, &[identity, previous_sigrefs, base]),
        updates,
        vec![
            NativeSubject {
                ordinal: 0,
                envelope: document_signature.to_hex(),
            },
            NativeSubject {
                ordinal: 1,
                envelope: signature.to_hex(),
            },
            NativeSubject {
                ordinal: 2,
                envelope: initial_signature.to_hex(),
            },
        ],
        owner_snapshot,
    )
    .unwrap();
    bootstrap_body.package_manifest_hash = manifest.hash().unwrap();
    let checkpoint = crate::manager::FinalizedPublicationReceipt {
        block: FinalizedBlock {
            number: 8,
            hash: B256::repeat_byte(8),
        },
        receipt: outbe_radicleregistry::publication::PublicationReceipt {
            accepted_height: 8,
            receipt_hash: bootstrap_body.receipt_hash(),
            body: bootstrap_body,
        },
    };
    let mut snapshot = CheckpointOwnerSnapshot::bootstrap(checkpoint, &manifest).unwrap();
    let signing = material.review_context(snapshot.checkpoint().receipt().receipt_hash.0);
    for field in 0..6 {
        let mut stale = signing.clone();
        match field {
            0 => stale.chain_id += 1,
            1 => stale.genesis_hash[0] ^= 1,
            2 => stale.epoch += 1,
            3 => stale.material_version += 1,
            4 => stale.material_hash[0] ^= 1,
            _ => stale.previous_receipt_hash[0] ^= 1,
        }
        assert!(matches!(
            prepare_patch_review_from_checkpoint(&repo, &approval, &snapshot, &stale),
            Err(Error::Snapshot)
        ));
    }
    let prepared = Arc::new(
        prepare_patch_review_from_checkpoint(&repo, &approval, &snapshot, &signing).unwrap(),
    );
    let current_checkpoint = crate::manager::FinalizedPublicationReceipt {
        block: approval.block,
        receipt: snapshot.checkpoint().receipt().clone(),
    };
    let current_state = crate::manager::FinalizedPatchSigningState {
        authority: authority.clone(),
        approval: approval.clone(),
        checkpoint: current_checkpoint.clone(),
        material: current_checkpoint,
        stage: outbe_radicleregistry::schema::PATCH_EXECUTION_PENDING,
        anchor: FinalizedBlock {
            number: 10,
            hash: B256::repeat_byte(10),
        },
    };
    let local_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let later_head = code(&repo, Some(head), b"later revision");
    let later_revision = patch
        .update("Later local author update", base, later_head)
        .unwrap();
    assert_ne!(later_revision, revision);
    let again = prepare_patch_review(&repo, &approval, identity, None, &signing).unwrap();
    assert_eq!(prepared.commit.as_bytes(), again.commit.as_bytes());
    assert_eq!(prepared.revision, revision);
    assert_eq!(prepared.subject, again.subject);
    let context = prepared.context;
    let refs_context = prepared.signed_refs_context;
    let contributions: Vec<_> = local_runtime
        .block_on(
            crate::integration::network::material_tests::review_partials(
                prepared.clone(),
                &current_state,
                &dkg.output,
                &providers,
                &identities,
            ),
        )
        .into_iter()
        .map(|(key, frame)| {
            let bytes = frame.to_bytes();
            assert_eq!(bytes.len(), 268);
            let frame = RepositoryPartial::from_bytes(&bytes).unwrap();
            material
                .verify_partial(&providers[0], &prepared, &key, &frame)
                .unwrap();
            (key, frame)
        })
        .collect();
    assert!(material
        .verify_partial(
            &providers[0],
            &prepared,
            &contributions[1].0,
            &contributions[0].1
        )
        .is_err());
    let bytes = contributions[0].1.to_bytes();
    assert!(RepositoryPartial::from_bytes(&bytes[..267]).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(RepositoryPartial::from_bytes(&trailing).is_err());
    for offset in [0, 32, 40, 72, 80, 88, 120, 124, 172] {
        let mut forged = bytes.clone();
        forged[offset] ^= 1;
        if let Ok(frame) = RepositoryPartial::from_bytes(&forged) {
            assert!(material
                .verify_partial(&providers[0], &prepared, &contributions[0].0, &frame)
                .is_err());
        }
    }
    let mut forged = bytes.clone();
    let wrong_identity = identities[1]
        .sign(b"outbe.radicle.partial.v1", &forged[..172])
        .encode();
    forged[172..].copy_from_slice(wrong_identity.as_ref());
    let forged = RepositoryPartial::from_bytes(&forged).unwrap();
    assert!(material
        .verify_partial(&providers[0], &prepared, &contributions[0].0, &forged)
        .is_err());
    let mut wrong_partial = bytes.clone();
    wrong_partial[124..172].copy_from_slice(&contributions[1].1.to_bytes()[124..172]);
    let endorsement = identities[0]
        .sign(b"outbe.radicle.partial.v1", &wrong_partial[..172])
        .encode();
    wrong_partial[172..].copy_from_slice(endorsement.as_ref());
    let wrong_partial = RepositoryPartial::from_bytes(&wrong_partial).unwrap();
    assert!(material
        .verify_partial(
            &providers[0],
            &prepared,
            &contributions[0].0,
            &wrong_partial
        )
        .is_err());
    let mut wrong_domain = bytes.clone();
    let endorsement = identities[0]
        .sign(b"outbe.radicle.publication.v1", &wrong_domain[..172])
        .encode();
    wrong_domain[172..].copy_from_slice(endorsement.as_ref());
    let wrong_domain = RepositoryPartial::from_bytes(&wrong_domain).unwrap();
    assert!(material
        .verify_partial(&providers[0], &prepared, &contributions[0].0, &wrong_domain)
        .is_err());
    assert!(material
        .sign_review(
            &providers[0],
            &prepared,
            alloy_primitives::Address::repeat_byte(2),
            &identities[0]
        )
        .is_err());
    assert!(material
        .sign_review(
            &providers[0],
            &prepared,
            alloy_primitives::Address::repeat_byte(1),
            &bls12381::PrivateKey::from_seed(999)
        )
        .is_err());
    assert!(material
        .sign_review(
            &providers[1],
            &prepared,
            alloy_primitives::Address::repeat_byte(1),
            &identities[0]
        )
        .is_err());
    assert!(material
        .recover_review(&providers[0], &prepared, &contributions[..2])
        .is_err());
    let duplicate = [
        contributions[0].clone(),
        contributions[1].clone(),
        contributions[2].clone(),
        contributions[0].clone(),
    ];
    assert!(material
        .recover_review(&providers[0], &prepared, &duplicate)
        .is_err());
    let signature = material
        .recover_review(&providers[0], &prepared, &contributions[..3])
        .unwrap();
    let alternate = material
        .recover_review(&providers[0], &prepared, &contributions[1..])
        .unwrap();
    assert_eq!(signature.to_bytes(), alternate.to_bytes());
    assert_independent_witnesses(&independent, prepared.subject(), &signature, &contributions);
    for forged in [forged, wrong_partial, wrong_domain] {
        let mut frames: Vec<_> = contributions[..3].iter().map(|(_, f)| f.clone()).collect();
        frames[0] = forged;
        assert!(independent
            .verify_witness(prepared.subject(), &signature, &frames)
            .is_err());
    }
    let mut other_material = signing.clone();
    other_material.material_version += 1;
    let stale = prepare_patch_review(&repo, &approval, identity, None, &other_material).unwrap();
    assert!(signature.verify(&stale.subject, owner).is_err());
    assert!(independent
        .verify_witness(
            stale.subject(),
            &signature,
            &contributions
                .iter()
                .map(|(_, f)| f.clone())
                .collect::<Vec<_>>()
        )
        .is_err());
    assert!(material
        .sign_review(
            &providers[0],
            &stale,
            alloy_primitives::Address::repeat_byte(1),
            &identities[0]
        )
        .is_err());
    assert!(material
        .recover_review(&providers[0], &stale, &contributions[..3])
        .is_err());
    let mut other_network = signing.clone();
    other_network.genesis_hash = [8; 32];
    let replay = prepare_patch_review(&repo, &approval, identity, None, &other_network).unwrap();
    assert!(signature.verify(&replay.subject, owner).is_err());
    let mut other_approval = approval.clone();
    other_approval.approval.proposal_id += U256::from(1);
    let replay = prepare_patch_review(&repo, &other_approval, identity, None, &signing).unwrap();
    assert!(signature.verify(&replay.subject, owner).is_err());
    let (review, prepared_refs) = Arc::into_inner(prepared)
        .unwrap()
        .attach(&repo, &signature)
        .unwrap();
    let prepared_refs = Arc::new(prepared_refs);
    assert_eq!(
        repo.reference_oid(&owner, &git::refs::storage::SIGREFS_BRANCH)
            .unwrap(),
        previous_sigrefs
    );
    let refs_contributions: Vec<_> = local_runtime
        .block_on(crate::integration::network::material_tests::refs_partials(
            prepared_refs.clone(),
            &current_state,
            &dkg.output,
            &providers,
            &identities,
        ))
        .into_iter()
        .map(|(key, frame)| {
            let frame = RepositoryPartial::from_bytes(&frame.to_bytes()).unwrap();
            material
                .verify_review_refs_partial(&providers[0], &prepared_refs, &key, &frame)
                .unwrap();
            (key, frame)
        })
        .collect();
    assert!(material
        .verify_review_refs_partial(
            &providers[0],
            &prepared_refs,
            &contributions[0].0,
            &contributions[0].1
        )
        .is_err());
    assert!(material
        .verify_partial(
            &providers[0],
            &again,
            &refs_contributions[0].0,
            &refs_contributions[0].1
        )
        .is_err());
    assert!(material
        .verify_review_refs_partial(
            &providers[0],
            &prepared_refs,
            &refs_contributions[1].0,
            &refs_contributions[0].1
        )
        .is_err());
    assert!(material
        .recover_review_refs(&providers[0], &prepared_refs, &refs_contributions[..2])
        .is_err());
    assert!(material
        .recover_review_refs(
            &providers[0],
            &prepared_refs,
            &[
                refs_contributions[0].clone(),
                refs_contributions[1].clone(),
                refs_contributions[0].clone(),
            ]
        )
        .is_err());
    let refs_signature = material
        .recover_review_refs(&providers[0], &prepared_refs, &refs_contributions[..3])
        .unwrap();
    let alternate_refs_signature = material
        .recover_review_refs(&providers[0], &prepared_refs, &refs_contributions[1..])
        .unwrap();
    let actual = [
        (again.subject().clone(), signature.clone()),
        (prepared_refs.subject().clone(), refs_signature.clone()),
    ];
    let bundles: Vec<_> = [0..3, 1..4]
        .into_iter()
        .map(|subset| {
            assert_witness_bundle(
                &independent,
                &actual,
                vec![
                    contributions[subset.clone()]
                        .iter()
                        .map(|(_, f)| f.clone())
                        .collect(),
                    refs_contributions[subset]
                        .iter()
                        .map(|(_, f)| f.clone())
                        .collect(),
                ],
            )
        })
        .collect();
    assert_ne!(bundles[0], bundles[1]);
    assert_independent_witnesses(
        &independent,
        prepared_refs.subject(),
        &refs_signature,
        &refs_contributions,
    );
    assert!(independent
        .verify_witness(
            prepared_refs.subject(),
            &refs_signature,
            &contributions
                .iter()
                .map(|(_, f)| f.clone())
                .collect::<Vec<_>>()
        )
        .is_err());
    assert!(independent
        .verify_witness(
            prepared_refs.subject(),
            &signature,
            &refs_contributions
                .iter()
                .map(|(_, f)| f.clone())
                .collect::<Vec<_>>()
        )
        .is_err());
    assert_eq!(
        refs_signature.to_bytes(),
        alternate_refs_signature.to_bytes()
    );
    let (_, stale_refs) = prepare_patch_review(&repo, &approval, identity, None, &signing)
        .unwrap()
        .attach(&repo, &signature, accepted_refs.clone(), previous_sigrefs)
        .unwrap();
    assert_eq!(stale_refs.subject(), prepared_refs.subject());
    let signed_refs = Arc::into_inner(prepared_refs)
        .unwrap()
        .attach(repo.raw(), refs_signature.clone())
        .unwrap()
        .oid();
    let (alternate_entry, alternate_prepared_refs) =
        prepare_patch_review_from_checkpoint(&repo, &approval, &snapshot, &signing)
            .unwrap()
            .attach(&repo, &alternate)
            .unwrap();
    let alternate_sigrefs = alternate_prepared_refs
        .attach(repo.raw(), alternate_refs_signature.clone())
        .unwrap()
        .oid();
    assert_eq!(alternate_entry.id, review.id);
    assert_eq!(alternate_sigrefs, signed_refs);
    assert_eq!(
        repo.reference_oid(&owner, &git::refs::storage::SIGREFS_BRANCH)
            .unwrap(),
        previous_sigrefs
    );
    let patch_ref_name =
        git::refs::storage::cob(&owner, &patch::TYPENAME, &patch_id).strip_namespace_recursive();
    let mut desired_refs = accepted_refs.clone();
    desired_refs.insert(patch_ref_name.to_ref_string(), review.id);
    let pinned: std::collections::BTreeSet<_> =
        native_object_commitments(&repo, &[identity, previous_sigrefs, base, *revision])
            .into_iter()
            .map(|object| object.oid)
            .collect();
    let new_objects = native_object_commitments(&repo, &[review.id, signed_refs])
        .into_iter()
        .filter(|object| !pinned.contains(&object.oid))
        .collect();
    let review_manifest = PublicationManifest::new(
        new_objects,
        vec![
            RefUpdate {
                name: patch_ref_name.to_string(),
                old: git::Oid::ZERO_SHA1.to_string(),
                new: review.id.to_string(),
            },
            RefUpdate {
                name: radicle::storage::refs::SIGREFS_BRANCH.to_string(),
                old: previous_sigrefs.to_string(),
                new: signed_refs.to_string(),
            },
        ],
        vec![
            NativeSubject {
                ordinal: 0,
                envelope: signature.to_hex(),
            },
            NativeSubject {
                ordinal: 1,
                envelope: refs_signature.to_hex(),
            },
        ],
        desired_refs
            .iter()
            .map(|(name, id)| OwnerRef {
                name: name.to_string(),
                oid: id.to_string(),
            })
            .collect(),
    )
    .unwrap();
    let alternate_manifest = PublicationManifest::new(
        review_manifest.objects().to_vec(),
        review_manifest.refs().to_vec(),
        vec![
            NativeSubject {
                ordinal: 0,
                envelope: alternate.to_hex(),
            },
            NativeSubject {
                ordinal: 1,
                envelope: alternate_refs_signature.to_hex(),
            },
        ],
        review_manifest.owner_snapshot().to_vec(),
    )
    .unwrap();
    assert_eq!(
        review_manifest.to_bytes().unwrap(),
        alternate_manifest.to_bytes().unwrap()
    );
    let decision = PatchVotePayloadV1::decode(approval.approval.payload.as_bytes()).unwrap();
    let mut review_body = snapshot.checkpoint().receipt().body.clone();
    review_body.sequence += 1;
    review_body.previous_receipt_hash = snapshot.checkpoint().receipt().receipt_hash;
    review_body.action = outbe_primitives::radicle_publication::PublicationAction::Review;
    review_body.proposal_id = approval.approval.proposal_id;
    review_body.patch_id = decision.patch_id;
    review_body.revision_id = decision.revision_id;
    review_body.expected_target_head = decision.expected_target_head;
    review_body.finalized_anchor_height = approval.approval.approved_height;
    review_body.finalized_anchor_hash = B256::repeat_byte(10);
    review_body.package_manifest_hash = review_manifest.hash().unwrap();
    let witness = RepositoryWitnessBundle::from_bytes(&bundles[0]).unwrap();
    let proof = prepare_review_publication(
        &repo,
        &current_state,
        &snapshot,
        &material,
        [&signature, &refs_signature],
        &witness,
    )
    .unwrap();
    assert_eq!(proof.body(), &review_body);
    assert_eq!(proof.manifest(), &review_manifest);
    proof.authorize_current(&current_state, &material).unwrap();
    let mut consumed = current_state.clone();
    consumed.stage = outbe_radicleregistry::schema::PATCH_EXECUTION_REVIEWED;
    assert!(proof.authorize_current(&consumed, &material).is_err());
    let mut changed = current_state.clone();
    changed.approval.approval.approved_timestamp += 1;
    assert!(proof.authorize_current(&changed, &material).is_err());
    let mut advanced = current_state.clone();
    advanced.checkpoint.receipt.body.sequence += 1;
    advanced.checkpoint.receipt.receipt_hash = advanced.checkpoint.receipt.body.receipt_hash();
    assert!(proof.authorize_current(&advanced, &material).is_err());
    let mut newer_head = current_state.clone();
    newer_head.authority.block.number += 1;
    newer_head.authority.block.hash = B256::repeat_byte(13);
    let newer_material = RepositorySigningMaterial::from_canonical_output(
        newer_head.authority.clone(),
        &material.canonical_output(),
        owner_key,
    )
    .unwrap();
    proof
        .authorize_current(&newer_head, &newer_material)
        .unwrap();
    assert!(proof.authorize_current(&newer_head, &material).is_err());
    let receiver_storage = radicle::Storage::open(
        temp.path().join("independent-verifier"),
        git::UserInfo {
            alias: radicle::node::Alias::new("Verifier"),
            key: *SigningKey::from_seed(Seed::new([42; 32])).public_key(),
        },
    )
    .unwrap();
    receiver_storage
        .manage_repository(rid, owner, receipts.clone())
        .unwrap();
    let receiver = receiver_storage.create(rid).unwrap();
    let missing_blob = repo
        .raw()
        .find_commit(head.into())
        .unwrap()
        .tree()
        .unwrap()
        .get_name("README")
        .unwrap()
        .id();
    let source_odb = repo.raw().odb().unwrap();
    let receiver_odb = receiver.raw().odb().unwrap();
    source_odb
        .foreach(|oid| {
            if *oid != missing_blob {
                let object = source_odb.read(*oid).unwrap();
                assert_eq!(
                    receiver_odb.write(object.kind(), object.data()).unwrap(),
                    *oid
                );
            }
            true
        })
        .unwrap();
    assert!(
        prepare_patch_review_from_checkpoint(&receiver, &approval, &snapshot, &signing).is_err()
    );
    assert!(verify_review_publication(
        &receiver,
        &current_state,
        &snapshot,
        &material,
        &review_body,
        &review_manifest,
        &witness,
    )
    .is_err());
    let object = source_odb.read(missing_blob).unwrap();
    receiver_odb.write(object.kind(), object.data()).unwrap();
    for encoded in &bundles {
        let witness = RepositoryWitnessBundle::from_bytes(encoded).unwrap();
        let received = verify_review_publication(
            &receiver,
            &current_state,
            &snapshot,
            &independent,
            &review_body,
            &review_manifest,
            &witness,
        )
        .unwrap();
        assert_eq!(received.manifest(), &review_manifest);
    }
    let mut bad_manifests = vec![];
    let mut objects = review_manifest.objects().to_vec();
    objects[0].sha256 = hex::encode([0; 32]);
    bad_manifests.push(
        PublicationManifest::new(
            objects,
            review_manifest.refs().to_vec(),
            review_manifest.subjects().to_vec(),
            review_manifest.owner_snapshot().to_vec(),
        )
        .unwrap(),
    );
    let mut objects = review_manifest.objects().to_vec();
    objects.pop();
    bad_manifests.push(
        PublicationManifest::new(
            objects,
            review_manifest.refs().to_vec(),
            review_manifest.subjects().to_vec(),
            review_manifest.owner_snapshot().to_vec(),
        )
        .unwrap(),
    );
    let mut objects = review_manifest.objects().to_vec();
    objects[0].kind = match objects[0].kind {
        ObjectKind::Blob => ObjectKind::Tree,
        _ => ObjectKind::Blob,
    };
    bad_manifests.push(
        PublicationManifest::new(
            objects,
            review_manifest.refs().to_vec(),
            review_manifest.subjects().to_vec(),
            review_manifest.owner_snapshot().to_vec(),
        )
        .unwrap(),
    );
    for extra in [base, later_head] {
        let object = source_odb.read(extra.into()).unwrap();
        let mut objects = review_manifest.objects().to_vec();
        objects.push(ObjectCommitment {
            kind: ObjectKind::Commit,
            oid: extra.to_string(),
            sha256: hex::encode(Sha256::hash(&[object.data()]).0),
        });
        bad_manifests.push(
            PublicationManifest::new(
                objects,
                review_manifest.refs().to_vec(),
                review_manifest.subjects().to_vec(),
                review_manifest.owner_snapshot().to_vec(),
            )
            .unwrap(),
        );
    }
    let mut wrong_refs = review_manifest.refs().to_vec();
    wrong_refs
        .iter_mut()
        .find(|r| r.name == radicle::storage::refs::SIGREFS_BRANCH.as_str())
        .unwrap()
        .old = git::Oid::ZERO_SHA1.to_string();
    bad_manifests.push(
        PublicationManifest::new(
            review_manifest.objects().to_vec(),
            wrong_refs,
            review_manifest.subjects().to_vec(),
            review_manifest.owner_snapshot().to_vec(),
        )
        .unwrap(),
    );
    let mut wrong_snapshot = review_manifest.owner_snapshot().to_vec();
    wrong_snapshot
        .iter_mut()
        .find(|r| r.name == "refs/heads/main")
        .unwrap()
        .oid = later_head.to_string();
    bad_manifests.push(
        PublicationManifest::new(
            review_manifest.objects().to_vec(),
            review_manifest.refs().to_vec(),
            review_manifest.subjects().to_vec(),
            wrong_snapshot,
        )
        .unwrap(),
    );
    for bad_manifest in bad_manifests {
        let mut bad_body = review_body.clone();
        bad_body.package_manifest_hash = bad_manifest.hash().unwrap();
        assert!(verify_review_publication(
            &receiver,
            &current_state,
            &snapshot,
            &independent,
            &bad_body,
            &bad_manifest,
            &witness,
        )
        .is_err());
    }
    for field in 0..3 {
        let mut bad_body = review_body.clone();
        match field {
            0 => bad_body.sequence += 1,
            1 => bad_body.finalized_anchor_height += 1,
            _ => bad_body.finalized_anchor_hash = B256::repeat_byte(99),
        }
        assert!(verify_review_publication(
            &receiver,
            &current_state,
            &snapshot,
            &independent,
            &bad_body,
            &review_manifest,
            &witness,
        )
        .is_err());
    }
    assert!(receiver.raw().references().unwrap().next().is_none());
    assert_eq!(
        repo.reference_oid(&owner, &git::refs::storage::SIGREFS_BRANCH)
            .unwrap(),
        previous_sigrefs
    );
    let checkpoint = crate::manager::FinalizedPublicationReceipt {
        block: approval.block,
        receipt: outbe_radicleregistry::publication::PublicationReceipt {
            accepted_height: 12,
            receipt_hash: review_body.receipt_hash(),
            body: review_body,
        },
    };
    let mut wrong_updates = review_manifest.refs().to_vec();
    wrong_updates
        .iter_mut()
        .find(|update| update.name == radicle::storage::refs::SIGREFS_BRANCH.as_str())
        .unwrap()
        .old = git::Oid::ZERO_SHA1.to_string();
    let wrong_manifest = PublicationManifest::new(
        review_manifest.objects().to_vec(),
        wrong_updates,
        review_manifest.subjects().to_vec(),
        review_manifest.owner_snapshot().to_vec(),
    )
    .unwrap();
    let mut wrong_checkpoint = checkpoint.clone();
    wrong_checkpoint.receipt.body.package_manifest_hash = wrong_manifest.hash().unwrap();
    wrong_checkpoint.receipt.receipt_hash = wrong_checkpoint.receipt.body.receipt_hash();
    let prior_checkpoint = snapshot.checkpoint().clone();
    let prior_refs = snapshot.owner_refs().clone();
    assert!(snapshot.advance(wrong_checkpoint, &wrong_manifest).is_err());
    assert_eq!(snapshot.checkpoint(), &prior_checkpoint);
    assert_eq!(snapshot.owner_refs(), &prior_refs);
    assert_eq!(snapshot.sigrefs(), previous_sigrefs);
    snapshot.advance(checkpoint, &review_manifest).unwrap();
    assert_eq!(snapshot.sigrefs(), signed_refs);
    assert_eq!(snapshot.owner_refs(), &desired_refs);
    assert!(matches!(
        prepare_patch_review_from_checkpoint(&repo, &approval, &snapshot, &signing),
        Err(Error::Snapshot)
    ));
    receipts.accept(review.id, SubjectKind::Cob, context);
    receipts.accept(signed_refs, SubjectKind::SignedRefs, refs_context);
    assert_eq!(*review.author(), owner);
    object::Storage::update(&repo, &owner, &patch::TYPENAME, &patch_id, &review.id).unwrap();
    repo.raw()
        .reference(
            git::refs::storage::sigrefs(&owner).as_str(),
            signed_refs.into(),
            true,
            "Accepted network Review",
        )
        .unwrap();
    let refs = radicle::storage::refs::SignedRefs::load(owner, &repo)
        .unwrap()
        .unwrap();
    assert_eq!(refs.parent(), Some(&previous_sigrefs));
    let patch_ref =
        git::refs::storage::cob(&owner, &patch::TYPENAME, &patch_id).strip_namespace_recursive();
    assert_eq!(refs.refs().get(&patch_ref), Some(review.id));
    assert_eq!(refs.refs().head(git::fmt::refname!("main")), Some(base));
    let accepted = cob::get::<Patch, _>(&repo, &patch::TYPENAME, &patch_id)
        .unwrap()
        .unwrap()
        .object;
    assert_eq!(
        accepted
            .revision(&revision)
            .unwrap()
            .review_by(&owner)
            .unwrap()
            .verdict(),
        Some(patch::Verdict::Accept)
    );
    assert!(accepted
        .revision(&later_revision)
        .unwrap()
        .review_by(&owner)
        .is_none());
    assert!(accepted.is_open());
    let after_review =
        prepare_patch_review(&repo, &approval, identity, Some(review.id), &signing).unwrap();
    let before = repo.raw().references().unwrap().count();
    assert!(matches!(
        after_review.attach(&repo, &signature, accepted_refs, previous_sigrefs),
        Err(Error::Snapshot)
    ));
    assert_eq!(repo.raw().references().unwrap().count(), before);
    let valid_payload = approval.approval.payload.clone();
    for decision in [
        PatchVotePayloadV1::accept(
            [99; 20].into(),
            (*patch_id).into_sha1().unwrap().into(),
            revision.into_inner().into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
        ),
        PatchVotePayloadV1::accept(
            rid.into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
            revision.into_inner().into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
        ),
        PatchVotePayloadV1::accept(
            rid.into_sha1().unwrap().into(),
            (*patch_id).into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
            base.into_sha1().unwrap().into(),
        ),
    ] {
        approval.approval.payload = String::from_utf8(decision.encode().unwrap()).unwrap();
        assert!(prepare_patch_review(&repo, &approval, identity, None, &signing).is_err());
    }
    approval.approval.payload = valid_payload;
    approval.approval.approved_height = 13;
    assert!(matches!(
        prepare_patch_review(&repo, &approval, identity, None, &signing),
        Err(Error::Approval)
    ));
    providers[0].activate(8, dkg.polynomial.clone(), Some(dkg.shares[0].clone()));
    assert!(material
        .sign_review_refs(
            &providers[0],
            &stale_refs,
            alloy_primitives::Address::repeat_byte(1),
            &identities[0]
        )
        .is_err());
    assert!(material
        .verify_review_refs_partial(
            &providers[0],
            &stale_refs,
            &refs_contributions[0].0,
            &refs_contributions[0].1
        )
        .is_err());
    assert!(material
        .recover_review_refs(&providers[0], &stale_refs, &refs_contributions[..3])
        .is_err());
    refs_signature.verify(stale_refs.subject(), owner).unwrap();
    assert!(material
        .verify_partial(
            &providers[0],
            &again,
            &contributions[0].0,
            &contributions[0].1
        )
        .is_err());
    assert!(material
        .sign_review(
            &providers[0],
            &again,
            alloy_primitives::Address::repeat_byte(1),
            &identities[0]
        )
        .is_err());
    assert!(material
        .recover_review(&providers[0], &again, &contributions[..3])
        .is_err());
    signature.verify(&again.subject, owner).unwrap();
}

#[test]
fn pinned_git_closure_checks_code_objects_and_preserves_external_submodules() {
    let temp = tempfile::tempdir().unwrap();
    let repo = git::raw::Repository::init_bare(temp.path()).unwrap();
    let blob = repo.blob(b"pinned content").unwrap();
    let external = git::raw::Oid::from_bytes(&[97; 20]).unwrap();
    let mut builder = repo.treebuilder(None).unwrap();
    builder.insert("README", blob, 0o100644).unwrap();
    builder.insert("submodule", external, 0o160000).unwrap();
    let tree = repo.find_tree(builder.write().unwrap()).unwrap();
    let actor = git::raw::Signature::now("Author", "a@example.test").unwrap();
    let commit = repo
        .commit(None, &actor, &actor, "Pinned", &tree, &[])
        .unwrap();
    let objects = object_closure(&repo, vec![commit.into()], true).unwrap();
    assert_eq!(objects.len(), 3);
    assert!(objects.contains(&blob.into()));
    assert!(!objects.contains(&external.into()));
    assert!(object_closure(&repo, vec![external.into()], true).is_err());
}
