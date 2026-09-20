//! Process-level creation tests, not semantic validation or ordinary-node acceptance.

use std::fs;

use outbe_snapshot::{
    archive::read_archive_index,
    manifest::{DomainKind, EntryKind},
};

#[path = "common/snapshot.rs"]
mod snapshot;
use snapshot::{binary, fingerprint, run, stopped_fixture, transcript};

#[test]
fn snapshot_create_help_and_required_signing_key_are_dispatched_offline() {
    let node_help = run(binary().args(["node", "--help"]));
    assert!(node_help.status.success(), "{}", transcript(&node_help));
    assert!(transcript(&node_help).contains("--datadir"));

    let help = run(binary().args(["snapshot", "create", "--help"]));
    assert!(help.status.success(), "{}", transcript(&help));
    for option in ["--output", "--signing-key", "--creator", "--source"] {
        assert!(transcript(&help).contains(option), "{}", transcript(&help));
    }

    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("unsigned.tar");
    let missing_key = run(binary()
        .args(["snapshot", "create", "--output"])
        .arg(&output));
    assert!(!missing_key.status.success());
    assert!(
        transcript(&missing_key).contains("--signing-key"),
        "{}",
        transcript(&missing_key)
    );
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn stopped_native_files_create_a_portable_signed_archive_without_rewriting_progress() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = stopped_fixture(&temp.path().join("donor"));
    let before = fingerprint(&fixture.donor);
    let key_before = fs::read(&fixture.signing_key).unwrap();
    let output = temp.path().join("snapshot.tar");
    let created = run(binary()
        .args(["snapshot", "create", "--output"])
        .arg(&output)
        .arg("--signing-key")
        .arg(&fixture.signing_key)
        .args([
            "--creator",
            "fixture producer",
            "--source",
            "stopped fixture",
            "--",
            "--chain",
        ])
        .arg(&fixture.genesis)
        .arg("--datadir")
        .arg(&fixture.chain)
        .arg("--projection.storage-config")
        .arg(&fixture.projection_config));
    assert!(created.status.success(), "{}", transcript(&created));
    assert_eq!(fs::read(&fixture.signing_key).unwrap(), key_before);
    assert_eq!(fingerprint(&fixture.donor), before);
    assert!(!fixture.donor.join("secondary").exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);

    let recipient = tempfile::tempdir().unwrap();
    let relocated = recipient.path().join("received.tar");
    fs::rename(&output, &relocated).unwrap();
    fs::remove_dir_all(&fixture.donor).unwrap();
    let index = read_archive_index(
        fs::File::open(&relocated).unwrap(),
        Some(&fixture.public_key),
    )
    .unwrap();
    let manifest = &index.manifest;
    assert_eq!(manifest.chain_id, 54322345);
    assert_eq!(manifest.genesis_hash, hex::encode(fixture.genesis_hash));
    assert_eq!(manifest.creator.as_deref(), Some("fixture producer"));
    assert_eq!(manifest.source.as_deref(), Some("stopped fixture"));
    assert_eq!(manifest.progress.finalized.number, 100);
    assert_eq!(
        manifest.progress.finalized.hash,
        hex::encode(fixture.finalized_hash)
    );
    assert_eq!(manifest.progress.execution.number, 101);
    assert_eq!(
        manifest.progress.execution.hash,
        hex::encode(fixture.execution_hash)
    );
    assert_eq!(manifest.progress.execution_stage, Some(101));
    assert_eq!(manifest.progress.finish_stage, Some(100));
    assert_eq!(manifest.progress.storage_version, 2);
    assert_eq!(
        manifest
            .progress
            .unwind
            .as_ref()
            .unwrap()
            .partial_state_trie,
        99
    );
    assert_eq!(manifest.progress.ce.number, 100);
    assert_eq!(manifest.progress.projection.number, 98);
    assert_eq!(manifest.progress.ocomp_current.number, 97);
    assert_eq!(manifest.progress.ocomp_previous.number, 0);
    assert_eq!(
        manifest.progress.ocomp_baseline.hash,
        hex::encode(fixture.genesis_hash)
    );
    assert_eq!(manifest.domains.len(), 23);
    for kind in [
        DomainKind::ExecutionDb,
        DomainKind::Ce,
        DomainKind::OffchainProjection,
        DomainKind::ClosureCheckpoint,
    ] {
        assert!(manifest.domains.iter().any(|domain| domain.kind == kind
            && domain
                .entries
                .iter()
                .any(|entry| entry.kind == EntryKind::File && entry.size > 0)));
    }
    assert!(manifest
        .domains
        .iter()
        .any(|domain| domain.kind == DomainKind::LocalResults
            && domain
                .entries
                .iter()
                .any(|entry| entry.path == fixture.pending_result)));
    assert!(manifest
        .domains
        .iter()
        .flat_map(|domain| &domain.entries)
        .all(|entry| !entry.path.ends_with("private.hex")
            && !entry.path.ends_with("ocomp-evm-key.hex")
            && !entry.path.ends_with("snapshot-signing-key.hex")));
    assert_eq!(
        index
            .signature
            .verify(&index.raw_manifest, Some(&fixture.public_key))
            .unwrap(),
        fixture.public_key
    );
}
