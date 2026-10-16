//! Diagnostic only: a local BLS test secret isolates admission, not DKG/current-set security.
use std::{collections::BTreeSet, sync::{Arc, RwLock}};
use blst::min_sig;
use nonempty::NonEmpty;
use radicle::{
    cob::{self, Embed, Evaluate, identity, patch::{self, Patch}},
    crypto::{self, signature::Keypair as _},
    git,
    identity::{doc::{Doc, Visibility}, Project},
    storage::{ReadStorage as _, WriteStorage as _, publication::{EntryAuthorization, EntryRequest, Error, PublicationPolicy}},
};
use radicle_cob::change::Storage as _;

const CONTEXT: [u8; 32] = [7; 32];
#[derive(Debug, Default)]
struct Policy { denied: RwLock<BTreeSet<git::Oid>> }
impl PublicationPolicy for Policy {
    fn id(&self) -> &str { "semantic-reproduction" }
    fn authorize_entry(&self, request: &EntryRequest) -> Result<EntryAuthorization, Error> {
        if self.denied.read().unwrap().contains(&request.entry) { return Err(Error::Denied); }
        Ok(EntryAuthorization::accepted(request.clone(), CONTEXT, [9; 32]))
    }
}
fn envelope(secret: &min_sig::SecretKey, subject: crypto::RepositorySubject) -> crypto::BlsEnvelope {
    let mut message = vec![25];
    message.extend_from_slice(b"radicle.repository.bls.v1");
    message.extend_from_slice(subject.as_bytes());
    crypto::BlsEnvelope::attach(subject,
        crypto::BlsPublicKey::from_bytes(secret.sk_to_pk().to_bytes()).unwrap(),
        secret.sign(&message, b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_", &[]).to_bytes(),
    ).unwrap()
}
fn native(
    raw: &git::raw::Repository, repo: [u8;20], secret: &min_sig::SecretKey,
    template: radicle_cob::change::Template, resource: Option<git::Oid>,
) -> cob::Entry {
    // Reuse the production tree/trailer builder; the temporary Ed commit is unreferenced.
    let temporary = raw.store(resource, vec![], &crypto::SigningKey::mock(90), template).unwrap();
    let commit = raw.find_commit(temporary.id.into()).unwrap();
    let mut bytes = format!("tree {}\n", commit.tree_id());
    for parent in commit.parent_ids() { bytes.push_str(&format!("parent {parent}\n")); }
    bytes.push_str("author Network <network@outbe.invalid> 1700000000 +0000\ncommitter Network <network@outbe.invalid> 1700000000 +0000\n\n");
    bytes.push_str(commit.message().unwrap());
    if !bytes.ends_with('\n') { bytes.push('\n'); }
    let prepared = radicle_git_metadata::commit::bls::UnsignedCommit::parse(bytes.as_bytes()).unwrap();
    let subject = crypto::RepositorySubject::cob(repo, CONTEXT, prepared.tree(), prepared.as_bytes());
    let entry = radicle_cob::attach_native(raw, prepared, repo, CONTEXT, &envelope(secret, subject)).unwrap();
    assert!(entry.valid_signatures());
    entry
}
fn identity_entry(
    raw: &git::raw::Repository, repo: [u8;20], secret: &min_sig::SecretKey,
    doc: &Doc, parent: Option<git::Oid>,
) -> cob::Entry {
    let (blob, bytes) = doc.encode().unwrap();
    let signature = crypto::RepositorySignature::from(envelope(secret,
        crypto::RepositorySubject::identity(repo, CONTEXT, blob.into_sha1().unwrap())));
    let action = identity::Action::Revision {
        title: "Identity".parse().unwrap(), description: String::new(), blob, signature, parent,
    };
    native(raw, repo, secret, radicle_cob::change::Template {
        type_name: "xyz.radicle.id".parse().unwrap(), tips: parent.into_iter().collect(),
        message: "Identity\n".into(), embeds: vec![Embed { name: "radicle.json".into(), content: bytes }],
        contents: NonEmpty::new(serde_json::to_vec(&action).unwrap()),
    }, None)
}
fn main() {
    let secret = min_sig::SecretKey::key_gen(&[37;32], &[]).unwrap();
    let owner = crypto::RepositoryKey::from(crypto::BlsPublicKey::from_bytes(secret.sk_to_pk().to_bytes()).unwrap());
    let author = crypto::SigningKey::mock(42);
    let author_key = crypto::RepositoryKey::from(author.public_key());
    let doc = Doc::initial(Project::new("governments".parse().unwrap(), String::new(), "main".parse().unwrap()).unwrap(), owner.into(), Visibility::Public);
    assert_eq!(doc.delegates().len(), 1);
    assert!(doc.is_delegate(&owner.into()));
    assert!(!doc.is_delegate(&author_key.into()));
    let repo_oid = doc.encode().unwrap().0;
    let temp = tempfile::tempdir().unwrap();
    let storage = radicle::storage::git::Storage::open(temp.path(), radicle::test::fixtures::user()).unwrap();
    let policy = Arc::new(Policy::default());
    storage.manage_repository(repo_oid.into(), owner, policy.clone()).unwrap();
    let repo = storage.create(repo_oid.into()).unwrap();
    let raw = &repo.backend;
    let root = identity_entry(raw, repo_oid.into_sha1().unwrap(), &secret, &doc, None);
    let verified_identity = <identity::Identity as Evaluate<_>>::init(&repo.load(root.id).unwrap(), &repo).unwrap();
    assert_eq!(verified_identity.root().doc().delegates().len(), 1);
    println!("initial_identity_verified=true sole_delegate_is_bls=true ed_author_is_delegate=false");
    raw.reference("refs/rad/id", root.id.into(), true, "Trusted initial identity").unwrap();

    let tree_id = raw.treebuilder(None).unwrap().write().unwrap();
    let tree = raw.find_tree(tree_id).unwrap();
    let signature = git::raw::Signature::new("Code", "code@example.invalid", &git::raw::Time::new(1700000000, 0)).unwrap();
    let code: git::Oid = raw.commit(None, &signature, &signature, "Code\n", &tree, &[]).unwrap().into();
    let actions = vec![
        patch::Action::Revision { description: String::new(), base: code, oid: code, resolves: Default::default() },
        patch::Action::Edit { title: "Proposed patch".parse().unwrap(), target: patch::MergeTarget::Delegates },
    ];
    let patch_root = raw.store(Some(root.id), vec![], &author, radicle_cob::change::Template {
        type_name: patch::TYPENAME.clone(), tips: vec![], message: "Patch\n".into(), embeds: vec![],
        contents: NonEmpty::from_vec(actions.iter().map(|a| serde_json::to_vec(a).unwrap()).collect()).unwrap(),
    }).unwrap();
    let object: cob::ObjectId = patch_root.id.into();
    let reference = format!("refs/namespaces/{author_key}/refs/cobs/xyz.radicle.patch/{object}");
    raw.reference(&reference, patch_root.id.into(), true, "Author patch").unwrap();
    raw.reference(&format!("refs/namespaces/{author_key}/refs/heads/main"), code.into(), true, "Author code branch").unwrap();
    let clean = cob::get::<Patch,_>(&repo, &patch::TYPENAME, &object).unwrap().unwrap().object;
    assert!(!matches!(clean.state(), patch::State::Merged { .. }));

    let bad_doc = doc.clone().with_edits(|raw| { raw.delegate(author_key.into()); }).unwrap();
    let denied_identity = identity_entry(raw, repo_oid.into_sha1().unwrap(), &secret, &bad_doc, Some(root.id));
    policy.denied.write().unwrap().insert(denied_identity.id);
    assert!(repo.load(denied_identity.id).is_err());
    assert!(radicle_cob::load_native(raw, denied_identity.id, repo_oid.into_sha1().unwrap(), CONTEXT).unwrap().valid_signatures());
    println!("candidate_identity_crypto_valid=true candidate_identity_policy_denied=true");
    for (label, resource) in [("accepted_identity_control", root.id), ("denied_identity_resource", denied_identity.id)] {
        let action = patch::Action::Merge { revision: patch_root.id.into(), commit: code };
        let entry = raw.store(Some(resource), vec![], &author, radicle_cob::change::Template {
            type_name: patch::TYPENAME.clone(), tips: vec![patch_root.id], message: "Author merge attempt\n".into(), embeds: vec![],
            contents: NonEmpty::new(serde_json::to_vec(&action).unwrap()),
        }).unwrap();
        assert!(entry.valid_signatures());
        raw.reference(&reference, entry.id.into(), true, "Diagnostic candidate tip").unwrap();
        let patch = cob::get::<Patch,_>(&repo, &patch::TYPENAME, &object).unwrap().unwrap().object;
        let merged = matches!(patch.state(), patch::State::Merged { .. });
        println!("case={label} state={:?} merged={merged}", patch.state());
        if label == "accepted_identity_control" { assert!(!merged); }
    }
    println!("LIMIT: local production evaluator diagnostic; no Vote/DKG, full fetch/cache, or release exploit claim.");
}
