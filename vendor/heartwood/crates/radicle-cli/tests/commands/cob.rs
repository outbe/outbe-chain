use std::path::Path;

use crate::util::environment::Environment;
use crate::{program_reports_version, test};
use radicle::cob::store::access::{ReadOnly, WriteAs};
use radicle::node::Handle;
use radicle::test::fixtures;

#[test]
fn rad_cob_update() {
    Environment::alice(["rad-init", "rad-cob-update"]);
}

#[test]
fn rad_cob_update_identity() {
    let mut environment = Environment::new();
    let profile = environment.profile("alice");
    let working = environment.tempdir().join("working");
    let home = &profile.home;

    let base = Path::new(env!("CARGO_MANIFEST_DIR"));

    std::fs::create_dir_all(base).unwrap();
    std::fs::create_dir_all(working.clone()).unwrap();

    // Set up a test repository.
    fixtures::repository(&working);

    test("examples/rad-init.md", &working, Some(home), []).unwrap();
    test(
        "examples/rad-cob-update-identity.md",
        &working,
        Some(home),
        [],
    )
    .unwrap();
}

#[test]
fn rad_cob_multiset() {
    // `rad-cob-multiset` is a `jq` script, which requires `jq` to be installed.
    // We test whether `jq` is installed, and have this test succeed if it is not.
    // Programmatic skipping of tests is not supported as of 2024-08.
    if !program_reports_version("jq") {
        return;
    }

    let mut environment = Environment::new();
    let profile = environment.profile("alice");
    let home = &profile.home;
    let working = environment.tempdir().join("working");

    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(base).unwrap();
    std::fs::create_dir_all(working.clone()).unwrap();

    // Copy over the script that implements the multiset COB.
    std::fs::copy(
        base.join("examples").join("rad-cob-multiset"),
        working.join("rad-cob-multiset"),
    )
    .unwrap();

    // Set up a test repository.
    fixtures::repository(&working);

    test("examples/rad-init.md", &working, Some(home), []).unwrap();
    test("examples/rad-cob-multiset.md", &working, Some(home), []).unwrap();
}

#[test]
fn rad_cob_log() {
    Environment::alice(["rad-init", "rad-cob-log"]);
}

#[test]
fn rad_cob_log_bls_native_operation() {
    use radicle::crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey, RepositorySubject};
    use radicle::storage::refs::sigrefs::git::Committer;
    use radicle::storage::{WriteRepository as _, WriteStorage as _};
    use std::process::Command;

    let mut environment = Environment::new();
    let profile = environment.profile("alice");
    let rid = radicle::git::Oid::from_sha1([42; 20]).into();
    let repo = profile.storage.create(rid).unwrap();
    let key = blst::min_sig::SecretKey::key_gen(&[74; 32], &[]).unwrap();
    let public = BlsPublicKey::from_bytes(key.sk_to_pk().to_bytes()).unwrap();
    let owner = RepositoryKey::from(public);
    let author = Committer::new(
        "BLS author <bls@example.test> 1700000000 +0000"
            .parse()
            .unwrap(),
    )
    .into_inner();
    let prepared = radicle::cob::prepare_native(repo.raw(), None, vec![], radicle::cob::change::Template {
        type_name: radicle::cob::patch::TYPENAME.clone(), tips: vec![], message: "Accept patch\n".into(), embeds: vec![],
        contents: radicle::cob::Contents::new(br#"{"revision":"3333333333333333333333333333333333333333","type":"review","verdict":"accept"}"#.to_vec()),
    }, author.clone(), author).unwrap();
    let subject = RepositorySubject::cob(
        rid.into_sha1().unwrap(),
        [0; 32],
        prepared.tree(),
        prepared.as_bytes(),
    );
    let mut message = vec![25];
    message.extend_from_slice(b"radicle.repository.bls.v1");
    message.extend_from_slice(subject.as_bytes());
    let envelope = BlsEnvelope::attach(
        subject,
        public,
        key.sign(
            &message,
            b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_POP_",
            &[],
        )
        .to_bytes(),
    )
    .unwrap();
    let entry = radicle::cob::attach_native(
        repo.raw(),
        prepared,
        rid.into_sha1().unwrap(),
        [0; 32],
        &envelope,
    )
    .unwrap();
    let object = entry.id.into();
    repo.raw()
        .reference(
            radicle::git::refs::storage::cob(&owner, &radicle::cob::patch::TYPENAME, &object)
                .as_str(),
            entry.id.into(),
            false,
            "native CLI fixture",
        )
        .unwrap();
    for format in ["pretty", "json"] {
        let result = Command::new(env!("CARGO_BIN_EXE_rad"))
            .env("RAD_HOME", profile.home.path())
            .args([
                "cob",
                "log",
                "--repo",
                &rid.to_string(),
                "--type",
                "xyz.radicle.patch",
                "--object",
                &entry.id.to_string(),
                "--format",
                format,
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains(&owner.to_string()));
        assert!(output.contains(&entry.id.to_string()));
        assert!(output.contains("accept"));
        if format == "json" {
            let op: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(
                op.get("author"),
                Some(&serde_json::Value::String(owner.to_string()))
            );
            assert_eq!(
                op.pointer("/actions/0/verdict")
                    .and_then(serde_json::Value::as_str),
                Some("accept")
            );
        }
    }
}

#[test]
fn rad_cob_show() {
    Environment::alice(["rad-init", "rad-cob-show"]);
}

#[test]
fn rad_cob_migrate() {
    let mut environment = Environment::new();
    let profile = environment.profile("alice");
    let home = &profile.home;

    home.cobs_db_mut()
        .unwrap()
        .raw_query(|conn| conn.execute("PRAGMA user_version = 0"))
        .unwrap();

    environment.repository(&profile);

    environment
        .tests(["rad-init", "rad-cob-migrate"], &profile)
        .unwrap();
}

#[test]
fn rad_cob_operations() {
    Environment::alice(["rad-init", "rad-cob-operations"]);
}

#[test]
fn test_cob_replication() {
    let mut environment = Environment::new();
    let working = tempfile::tempdir().unwrap();
    let mut alice = environment.node("alice");
    let bob = environment.node("bob");

    let rid = alice.project("heartwood", "");

    let mut alice = alice.spawn();
    let mut bob = bob.spawn();
    let events = alice.handle.events();

    alice.handle.follow(bob.id, None).unwrap();
    alice.connect(&bob);

    bob.routes_to(&[(rid, alice.id)]);
    bob.fork(rid, working.path()).unwrap();

    // Wait for Alice to fetch the clone refs.
    events
        .wait(
            |e| {
                matches!(
                    e,
                    radicle::node::Event::RefsFetched { updated, .. }
                    if updated.iter().any(|u| matches!(u, radicle::storage::RefUpdate::Created { .. }))
                )
                .then_some(())
            },
            std::time::Duration::from_secs(6),
        )
        .unwrap();

    let bob_repo = radicle::storage::ReadStorage::repository(&bob.storage, rid).unwrap();
    let mut bob_issues =
        radicle::cob::issue::Issues::open(&bob_repo, WriteAs::new(&bob.signer)).unwrap();
    let mut bob_cache = radicle::cob::cache::InMemory::default();
    let issue = bob_issues
        .create(
            radicle::cob::Title::new("Something's fishy").unwrap(),
            "I don't know what it is",
            &[],
            &[],
            [],
            &mut bob_cache,
        )
        .unwrap();
    log::debug!(target: "test", "Issue {} created", issue.id());

    // Make sure that Bob's issue refs announcement has a different timestamp than his fork's
    // announcement; otherwise, Alice will consider it stale.
    std::thread::sleep(std::time::Duration::from_millis(3));

    bob.handle.announce_refs_for(rid, [bob.id.into()]).unwrap();

    // Wait for Alice to fetch the issue refs.
    events
        .iter()
        .find(|e| matches!(e, radicle::node::Event::RefsFetched { .. }))
        .unwrap();

    let alice_repo = radicle::storage::ReadStorage::repository(&alice.storage, rid).unwrap();
    let alice_issues = radicle::cob::issue::Issues::open(&alice_repo, ReadOnly).unwrap();
    let alice_issue = alice_issues.get(issue.id()).unwrap().unwrap();

    assert_eq!(alice_issue.title(), "Something's fishy");
}

#[test]
fn test_cob_deletion() {
    let mut environment = Environment::new();
    let working = tempfile::tempdir().unwrap();
    let mut alice = environment.node("alice");
    let bob = environment.node("bob");

    let rid = alice.project("heartwood", "");

    let mut alice = alice.spawn();
    let mut bob = bob.spawn();

    alice
        .handle
        .seed(rid, radicle::node::policy::Scope::All)
        .unwrap();
    bob.handle
        .seed(rid, radicle::node::policy::Scope::All)
        .unwrap();
    alice.connect(&bob);
    bob.routes_to(&[(rid, alice.id)]);

    let alice_repo = radicle::storage::ReadStorage::repository(&alice.storage, rid).unwrap();
    let mut alice_issues =
        radicle::cob::issue::Cache::no_cache(&alice_repo, &alice.signer).unwrap();
    let issue = alice_issues
        .create(
            radicle::cob::Title::new("Something's fishy").unwrap(),
            "I don't know what it is",
            &[],
            &[],
            [],
        )
        .unwrap();
    let issue_id = issue.id();
    log::debug!(target: "test", "Issue {issue_id} created");

    bob.rad("clone", &[rid.to_string().as_str()], working.path())
        .unwrap();

    let bob_repo = radicle::storage::ReadStorage::repository(&bob.storage, rid).unwrap();
    let bob_issues = radicle::cob::issue::Issues::open(&bob_repo, ReadOnly).unwrap();
    assert!(bob_issues.get(issue_id).unwrap().is_some());

    let mut alice_issues =
        radicle::cob::issue::Cache::no_cache(&alice_repo, &alice.signer).unwrap();
    alice_issues.remove(issue_id).unwrap();

    log::debug!(target: "test", "Removing issue..");

    radicle::assert_matches!(
        bob.handle
            .fetch(rid, alice.id, radicle::node::DEFAULT_TIMEOUT, None)
            .unwrap(),
        radicle::node::FetchResult::Success { .. }
    );
    let bob_repo = radicle::storage::ReadStorage::repository(&bob.storage, rid).unwrap();
    let bob_issues = radicle::cob::issue::Issues::open(&bob_repo, ReadOnly).unwrap();
    assert!(bob_issues.get(issue_id).unwrap().is_none());
}
