use blst::min_sig;
use crypto::signature::Keypair as _;
use crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject};

use crate::storage::ReadRepository as _;
use crate::storage::refs::sigrefs::{
    SignedRefsReader, git::Committer, read, write::PreparedSignedRefs,
};
use crate::storage::refs::{IDENTITY_ROOT, Refs, SIGREFS_PARENT};
use crate::test::setup::NodeWithRepo;

fn envelope(key: &min_sig::SecretKey, subject: RepositorySubject) -> BlsEnvelope {
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
fn externally_signed_refs_bind_actual_repository_parent_and_per_commit_context() {
    let NodeWithRepo { node: _node, repo } = NodeWithRepo::default();
    let raw = &repo.backend;
    let key = min_sig::SecretKey::key_gen(&[42; 32], &[]).unwrap();
    let authority =
        RepositoryKey::from(BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap());
    let root = repo.identity_root().unwrap();
    let refs = Refs::from(
        [
            (IDENTITY_ROOT.to_ref_string(), root),
            (
                crate::git::fmt::refname!("refs/heads/main"),
                repo.head().unwrap().1,
            ),
        ]
        .into_iter(),
    );
    let prepare = |parent, context| {
        PreparedSignedRefs::new(
            refs.clone(),
            repo.id,
            authority,
            context,
            parent,
            Committer::stable(&authority),
            "Native external refs\n".into(),
        )
        .unwrap()
    };
    let before: Vec<_> = raw
        .references()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.name().unwrap().to_owned(), r.target())
        })
        .collect();
    let prepared = prepare(None, [0; 32]);
    let signature = envelope(&key, prepared.subject().unwrap());
    let parent = prepared.attach(raw, signature.clone()).unwrap();
    let parent_id = *parent.commit().oid();
    assert_eq!(parent.commit().refs(), &refs);
    let reader = read::RepositoryVerifier::new(authority, |at| {
        assert_eq!(at, parent_id);
        Ok([0; 32])
    });
    assert!(
        read::Verify::verify(
            &reader,
            repo.id,
            parent_id,
            Some(crate::git::Oid::from_sha1([0; 20])),
            &refs.canonical(),
            parent.commit().signature(),
        )
        .is_err()
    );
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(parent_id), raw, &reader)
            .read()
            .is_ok()
    );

    let context = [7; 32];
    let prepared = prepare(Some(parent_id), context);
    assert!(prepared.attach(raw, signature).is_err());
    let prepared = prepare(Some(parent_id), context);
    let signature = envelope(&key, prepared.subject().unwrap());
    let child = prepared.attach(raw, signature).unwrap();
    let child_id = *child.commit().oid();
    assert_eq!(child.commit().parent(), Some(&parent_id));
    assert_eq!(child.commit().refs(), &refs);
    let read = |repository, owner, context| {
        let verifier = read::RepositoryVerifier::new(owner, |at| {
            assert_eq!(at, child_id);
            Ok(context)
        });
        SignedRefsReader::new(repository, read::Tip::Commit(child_id), raw, &verifier).read()
    };
    assert!(read(repo.id, authority, context).is_ok());
    assert!(read(repo.id, authority, [0; 32]).is_err());
    assert!(
        read(
            crate::git::Oid::from_sha1([1; 20]).into(),
            authority,
            context
        )
        .is_err()
    );
    let other = min_sig::SecretKey::key_gen(&[43; 32], &[]).unwrap();
    let other = BlsPublicKey::from_bytes(other.sk_to_pk().to_bytes()).unwrap();
    assert!(read(repo.id, other.into(), context).is_err());
    let unavailable =
        read::RepositoryVerifier::new(authority, |_| Err(crypto::signature::Error::new()));
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(child_id), raw, &unavailable)
            .read()
            .is_err()
    );
    let ed = crypto::SigningKey::mock(42).verifying_key();
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(child_id), raw, &ed)
            .read()
            .is_err()
    );
    let original = raw
        .odb()
        .unwrap()
        .read(child_id.into())
        .unwrap()
        .data()
        .to_vec();
    let original = String::from_utf8(original).unwrap();
    let changed = original.replace(
        &format!("parent {parent_id}"),
        &format!("parent {}", repo.head().unwrap().1),
    );
    let id = raw
        .odb()
        .unwrap()
        .write(git2::ObjectType::Commit, changed.as_bytes())
        .unwrap();
    let verifier = read::RepositoryVerifier::new(authority, |_| Ok(context));
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(id.into()), raw, &verifier)
            .read()
            .is_err()
    );
    use crate::storage::refs::sigrefs::git::object::{self, Writer as _};
    let mut altered_refs = refs.clone();
    altered_refs.add_parent(parent_id);
    altered_refs.insert(crate::git::fmt::refname!("refs/heads/forged"), root);
    let tree = raw
        .write_tree(
            object::RefsEntry {
                path: "refs".into(),
                content: altered_refs.canonical(),
            },
            object::SignatureEntry {
                path: "signature".into(),
                content: child.commit().signature().to_bytes(),
            },
        )
        .unwrap();
    let old_tree = raw.find_commit(child_id.into()).unwrap().tree_id();
    let changed = original.replace(&format!("tree {old_tree}"), &format!("tree {tree}"));
    let id = raw
        .odb()
        .unwrap()
        .write(git2::ObjectType::Commit, changed.as_bytes())
        .unwrap();
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(id.into()), raw, &verifier)
            .read()
            .is_err()
    );
    let after: Vec<_> = raw
        .references()
        .unwrap()
        .map(|r| {
            let r = r.unwrap();
            (r.name().unwrap().to_owned(), r.target())
        })
        .collect();
    assert_eq!(before, after);
    let reopened = git2::Repository::open_bare(raw.path()).unwrap();
    assert!(
        SignedRefsReader::new(repo.id, read::Tip::Commit(child_id), &reopened, &verifier)
            .read()
            .is_ok()
    );
}

#[test]
fn native_preparation_rejects_missing_root_ed_owner_and_preinserted_parent() {
    let NodeWithRepo { node: _node, repo } = NodeWithRepo::default();
    let key = min_sig::SecretKey::key_gen(&[42; 32], &[]).unwrap();
    let authority =
        RepositoryKey::from(BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap());
    let root = repo.identity_root().unwrap();
    let valid = Refs::from([(IDENTITY_ROOT.to_ref_string(), root)].into_iter());
    let make = |refs, owner| {
        PreparedSignedRefs::new(
            refs,
            repo.id,
            owner,
            [0; 32],
            None,
            Committer::stable(&owner),
            "Native refs".into(),
        )
    };
    assert!(make(Refs::new(), authority).is_err());
    let mut refs = valid.clone();
    refs.insert(SIGREFS_PARENT.to_ref_string(), root);
    assert!(make(refs, authority).is_err());
    assert!(
        PreparedSignedRefs::new(
            valid.clone(),
            repo.id,
            authority,
            [0; 32],
            Some(crate::git::Oid::from_sha1([0; 20])),
            Committer::stable(&authority),
            "Native refs".into(),
        )
        .is_err()
    );
    use crypto::Signer as _;
    assert!(make(valid, (*crypto::SigningKey::mock(42).public_key()).into()).is_err());
}
