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
    let signature = sign(
        &key,
        RepositorySubject::identity(rid.into_sha1().unwrap(), [0; 32], blob.into_sha1().unwrap()),
    );
    let action = Action::Revision {
        title: cob::Title::new("Initial revision").unwrap(),
        description: String::new(),
        blob,
        parent: None,
        signature: signature.into(),
    };
    let embeds = vec![Embed::<Uri>::store("radicle.json", &bytes, repo.raw()).unwrap()];
    let author: Author = "Outbe Network <network@outbe.invalid> 1700000000 +0000"
        .parse()
        .unwrap();
    let tx =
        Transaction::<Identity, storage::Repository>::new(TYPENAME.clone(), vec![action], embeds);
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
        [0; 32],
        prepared.tree(),
        prepared.as_bytes(),
    );
    let signature = sign(&key, subject);
    let entry = radicle_cob::attach_native(
        repo.raw(),
        prepared,
        rid.into_sha1().unwrap(),
        [0; 32],
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
