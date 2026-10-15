use super::*;
use crate::manager::FinalizedBlock;
use alloy_primitives::{B256, U256};
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_utils::Participant;
use outbe_consensus::{bls::bootstrap_dkg, hybrid::VrfMaterialProvider};
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
        WriteStorage as _,
        publication::{self, EntryAuthorization, EntryRequest, PublicationPolicy},
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
    owner: BlsPublicKey,
    subject: RepositorySubject,
) -> BlsEnvelope {
    use commonware_codec::DecodeExt as _;
    use commonware_cryptography::bls12381::primitives::variant::Variant;
    let group = <MinSig as Variant>::Public::decode(owner.as_bytes().as_slice()).unwrap();
    let partials: Vec<_> = providers
        .iter()
        .enumerate()
        .take(3)
        .map(|(index, provider)| {
            provider
                .sign_repository(7, Participant::from_usize(index), &group, &subject)
                .unwrap()
        })
        .collect();
    let signature = providers[0]
        .recover_repository(7, &group, &subject, &partials)
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
    let dkg = bootstrap_dkg(4).unwrap();
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
        owner,
        RepositorySubject::identity(rid.into_sha1().unwrap(), [1; 32], blob.into_sha1().unwrap()),
    );
    let tx = Transaction::<Identity, Repository>::new(
        identity::TYPENAME.clone(),
        vec![identity::Action::Revision {
            title: cob::Title::new("Initial revision").unwrap(),
            description: String::new(),
            blob,
            parent: None,
            signature: document_signature.into(),
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
        owner,
        RepositorySubject::cob(
            rid.into_sha1().unwrap(),
            [2; 32],
            unsigned.tree(),
            unsigned.as_bytes(),
        ),
    );
    let identity = cob::attach_native(
        repo.raw(),
        unsigned,
        rid.into_sha1().unwrap(),
        [2; 32],
        &signature,
    )
    .unwrap()
    .id;
    receipts.accept(identity, SubjectKind::Cob, [2; 32]);
    receipts.accept(identity, SubjectKind::Identity, [1; 32]);
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
        [3; 32],
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
        match owner {
            RepositoryKey::Bls12381G2(key) => key,
            _ => unreachable!(),
        },
        initial_refs.subject().unwrap(),
    );
    let previous_sigrefs = initial_refs
        .attach(repo.raw(), initial_signature)
        .unwrap()
        .oid();
    receipts.accept(previous_sigrefs, SubjectKind::SignedRefs, [3; 32]);
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
    let signing = ReviewSigningContext {
        chain_id: 2026,
        genesis_hash: [7; 32],
        epoch: 2,
        material_version: 7,
        material_hash: [9; 32],
        previous_receipt_hash: [17; 32],
    };
    let prepared = prepare_patch_review(&repo, &approval, identity, None, &signing).unwrap();
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
    let signature = sign(
        &providers,
        match owner {
            RepositoryKey::Bls12381G2(key) => key,
            _ => unreachable!(),
        },
        prepared.subject.clone(),
    );
    let mut other_material = signing.clone();
    other_material.material_version += 1;
    let stale = prepare_patch_review(&repo, &approval, identity, None, &other_material).unwrap();
    assert!(signature.verify(&stale.subject, owner).is_err());
    let mut other_network = signing.clone();
    other_network.genesis_hash = [8; 32];
    let replay = prepare_patch_review(&repo, &approval, identity, None, &other_network).unwrap();
    assert!(signature.verify(&replay.subject, owner).is_err());
    let mut other_approval = approval.clone();
    other_approval.approval.proposal_id += U256::from(1);
    let replay = prepare_patch_review(&repo, &other_approval, identity, None, &signing).unwrap();
    assert!(signature.verify(&replay.subject, owner).is_err());
    let (review, prepared_refs) = prepared
        .attach(&repo, &signature, accepted_refs.clone(), previous_sigrefs)
        .unwrap();
    assert_eq!(
        repo.reference_oid(&owner, &git::refs::storage::SIGREFS_BRANCH)
            .unwrap(),
        previous_sigrefs
    );
    let refs_signature = sign(
        &providers,
        match owner {
            RepositoryKey::Bls12381G2(key) => key,
            _ => unreachable!(),
        },
        prepared_refs.subject().unwrap(),
    );
    let signed_refs = prepared_refs
        .attach(repo.raw(), refs_signature)
        .unwrap()
        .oid();
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
    assert!(
        accepted
            .revision(&later_revision)
            .unwrap()
            .review_by(&owner)
            .is_none()
    );
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
}
