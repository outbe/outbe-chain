#[cfg(target_os = "linux")]
use outbe_snapshot::create::create_snapshot;
#[cfg(target_os = "linux")]
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::symlink,
    path::Path,
};

#[cfg(target_os = "linux")]
use outbe_snapshot::fs::{PendingArchive, SourceRoot};
use outbe_snapshot::{
    archive::read_archive_index,
    manifest::SnapshotManifestV1,
    provenance::{signing_digest, SignatureEnvelope},
};

fn manifest_template() -> SnapshotManifestV1 {
    let block = |n| serde_json::json!({"number":n,"hash":"11".repeat(32)});
    serde_json::from_value(serde_json::json!({
        "version":1,"chain_id":54322345,"genesis_hash":"22".repeat(32),
        "created_at_unix":1789940000_u64,"creator":"test creator","source":null,
        "progress":{"finalized":block(100),"execution":block(101),"execution_stage":101,
            "finish_stage":100,"partial_state_trie":null,"unwind":null,"storage_version":2,
            "ce":block(100),"projection":block(98),"ocomp_baseline":block(0),
            "ocomp_previous":block(90),"ocomp_current":block(97)},
        "domains":[{"id":"native","kind":"execution-db","native_root":"chain","native_path":"db","mode":448,
            "entries":[{"path":"z.data","kind":"file","size":0,"sha256":null,"mode":0},
                       {"path":"a.empty","kind":"directory","size":0,"sha256":null,"mode":0}]}],
        "file_count":0,"total_bytes":0
    })).unwrap()
}

fn sign_manifest(raw: &[u8]) -> std::io::Result<SignatureEnvelope> {
    let key = k256::ecdsa::SigningKey::from_bytes((&[7; 32]).into()).unwrap();
    let (signature, recovery) = key.sign_prehash_recoverable(&signing_digest(raw)).unwrap();
    let mut bytes = [0; 65];
    bytes[..64].copy_from_slice(&signature.to_bytes());
    bytes[64] = recovery.to_byte();
    SignatureEnvelope::from_signature(raw, bytes)
}

#[cfg(target_os = "linux")]
#[test]
fn signed_archive_is_portable_and_contains_unchanged_native_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("donor");
    fs::create_dir_all(donor.join("a.empty")).unwrap();
    let data = vec![42; 1024 * 1024 + 17];
    fs::write(donor.join("z.data"), &data).unwrap();
    let output = temp.path().join("snapshot.tar");
    create_snapshot(
        &output,
        manifest_template(),
        std::slice::from_ref(&donor),
        sign_manifest,
        || Ok(()),
    )
    .unwrap();
    fs::remove_dir_all(&donor).unwrap();
    let moved = temp.path().join("received.tar");
    fs::rename(output, &moved).unwrap();
    let index = read_archive_index(fs::File::open(&moved).unwrap(), None).unwrap();
    assert_eq!(index.manifest.progress.ocomp_current.number, 97);
    assert_eq!(index.manifest.file_count, 1);
    assert_eq!(index.manifest.total_bytes, data.len() as u64);
    let mut damaged = fs::read(&moved).unwrap();
    let at = damaged
        .windows(256)
        .position(|bytes| bytes.iter().all(|byte| *byte == 42))
        .unwrap();
    damaged[at] ^= 1;
    assert!(read_archive_index(damaged.as_slice(), None).is_err());
    let mut appended = fs::read(&moved).unwrap();
    appended.extend_from_slice(b"undeclared trailing data");
    assert!(read_archive_index(appended.as_slice(), None).is_err());
    let destination = temp.path().join("placed");
    tar::Archive::new(fs::File::open(&moved).unwrap())
        .unpack(&destination)
        .unwrap();
    assert_eq!(
        fs::read(destination.join("payload/native/z.data")).unwrap(),
        data
    );
    assert!(destination.join("payload/native/a.empty").is_dir());
}

#[test]
fn archive_root_permissions_must_match_the_signed_entry() {
    let mut manifest = manifest_template();
    manifest.domains[0].mode = 0o755;
    manifest.domains[0].entries.truncate(1);
    let entry = &mut manifest.domains[0].entries[0];
    entry.path.clear();
    entry.kind = outbe_snapshot::manifest::EntryKind::Directory;
    entry.mode = 0o700;
    let raw = serde_json::to_vec(&manifest).unwrap();
    let signature = sign_manifest(&raw).unwrap();
    let mut bytes = Vec::new();
    outbe_snapshot::archive::write_archive(&mut bytes, &raw, &signature, |archive| {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        archive.append_data(&mut header, "payload/native", std::io::empty())
    })
    .unwrap();
    assert!(read_archive_index(bytes.as_slice(), None).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn signing_failure_or_changed_source_never_publishes_an_archive() {
    for change_source in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let donor = temp.path().join("donor");
        fs::create_dir_all(donor.join("a.empty")).unwrap();
        fs::write(donor.join("z.data"), b"original").unwrap();
        let output = temp.path().join("snapshot.tar");
        let result = create_snapshot(
            &output,
            manifest_template(),
            std::slice::from_ref(&donor),
            |raw| {
                if change_source {
                    fs::write(donor.join("z.data"), b"modified")?;
                    sign_manifest(raw)
                } else {
                    Err(std::io::Error::other("signing failed"))
                }
            },
            || Ok(()),
        );
        assert!(result.is_err());
        assert!(!output.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn replacing_the_source_root_does_not_publish_from_an_unlinked_old_directory() {
    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("donor");
    fs::create_dir_all(donor.join("a.empty")).unwrap();
    fs::write(donor.join("z.data"), b"original").unwrap();
    let output = temp.path().join("snapshot.tar");
    let result = create_snapshot(
        &output,
        manifest_template(),
        std::slice::from_ref(&donor),
        |raw| {
            fs::rename(&donor, temp.path().join("old-donor"))?;
            fs::create_dir_all(donor.join("a.empty"))?;
            fs::write(donor.join("z.data"), b"replaced")?;
            sign_manifest(raw)
        },
        || Ok(()),
    );
    assert!(result.is_err());
    assert!(!output.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn changed_inventory_report_prevents_final_publication() {
    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("donor");
    fs::create_dir_all(donor.join("a.empty")).unwrap();
    fs::write(donor.join("z.data"), b"original").unwrap();
    let output = temp.path().join("snapshot.tar");
    let result = create_snapshot(
        &output,
        manifest_template(),
        std::slice::from_ref(&donor),
        sign_manifest,
        || Err(std::io::Error::other("new native job appeared during copy")),
    );
    assert!(result.unwrap_err().to_string().contains("new native job"));
    assert!(!output.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn native_files_are_read_in_place_and_replacement_is_detected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("native.db");
    fs::write(&path, b"stored state").unwrap();
    let root = SourceRoot::open(temp.path()).unwrap();
    let mut entry = root.open_entry(Path::new("native.db")).unwrap();
    let identity = entry.identity.clone();
    let mut bytes = Vec::new();
    entry.file.read_to_end(&mut bytes).unwrap();
    entry.verify_unchanged().unwrap();
    assert_eq!(bytes, b"stored state");
    root.reopen(Path::new("native.db"), &identity).unwrap();
    fs::rename(&path, temp.path().join("old.db")).unwrap();
    fs::write(&path, b"stored state").unwrap();
    assert!(root.reopen(Path::new("native.db"), &identity).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn copying_does_not_follow_source_links_or_accept_hardlinks() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(temp.path().join("secret"), b"private").unwrap();
    symlink(temp.path(), source.join("alias")).unwrap();
    fs::hard_link(temp.path().join("secret"), source.join("hardlink")).unwrap();
    let root = SourceRoot::open(&source).unwrap();
    assert!(root.open_entry(Path::new("alias/secret")).is_err());
    assert!(root.open_entry(Path::new("hardlink")).is_err());
    assert_eq!(fs::read(temp.path().join("secret")).unwrap(), b"private");
}

#[cfg(target_os = "linux")]
#[test]
fn pending_archive_publishes_once_and_cleans_only_its_unpublished_file() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("snapshot.tar");
    let mut pending = PendingArchive::new(&output).unwrap();
    pending.file.write_all(b"complete archive").unwrap();
    assert!(!output.exists());
    pending.publish().unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"complete archive");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);

    let mut conflicting = PendingArchive::new(&temp.path().join("other.tar")).unwrap();
    conflicting.file.write_all(b"unfinished").unwrap();
    fs::write(temp.path().join("other.tar"), b"other owner").unwrap();
    assert!(conflicting.publish().is_err());
    assert_eq!(
        fs::read(temp.path().join("other.tar")).unwrap(),
        b"other owner"
    );
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);

    let abandoned = PendingArchive::new(&temp.path().join("abandoned.tar")).unwrap();
    drop(abandoned);
    assert!(!temp.path().join("abandoned.tar").exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
}

#[cfg(target_os = "linux")]
#[test]
fn replaced_pending_file_is_neither_published_nor_removed() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("snapshot.tar");
    let pending = PendingArchive::new(&output).unwrap();
    let name = fs::read_dir(temp.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(&name).unwrap();
    fs::write(&name, b"another operation").unwrap();
    assert!(pending.publish().is_err());
    assert!(!output.exists());
    assert_eq!(fs::read(&name).unwrap(), b"another operation");
}
