use super::*;
use crate::cob::identity::{Action, Identity, TYPENAME};
use crate::crypto::{BlsEnvelope, BlsPublicKey, RepositorySubject};
use crate::identity::{Doc, doc::Visibility, project::Project};
use blst::min_sig;
use radicle_git_metadata::author::Author;

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

#[test]
fn prepare_and_attach_initial_bls_identity_without_ed_owner_or_ref_changes() {
    let temp = tempfile::tempdir().unwrap();
    let key = min_sig::SecretKey::key_gen(&[73; 32], &[]).unwrap();
    let authority =
        RepositoryKey::from(BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap());
    let project = Project::new(
        "governments".parse().unwrap(),
        "Native BLS preparation".into(),
        git::fmt::refname!("main"),
    )
    .unwrap();
    let doc = Doc::initial(project, authority.into(), Visibility::Public);
    let (blob, bytes) = doc.encode().unwrap();
    let rid: RepoId = blob.into();
    let path = temp.path().join(rid.canonical());
    drop(git::raw::Repository::init_bare(&path).unwrap());
    let repo = storage::Repository::open(&path, rid).unwrap();
    let document_signature = sign(
        &key,
        RepositorySubject::identity(rid.into_sha1().unwrap(), [7; 32], blob.into_sha1().unwrap()),
    );
    let action = Action::Revision {
        title: cob::Title::new("Initial revision").unwrap(),
        description: String::new(),
        blob,
        parent: None,
        signature: document_signature.clone().into(),
    };
    let embeds = vec![Embed::<Uri>::store("radicle.json", &bytes, repo.raw()).unwrap()];
    let author: Author = "Outbe Network <network@outbe.invalid> 1700000000 +0000"
        .parse()
        .unwrap();
    let tx = Transaction::<Identity, storage::Repository>::new(
        TYPENAME.clone(),
        vec![action.clone()],
        embeds,
    );
    let prepared = tx
        .prepare_native(
            &repo,
            None,
            vec![],
            "Bootstrap repository\n".into(),
            author.clone(),
        )
        .unwrap();
    let subject = RepositorySubject::cob(
        rid.into_sha1().unwrap(),
        [8; 32],
        prepared.tree(),
        prepared.as_bytes(),
    );
    let signature = sign(&key, subject);
    let entry = radicle_cob::attach_native(
        repo.raw(),
        prepared,
        rid.into_sha1().unwrap(),
        [8; 32],
        &signature,
    )
    .unwrap();
    assert!(entry.valid_signatures());
    assert_eq!(*entry.author(), authority);
    let identity = Identity::from_root(Op::try_from(&entry).unwrap(), &repo).unwrap();
    assert_eq!(identity.doc, doc);
    assert_eq!(identity.doc.delegates().len(), 1);
    assert_eq!(**identity.doc.delegates().first(), authority);
    assert_eq!(identity.doc.threshold(), 1);
    let raw = repo
        .raw()
        .odb()
        .unwrap()
        .read(entry.id.into())
        .unwrap()
        .data()
        .to_vec();
    let altered = String::from_utf8(raw)
        .unwrap()
        .replace("Bootstrap repository\n", "Tampered identity\n");
    let forged = repo
        .raw()
        .odb()
        .unwrap()
        .write(git::raw::ObjectType::Commit, altered.as_bytes())
        .unwrap();
    assert!(
        Doc::load_at(forged.into(), &repo).is_err(),
        "direct identity lookup accepted tampered BLS COB metadata"
    );
    assert_eq!(Doc::load_at(entry.id, &repo).unwrap().doc, doc);
    let wrong_document_signature = sign(
        &key,
        RepositorySubject::identity([42; 20], [7; 32], blob.into_sha1().unwrap()),
    );
    let changed_document = String::from_utf8(bytes.clone())
        .unwrap()
        .replace("Native BLS preparation", "Unbound document body")
        .into_bytes();
    for (signature, contents) in [
        (wrong_document_signature, bytes.clone()),
        (document_signature, changed_document),
    ] {
        let mut action = action.clone();
        let Action::Revision {
            signature: field, ..
        } = &mut action
        else {
            unreachable!()
        };
        *field = signature.into();
        let tx = Transaction::<Identity, storage::Repository>::new(
            TYPENAME.clone(),
            vec![action],
            vec![Embed::<Uri>::store("radicle.json", &contents, repo.raw()).unwrap()],
        );
        let prepared = tx
            .prepare_native(
                &repo,
                None,
                vec![],
                "Bootstrap repository\n".into(),
                author.clone(),
            )
            .unwrap();
        let signature = sign(
            &key,
            RepositorySubject::cob(
                rid.into_sha1().unwrap(),
                [8; 32],
                prepared.tree(),
                prepared.as_bytes(),
            ),
        );
        let invalid_identity = radicle_cob::attach_native(
            repo.raw(),
            prepared,
            rid.into_sha1().unwrap(),
            [8; 32],
            &signature,
        )
        .unwrap();
        assert!(
            invalid_identity.valid_signatures(),
            "fixture must have valid outer COB signature"
        );
        assert!(
            Doc::load_at(invalid_identity.id, &repo).is_err(),
            "direct lookup accepted unbound document/signature"
        );
    }
    assert!(repo.raw().references().unwrap().next().is_none());
    let empty = Transaction::<Identity, storage::Repository>::default();
    assert!(matches!(
        empty.prepare_native(&repo, None, vec![], "Empty\n".into(), author),
        Err(Error::EmptyTransaction)
    ));
    drop(repo);
    let reopened = storage::Repository::open(&path, rid).unwrap();
    let loaded =
        <storage::Repository as radicle_cob::change::Storage>::load(&reopened, entry.id).unwrap();
    assert_eq!(
        Identity::from_root(Op::try_from(&loaded).unwrap(), &reopened).unwrap(),
        identity
    );
    assert!(reopened.raw().references().unwrap().next().is_none());
}

#[test]
fn direct_managed_identity_read_requires_both_current_entry_receipts() {
    use crate::crypto::SubjectKind;
    use crate::storage::WriteStorage as _;
    use crate::storage::publication::{self, EntryAuthorization, EntryRequest, PublicationPolicy};
    use std::sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Debug, Default)]
    struct Receipts {
        entry: OnceLock<git::Oid>,
        document: AtomicBool,
    }
    impl PublicationPolicy for Receipts {
        fn id(&self) -> &str {
            "test.identity-receipts"
        }
        fn authorize_entry(
            &self,
            request: &EntryRequest,
        ) -> Result<EntryAuthorization, publication::Error> {
            if self.entry.get() != Some(&request.entry) {
                return Err(publication::Error::Denied);
            }
            let (context, receipt) = match request.kind {
                SubjectKind::Cob => ([3; 32], [33; 32]),
                SubjectKind::Identity if self.document.load(Ordering::SeqCst) => {
                    ([2; 32], [22; 32])
                }
                _ => return Err(publication::Error::Denied),
            };
            Ok(EntryAuthorization::accepted(
                request.clone(),
                context,
                receipt,
            ))
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let key = min_sig::SecretKey::key_gen(&[74; 32], &[]).unwrap();
    let owner = RepositoryKey::from(BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap());
    let doc = Doc::initial(
        Project::new(
            "governments".parse().unwrap(),
            "Managed identity read".into(),
            git::fmt::refname!("main"),
        )
        .unwrap(),
        owner.into(),
        Visibility::Public,
    );
    let (blob, bytes) = doc.encode().unwrap();
    let rid: RepoId = blob.into();
    let storage = crate::Storage::open(temp.path(), crate::test::fixtures::user()).unwrap();
    let receipts = Arc::new(Receipts::default());
    storage
        .manage_repository(rid, owner, receipts.clone())
        .unwrap();
    let repo = storage.create(rid).unwrap();
    let signature = sign(
        &key,
        RepositorySubject::identity(rid.into_sha1().unwrap(), [2; 32], blob.into_sha1().unwrap()),
    );
    let action = Action::Revision {
        title: cob::Title::new("Initial revision").unwrap(),
        description: String::new(),
        blob,
        parent: None,
        signature: signature.into(),
    };
    let tx = Transaction::<Identity, storage::Repository>::new(
        TYPENAME.clone(),
        vec![action],
        vec![Embed::<Uri>::store("radicle.json", &bytes, repo.raw()).unwrap()],
    );
    let author: Author = "Outbe Network <network@outbe.invalid> 1700000000 +0000"
        .parse()
        .unwrap();
    let prepared = tx
        .prepare_native(&repo, None, vec![], "Bootstrap repository\n".into(), author)
        .unwrap();
    let signature = sign(
        &key,
        RepositorySubject::cob(
            rid.into_sha1().unwrap(),
            [3; 32],
            prepared.tree(),
            prepared.as_bytes(),
        ),
    );
    let entry = radicle_cob::attach_native(
        repo.raw(),
        prepared,
        rid.into_sha1().unwrap(),
        [3; 32],
        &signature,
    )
    .unwrap();
    assert!(entry.valid_signatures());
    assert!(
        Doc::load_at(entry.id, &repo).is_err(),
        "no receipt must fail closed"
    );
    let object_id = entry.id.into();
    let owner_ref = git::refs::storage::cob(&owner, &TYPENAME, &object_id);
    let publish = |tip| {
        <storage::Repository as radicle_cob::object::Storage>::update(
            &repo, &owner, &TYPENAME, &object_id, &tip,
        )
    };
    let drafts = crate::storage::git::cob::DraftStore::new(&repo, owner);
    let draft_ref = git::refs::storage::draft::cob(&owner, &TYPENAME, &object_id);
    let publish_draft =
        |tip| radicle_cob::object::Storage::update(&drafts, &owner, &TYPENAME, &object_id, &tip);
    assert!(publish(entry.id).is_err(), "local write requires a receipt");
    assert!(publish_draft(entry.id).is_err());
    assert!(repo.raw().references().unwrap().next().is_none());
    receipts.entry.set(entry.id).unwrap();
    assert!(
        <storage::Repository as radicle_cob::change::Storage>::load(&repo, entry.id).is_ok(),
        "the containing COB receipt is available"
    );
    assert!(
        matches!(
            Doc::load_at(entry.id, &repo),
            Err(crate::identity::DocError::Publication(
                publication::Error::Denied
            ))
        ),
        "a COB receipt cannot replace a document receipt"
    );
    assert!(
        publish(entry.id).is_err(),
        "identity publication also requires the document receipt"
    );
    assert!(publish_draft(entry.id).is_err());
    assert!(repo.raw().references().unwrap().next().is_none());
    receipts.document.store(true, Ordering::SeqCst);
    let accepted = Doc::load_at(entry.id, &repo).unwrap();
    assert_eq!(accepted.doc, doc);
    assert_eq!(accepted.commit, entry.id);
    assert_eq!(Doc::load_at(entry.id, &repo).unwrap(), accepted);
    publish(entry.id).unwrap();
    publish_draft(entry.id).unwrap();
    assert_eq!(
        repo.raw().refname_to_id(owner_ref.as_str()).unwrap(),
        entry.id.into()
    );
    let author_key = crate::crypto::SigningKey::mock(75);
    use crate::crypto::Signer as _;
    let author_doc = Doc::initial(
        Project::new(
            "author-repo".parse().unwrap(),
            "Independent Ed identity".into(),
            git::fmt::refname!("main"),
        )
        .unwrap(),
        (*author_key.public_key()).into(),
        Visibility::Public,
    );
    let (author_blob, author_bytes, author_signature) = author_doc.sign(&author_key).unwrap();
    let embedded = repo.raw().blob(&author_bytes).unwrap();
    assert_eq!(author_blob, embedded);
    let action = Action::Revision {
        title: cob::Title::new("Author identity").unwrap(),
        description: String::new(),
        blob: author_blob,
        parent: None,
        signature: author_signature.into(),
    };
    use radicle_cob::change::Storage as _;
    let author_entry = repo
        .raw()
        .store(
            None,
            vec![],
            &author_key,
            radicle_cob::change::Template {
                type_name: TYPENAME.clone(),
                tips: vec![],
                message: "Independent author identity".into(),
                embeds: vec![radicle_cob::Embed {
                    name: "radicle.json".into(),
                    content: embedded.into(),
                }],
                contents: radicle_cob::Contents::new(encoding::encode(action).unwrap()),
            },
        )
        .unwrap();
    assert!(author_entry.valid_signatures());
    assert!(
        <storage::Repository as radicle_cob::change::Storage>::load(&repo, author_entry.id).is_ok(),
        "ordinary author COB loading stays native"
    );
    assert!(
        matches!(
            Doc::load_at(author_entry.id, &repo),
            Err(crate::identity::DocError::Publication(
                publication::Error::Denied
            ))
        ),
        "Ed author identity cannot replace the configured BLS owner identity"
    );
    assert_eq!(Doc::load_at(entry.id, &repo).unwrap(), accepted);
    assert!(publish(author_entry.id).is_err());
    assert!(publish_draft(author_entry.id).is_err());
    assert_eq!(
        repo.raw().refname_to_id(owner_ref.as_str()).unwrap(),
        entry.id.into()
    );
    assert_eq!(
        repo.raw().refname_to_id(draft_ref.as_str()).unwrap(),
        entry.id.into()
    );
}
