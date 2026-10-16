use super::*;
use blst::min_sig;
use radicle::cob::identity::{self, Identity};
use radicle::cob::patch::cache::Patches as _;
use radicle::cob::patch::{self, Patch, Patches, Verdict};
use radicle::cob::store::Transaction;
use radicle::cob::stream::CobStream as _;
use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject, SubjectKind};
use radicle::identity::{Doc, Project, RepoId, Visibility};
use radicle::storage::git::Repository;
use radicle::storage::publication::{self, EntryAuthorization, EntryRequest, PublicationPolicy};
use radicle::storage::refs::SIGREFS_BRANCH;
use radicle::storage::refs::sigrefs::{git::Committer, write::PreparedSignedRefs};

#[derive(Debug)]
struct Receipts {
    context: [u8; 32],
    entries: std::sync::Mutex<Vec<(git::Oid, SubjectKind)>>,
}

impl Receipts {
    fn new(context: [u8; 32]) -> Self {
        Self {
            context,
            entries: Default::default(),
        }
    }
    fn accept(&self, entry: git::Oid, kind: SubjectKind) {
        self.entries.lock().unwrap().push((entry, kind));
    }
}

impl PublicationPolicy for Receipts {
    fn id(&self) -> &str {
        "test.exact-receipts"
    }
    fn authorize_entry(
        &self,
        request: &EntryRequest,
    ) -> Result<EntryAuthorization, publication::Error> {
        if !self
            .entries
            .lock()
            .unwrap()
            .contains(&(request.entry, request.kind))
        {
            return Err(publication::Error::Denied);
        }
        Ok(EntryAuthorization::accepted(
            request.clone(),
            self.context,
            [17; 32],
        ))
    }
}

fn sign(key: &min_sig::SecretKey, subject: RepositorySubject) -> BlsEnvelope {
    let mut message = vec![25];
    message.extend_from_slice(b"radicle.repository.bls.v1");
    message.extend_from_slice(subject.as_bytes());
    BlsEnvelope::attach(
        subject,
        BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap(),
        key.sign(
            &message,
            b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_",
            &[],
        )
        .to_bytes(),
    )
    .unwrap()
}

fn metadata() -> Committer {
    Committer::new(
        "Native BLS <native@example.test> 1700000000 +0000"
            .parse()
            .unwrap(),
    )
}

fn attach(
    repo: &Repository,
    tx: Transaction<impl cob::store::Cob + cob::Evaluate<Repository>, Repository>,
    resource: Option<git::Oid>,
    tips: Vec<git::Oid>,
    key: &min_sig::SecretKey,
    context: [u8; 32],
) -> cob::Entry {
    let prepared = tx
        .prepare_native(
            repo,
            resource,
            tips,
            "Native operation\n".into(),
            metadata().into_inner(),
        )
        .unwrap();
    let signature = sign(
        key,
        RepositorySubject::cob(
            repo.id().into_sha1().unwrap(),
            context,
            prepared.tree(),
            prepared.as_bytes(),
        ),
    );
    cob::attach_native(
        repo.raw(),
        prepared,
        repo.id().into_sha1().unwrap(),
        context,
        &signature,
    )
    .unwrap()
}

fn signed_refs(
    repo: &Repository,
    owner: RepositoryKey,
    key: &min_sig::SecretKey,
    previous: Option<git::Oid>,
    context: [u8; 32],
) -> git::Oid {
    let prepared = PreparedSignedRefs::new(
        repo.references_of(&owner).unwrap(),
        repo.id(),
        owner,
        context,
        previous,
        metadata(),
        "Update signed refs\n".into(),
    )
    .unwrap();
    let signature = sign(key, prepared.subject().unwrap());
    let commit = prepared.attach(repo.raw(), signature).unwrap();
    let at = commit.oid();
    repo.raw()
        .reference(
            SIGREFS_BRANCH.with_namespace((&owner).into()).as_str(),
            at.into(),
            true,
            "publish static BLS refs fixture",
        )
        .unwrap();
    at
}

fn code_commit(repo: &Repository, parent: Option<git::Oid>, text: &[u8]) -> git::Oid {
    let blob = repo.raw().blob(text).unwrap();
    let mut builder = repo.raw().treebuilder(None).unwrap();
    builder.insert("README", blob, 0o100644).unwrap();
    let tree = repo.raw().find_tree(builder.write().unwrap()).unwrap();
    let author = git::raw::Signature::new(
        "Author",
        "author@example.test",
        &git::raw::Time::new(1700000000, 0),
    )
    .unwrap();
    let parents: Vec<_> = parent
        .into_iter()
        .map(|p| repo.raw().find_commit(p.into()).unwrap())
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

fn assert_cached_patch(
    repo: &Repository,
    id: patch::PatchId,
    expected: &Patch,
    path: &std::path::Path,
) {
    let revision = expected.latest().0;
    for rebuild in [false, true] {
        let db = cob::cache::StoreWriter::open(path)
            .unwrap()
            .with_migrations(cob::cache::migrate::ignore)
            .unwrap();
        let mut cache = patch::Cache::open(Patches::open(repo, ReadOnly).unwrap(), db);
        if rebuild {
            cache
                .write_all(|result, _| {
                    assert!(result.is_ok(), "cache rebuild: {result:?}");
                    std::ops::ControlFlow::Continue(())
                })
                .unwrap();
        } else {
            cache.write(&id).unwrap();
        }
        assert_eq!(cache.get(&id).unwrap().as_ref(), Some(expected));
        let found = cache.find_by_revision(&revision).unwrap().unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.revision_id, revision);
        assert_eq!(&found.patch, expected);
        assert_eq!(
            cache
                .list()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![(id, expected.clone())]
        );
        drop(cache);

        let db = cob::cache::StoreReader::reader(path).unwrap();
        db.check_version().unwrap();
        let cache = patch::Cache::reader(Patches::open(repo, ReadOnly).unwrap(), db);
        assert_eq!(cache.get(&id).unwrap().as_ref(), Some(expected));
        assert_eq!(
            &cache.find_by_revision(&revision).unwrap().unwrap().patch,
            expected
        );
        assert_eq!(cache.counts().unwrap().total(), 1);
    }
}

#[test_log::test]
fn two_runtime_nodes_fetch_bls_identity_ed_patch_and_external_bls_review() {
    fetch_native_repository(false, false);
}

#[test_log::test]
fn managed_runtime_nodes_require_receipts_for_bls_owner_but_not_ed_patch() {
    fetch_native_repository(true, true);
}

#[test_log::test]
fn ordinary_reader_fetches_network_bls_history_without_receipts() {
    fetch_native_repository(true, false);
}

fn fetch_native_repository(source_managed: bool, receiver_managed: bool) {
    #[derive(Debug)]
    struct MissingReceipts;
    impl PublicationPolicy for MissingReceipts {
        fn id(&self) -> &str {
            "test.missing-receipts"
        }
        fn authorize_entry(
            &self,
            _request: &EntryRequest,
        ) -> Result<EntryAuthorization, publication::Error> {
            Err(publication::Error::Denied)
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut alice = Node::init(tmp.path(), config::relay("bls-source"), 13);
    let bob = Node::init(tmp.path(), config::relay("bls-receiver"), 37);
    let mut guarded = Node::init(tmp.path(), config::relay("managed-receiver"), 57);
    let context = if source_managed { [3; 32] } else { [0; 32] };
    let source_receipts = std::sync::Arc::new(Receipts::new(context));
    let receiver_receipts = std::sync::Arc::new(Receipts::new(context));
    let key = min_sig::SecretKey::key_gen(&[73; 32], &[]).unwrap();
    let owner = RepositoryKey::from(BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap());
    let doc = Doc::initial(
        Project::new(
            "bls-repository".parse().unwrap(),
            "Native BLS fetch".into(),
            git::fmt::refname!("main"),
        )
        .unwrap(),
        owner.into(),
        Visibility::Public,
    );
    let (blob, bytes) = doc.encode().unwrap();
    let rid: RepoId = blob.into();
    guarded
        .storage
        .manage_repository(rid, owner, std::sync::Arc::new(MissingReceipts))
        .unwrap();
    assert!(!guarded.storage.contains(&rid).unwrap());
    let info = guarded.storage.info().clone();
    guarded.storage = radicle::Storage::open(guarded.storage.path(), info).unwrap();
    if source_managed {
        alice
            .storage
            .manage_repository(rid, owner, source_receipts.clone())
            .unwrap();
    }
    if receiver_managed {
        bob.storage
            .manage_repository(rid, owner, receiver_receipts.clone())
            .unwrap();
    }
    let repo = alice.storage.create(rid).unwrap();
    let signature = sign(
        &key,
        RepositorySubject::identity(rid.into_sha1().unwrap(), context, blob.into_sha1().unwrap()),
    );
    let tx = Transaction::<Identity, Repository>::new(
        identity::TYPENAME.clone(),
        vec![identity::Action::Revision {
            title: Title::new("Initial revision").unwrap(),
            description: String::new(),
            blob,
            parent: None,
            signature: signature.into(),
        }],
        vec![cob::Embed::<cob::Uri>::store("radicle.json", &bytes, repo.raw()).unwrap()],
    );
    let entry = attach(&repo, tx, None, vec![], &key, context);
    let identity = entry.id;
    source_receipts.accept(identity, SubjectKind::Cob);
    source_receipts.accept(identity, SubjectKind::Identity);
    let identity_ref = git::refs::storage::cob(&owner, &identity::TYPENAME, &identity.into());
    cob::object::Storage::update(
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
            identity_ref.as_str(),
            false,
            "static BLS identity",
        )
        .unwrap();
    repo.set_remote_identity_root_to(&owner, identity).unwrap();
    repo.set_identity_head_to(identity).unwrap();
    let base = code_commit(&repo, None, b"base\n");
    let main = git::fmt::Qualified::from(git::fmt::lit::refs_heads(git::fmt::refname!("main")));
    repo.raw()
        .reference(
            main.with_namespace((&owner).into()).as_str(),
            base.into(),
            false,
            "static BLS main",
        )
        .unwrap();
    let first_refs = signed_refs(&repo, owner, &key, None, context);
    source_receipts.accept(first_refs, SubjectKind::SignedRefs);
    repo.set_canonical_symbolic_refs("static BLS canonical refs")
        .unwrap();
    repo.set_default_branch_to_canonical_head().unwrap();
    assert_eq!(
        repo.identity_doc().unwrap().delegates().first().as_key(),
        &owner
    );
    let head = code_commit(&repo, Some(base), b"patch\n");
    let (patch_id, revision) = {
        let mut patches = Patches::open(&repo, WriteAs::new(&alice.secret_key)).unwrap();
        let mut cache = cob::cache::NoCache;
        let patch = patches
            .create(
                Title::new("Ordinary Ed author").unwrap(),
                "Before any BLS review",
                patch::MergeTarget::default(),
                base,
                head,
                &[],
                &mut cache,
            )
            .unwrap();
        (*patch.id(), patch.latest().0)
    };
    alice.policies.seed(&rid, Scope::All).unwrap();
    let mut alice = alice.spawn();
    let mut bob = bob.spawn();
    // The explicitly reopened storage retains its marker but has no provider.
    let mut guarded = guarded.spawn();
    guarded.connect(&alice);
    guarded.handle.seed(rid, Scope::All).unwrap();
    let denied = guarded
        .handle
        .fetch(rid, alice.id, time::Duration::from_secs(10), None)
        .unwrap();
    assert!(
        !denied.is_success(),
        "missing managed policy accepted: {denied:?}"
    );
    assert!(matches!(
        &denied,
        FetchResult::Failed { reason } if reason.contains("verification of rad/id")
    ));
    assert!(!guarded.storage.path_of(&rid).exists());
    assert!(!guarded.storage.contains(&rid).unwrap());
    println!("managed_receiver_without_policy={denied:?} canonical_repository_absent=true");
    bob.connect(&alice);
    bob.handle.seed(rid, Scope::All).unwrap();
    if receiver_managed {
        let denied = bob
            .handle
            .fetch(rid, alice.id, time::Duration::from_secs(10), None)
            .unwrap();
        assert!(
            !denied.is_success(),
            "managed bootstrap without receipt: {denied:?}"
        );
        assert!(!bob.storage.path_of(&rid).exists());
        println!("managed_bootstrap_without_receipt={denied:?}");
    }
    if receiver_managed {
        receiver_receipts.accept(identity, SubjectKind::Cob);
        receiver_receipts.accept(identity, SubjectKind::Identity);
        receiver_receipts.accept(first_refs, SubjectKind::SignedRefs);
    }
    let result = bob
        .handle
        .fetch(rid, alice.id, time::Duration::from_secs(10), None)
        .unwrap();
    assert!(result.is_success(), "BLS bootstrap fetch: {result:?}");
    let received = bob.storage.repository(rid).unwrap();
    assert_eq!(received.identity_doc().unwrap().doc, doc);
    let fetched = Patches::open(&received, ReadOnly)
        .unwrap()
        .get(&patch_id)
        .unwrap()
        .unwrap();
    assert_eq!(fetched.latest().0, revision);
    assert_eq!(
        fetched.author().public_key(),
        &RepositoryKey::from(alice.signer.public_key())
    );
    assert!(
        fetched
            .revision(&revision)
            .unwrap()
            .review_by(&owner)
            .is_none()
    );
    let cache_path = tmp.path().join("receiver-patches.db");
    assert_cached_patch(&received, patch_id, &fetched, &cache_path);
    let mut tx = Transaction::<Patch, Repository>::default();
    tx.review(revision, Some(Verdict::Accept), None, vec![])
        .unwrap();
    let review = attach(&repo, tx, Some(identity), vec![*revision], &key, context);
    source_receipts.accept(review.id, SubjectKind::Cob);
    cob::object::Storage::update(&repo, &owner, &patch::TYPENAME, &patch_id, &review.id).unwrap();
    let at = signed_refs(&repo, owner, &key, Some(first_refs), context);
    source_receipts.accept(at, SubjectKind::SignedRefs);
    alice.handle.announce_refs_for(rid, [owner]).unwrap();
    if receiver_managed {
        let denied = bob
            .handle
            .fetch(rid, alice.id, time::Duration::from_secs(10), None)
            .unwrap();
        assert!(
            !denied.is_success(),
            "managed Review without receipt: {denied:?}"
        );
        let unchanged = bob.storage.repository(rid).unwrap();
        assert_eq!(unchanged.remote(&owner).unwrap().refs.at, first_refs);
        assert_eq!(
            Patches::open(&unchanged, ReadOnly)
                .unwrap()
                .get(&patch_id)
                .unwrap()
                .unwrap(),
            fetched
        );
        assert_cached_patch(&unchanged, patch_id, &fetched, &cache_path);
        println!("managed_review_without_receipt={denied:?} preserved_sigrefs={first_refs}");
    }
    if receiver_managed {
        receiver_receipts.accept(review.id, SubjectKind::Cob);
        receiver_receipts.accept(at, SubjectKind::SignedRefs);
    }
    let result = bob
        .handle
        .fetch(rid, alice.id, time::Duration::from_secs(10), None)
        .unwrap();
    assert!(result.is_success(), "BLS Review fetch: {result:?}");
    let reopened = bob.storage.repository(rid).unwrap();
    let patch = Patches::open(&reopened, ReadOnly)
        .unwrap()
        .get(&patch_id)
        .unwrap()
        .unwrap();
    let accepted = patch
        .revision(&revision)
        .unwrap()
        .review_by(&owner)
        .unwrap();
    assert_eq!(accepted.verdict(), Some(Verdict::Accept));
    assert_eq!(accepted.id(), review.id.into());
    assert_cached_patch(&reopened, patch_id, &patch, &cache_path);
    assert!(reopened.validate().unwrap().is_empty());
    assert_eq!(reopened.remote(&owner).unwrap().refs.at, at);
    let operations = patch::PatchStream::init(patch_id, &reopened)
        .all()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        operations
            .iter()
            .any(|op| op.id == review.id && op.author == owner)
    );
    let identity_ops = identity::IdentityStream::init(identity.into(), &reopened)
        .all()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(identity_ops.len(), 1);
    assert_eq!(identity_ops[0].author, owner);
    println!(
        "repo={rid} owner={owner} identity={identity} patch={patch_id} revision={revision} review={} sigrefs={at} source_transport={} receiver_transport={}",
        review.id, alice.id, bob.id
    );

    // Change the actual refs payload while retaining the valid signature blob.
    let valid = repo.raw().find_commit(at.into()).unwrap();
    let tree = valid.tree().unwrap();
    let refs_blob = tree
        .get_name("refs")
        .unwrap()
        .to_object(repo.raw())
        .unwrap()
        .peel_to_blob()
        .unwrap();
    let changed = String::from_utf8(refs_blob.content().to_vec())
        .unwrap()
        .replace(&base.to_string(), &head.to_string());
    assert_ne!(changed.as_bytes(), refs_blob.content());
    let blob = repo.raw().blob(changed.as_bytes()).unwrap();
    let mut tree_builder = repo.raw().treebuilder(Some(&tree)).unwrap();
    tree_builder.insert("refs", blob, 0o100644).unwrap();
    let changed_tree = tree_builder.write().unwrap();
    let raw = repo
        .raw()
        .odb()
        .unwrap()
        .read(at.into())
        .unwrap()
        .data()
        .to_vec();
    let raw = String::from_utf8(raw).unwrap().replacen(
        &format!("tree {}\n", tree.id()),
        &format!("tree {changed_tree}\n"),
        1,
    );
    let forged = repo
        .raw()
        .odb()
        .unwrap()
        .write(git::raw::ObjectType::Commit, raw.as_bytes())
        .unwrap();
    repo.raw()
        .reference(
            SIGREFS_BRANCH.with_namespace((&owner).into()).as_str(),
            forged,
            true,
            "tampered static BLS refs fixture",
        )
        .unwrap();
    if receiver_managed {
        receiver_receipts.accept(forged.into(), SubjectKind::SignedRefs);
    }
    let result = bob
        .handle
        .fetch(rid, alice.id, time::Duration::from_secs(10), None)
        .unwrap();
    assert!(
        !result.is_success(),
        "tampered BLS refs accepted: {result:?}"
    );
    let preserved = bob.storage.repository(rid).unwrap();
    assert_eq!(preserved.remote(&owner).unwrap().refs.at, at);
    assert_eq!(
        Patches::open(&preserved, ReadOnly)
            .unwrap()
            .get(&patch_id)
            .unwrap()
            .unwrap(),
        patch
    );
    assert_cached_patch(&preserved, patch_id, &patch, &cache_path);
    println!("forged_sigrefs={forged} fetch={result:?} preserved_sigrefs={at}");
}
