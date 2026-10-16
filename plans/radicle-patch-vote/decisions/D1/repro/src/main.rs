use nonempty::NonEmpty;
use radicle::{
    crypto, git,
    storage::{
        ReadStorage as _, WriteStorage as _,
        publication::{EntryAuthorization, EntryRequest, Error, PublicationPolicy},
    },
};
use radicle_cob::change::Storage as _;
use std::{path::PathBuf, sync::Arc};

#[derive(Debug)]
struct Deny;
impl PublicationPolicy for Deny {
    fn id(&self) -> &str {
        "reproduction-deny"
    }
    fn authorize_entry(&self, _: &EntryRequest) -> Result<EntryAuthorization, Error> {
        Err(Error::Denied)
    }
}
fn bytes(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn main() {
    let root = PathBuf::from(std::env::args_os().nth(1).expect("worktree path required"));
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            root.join("vendor/heartwood/crates/radicle-crypto/src/repository/vectors.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let repo_oid = git::Oid::from_sha1(bytes(v["repo"].as_str().unwrap()).try_into().unwrap());
    let owner: crypto::RepositoryKey = v["key"].as_str().unwrap().parse().unwrap();
    let row = &v["vectors"][1];
    let context = bytes(row["context"].as_str().unwrap()).try_into().unwrap();
    let signature = crypto::BlsEnvelope::from_hex(row["envelope"].as_str().unwrap()).unwrap();
    let prepared = radicle_git_metadata::commit::bls::UnsignedCommit::parse(&bytes(
        v["unsigned_commit"].as_str().unwrap(),
    ))
    .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let storage =
        radicle::storage::git::Storage::open(temporary.path(), radicle::test::fixtures::user())
            .unwrap();
    storage
        .manage_repository(repo_oid.into(), owner, Arc::new(Deny))
        .unwrap();
    let repo = storage.create(repo_oid.into()).unwrap();
    let raw = &repo.backend;
    let op = br#"{"revision":"3333333333333333333333333333333333333333","type":"review","verdict":"accept"}"#;
    let manifest = br#"{"typeName":"xyz.radicle.patch","version":1}"#;
    let mut tree = raw.treebuilder(None).unwrap();
    tree.insert("0", raw.blob(op).unwrap(), git::raw::FileMode::Blob.into())
        .unwrap();
    tree.insert(
        "manifest",
        raw.blob(manifest).unwrap(),
        git::raw::FileMode::Blob.into(),
    )
    .unwrap();
    assert_eq!(tree.write().unwrap().as_bytes(), prepared.tree());
    drop(tree);
    let owner_entry = radicle_cob::attach_native(
        raw,
        prepared,
        repo_oid.into_sha1().unwrap(),
        context,
        &signature,
    )
    .unwrap();
    assert!(owner_entry.valid_signatures());
    assert_eq!(*owner_entry.author(), owner);
    assert!(repo.load(owner_entry.id).is_err());
    let signer = crypto::SigningKey::mock(42);
    for kind in ["pure", "related", "resource"] {
        let related = if kind == "related" {
            vec![owner_entry.id]
        } else {
            vec![]
        };
        let resource = (kind == "resource").then_some(owner_entry.id);
        let entry = raw
            .store(
                resource,
                related,
                &signer,
                radicle_cob::change::Template {
                    type_name: "xyz.radicle.patch".parse().unwrap(),
                    tips: vec![],
                    message: format!("Ed {kind}"),
                    embeds: vec![],
                    contents: NonEmpty::new(br#"{"type":"ordinary-ed-operation"}"#.to_vec()),
                },
            )
            .unwrap();
        assert!(entry.valid_signatures());
        let parent_is_denied_owner = raw
            .find_commit(entry.id.into())
            .unwrap()
            .parent_ids()
            .any(|p| owner_entry.id == p);
        let admitted = storage
            .repository(repo_oid.into())
            .unwrap()
            .load(entry.id)
            .is_ok();
        let direct_op = radicle::cob::Op::<serde_json::Value>::load(&repo, entry.id).is_ok();
        assert!(admitted && direct_op);
        assert_eq!(parent_is_denied_owner, kind != "pure");
        println!(
            "kind={kind} owner_parent_denied={parent_is_denied_owner} repository_load_admitted={admitted} direct_Op_load_admitted={direct_op}"
        );
    }
    println!(
        "OBSERVED: crypto-valid denied owner is rejected directly; its Ed resource/related descendants are still admitted. This reproduces an unfinished P2 admission gap, not a PASS of the required security contract."
    );
}
