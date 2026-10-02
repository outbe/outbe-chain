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

fn archive_fixture() -> Vec<u8> {
    use sha2::{Digest, Sha256};

    let mut manifest = manifest_template();
    let domain = &mut manifest.domains[0];
    domain.mode = 0o700;
    domain.entries[0].size = 8;
    domain.entries[0].mode = 0o640;
    domain.entries[0].sha256 = Some(hex::encode(Sha256::digest(b"original")));
    domain.entries[1].mode = 0o750;
    manifest.file_count = 1;
    manifest.total_bytes = 8;
    let raw = serde_json::to_vec(&manifest).unwrap();
    let mut bytes = Vec::new();
    outbe_snapshot::archive::write_archive(
        &mut bytes,
        &raw,
        &sign_manifest(&raw).unwrap(),
        |archive| {
            for (path, mode, directory, data) in [
                ("payload/native", 0o700, true, b"".as_slice()),
                (
                    "payload/native/z.data",
                    0o640,
                    false,
                    b"original".as_slice(),
                ),
                ("payload/native/a.empty", 0o750, true, b"".as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(mode);
                header.set_entry_type(if directory {
                    tar::EntryType::Directory
                } else {
                    tar::EntryType::Regular
                });
                header.set_cksum();
                archive.append_data(&mut header, path, data)?;
            }
            Ok(())
        },
    )
    .unwrap();
    bytes
}

#[test]
fn archive_reader_rejects_metadata_and_member_mismatches_before_contents() {
    let original = archive_fixture();
    let positions: Vec<_> = tar::Archive::new(original.as_slice())
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().raw_header_position() as usize)
        .collect();
    type Mutation = fn(&mut tar::Header);
    let cases: &[(usize, Mutation, &str)] = &[
        (
            0,
            |header| header.set_path("signature.json").unwrap(),
            "invalid manifest.json archive member",
        ),
        (
            0,
            |header| header.set_entry_type(tar::EntryType::Directory),
            "invalid manifest.json archive member",
        ),
        (
            0,
            |header| header.set_size(256 * 1024 * 1024 + 1),
            "invalid manifest.json archive member",
        ),
        (
            1,
            |header| header.set_path("manifest.json").unwrap(),
            "invalid signature.json archive member",
        ),
        (
            1,
            |header| header.set_entry_type(tar::EntryType::Directory),
            "invalid signature.json archive member",
        ),
        (
            1,
            |header| header.set_size(64 * 1024 + 1),
            "invalid signature.json archive member",
        ),
        (
            2,
            |header| header.set_path("payload/other").unwrap(),
            "payload domain differs from manifest",
        ),
        (
            2,
            |header| header.set_entry_type(tar::EntryType::Regular),
            "payload domain differs from manifest",
        ),
        (
            2,
            |header| header.set_size(1),
            "payload domain differs from manifest",
        ),
        (
            2,
            |header| header.set_mode(0o755),
            "payload domain differs from manifest",
        ),
        (
            3,
            |header| header.set_path("payload/native/a.empty").unwrap(),
            "payload member differs from manifest",
        ),
        (
            3,
            |header| header.set_entry_type(tar::EntryType::Directory),
            "payload member differs from manifest",
        ),
        (
            3,
            |header| header.set_size(7),
            "payload member differs from manifest",
        ),
        (
            3,
            |header| header.set_mode(0o644),
            "payload member differs from manifest",
        ),
        (
            4,
            |header| header.set_entry_type(tar::EntryType::Regular),
            "payload member differs from manifest",
        ),
    ];
    for (member, mutate, message) in cases {
        let mut bytes = original.clone();
        let start = positions[*member];
        let mut header = tar::Header::from_byte_slice(&bytes[start..start + 512]).clone();
        mutate(&mut header);
        header.set_cksum();
        bytes[start..start + 512].copy_from_slice(header.as_bytes());
        let error = read_archive_index(bytes.as_slice(), None).err().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), *message, "member {member}");
    }
    for (member, message) in [
        (0, "missing manifest.json"),
        (1, "missing signature.json"),
        (2, "missing payload domain"),
        (3, "missing declared payload"),
    ] {
        let mut bytes = original[..positions[member]].to_vec();
        bytes.extend_from_slice(&[0; 1024]);
        assert_eq!(
            read_archive_index(bytes.as_slice(), None)
                .err()
                .unwrap()
                .to_string(),
            message
        );
    }
    let mut extra = original[..positions[4] + 512].to_vec();
    extra.extend_from_slice(&original[positions[4]..positions[4] + 512]);
    extra.extend_from_slice(&[0; 1024]);
    assert_eq!(
        read_archive_index(extra.as_slice(), None)
            .err()
            .unwrap()
            .to_string(),
        "undeclared archive payload"
    );
    let mut padded = original;
    padded.extend_from_slice(&[0; 4097]);
    read_archive_index(padded.as_slice(), None).unwrap();
    padded.push(1);
    assert_eq!(
        read_archive_index(padded.as_slice(), None)
            .err()
            .unwrap()
            .to_string(),
        "nonzero data after archive terminator"
    );
}

#[test]
fn archive_authentication_precedes_manifest_and_payload_validation() {
    let wrong_key = [0; 33];
    let mut bytes = archive_fixture();
    let payload = tar::Archive::new(bytes.as_slice())
        .entries()
        .unwrap()
        .nth(2)
        .unwrap()
        .unwrap()
        .raw_header_position() as usize;
    let mut header = tar::Header::from_byte_slice(&bytes[payload..payload + 512]).clone();
    header.set_mode(0);
    header.set_cksum();
    bytes[payload..payload + 512].copy_from_slice(header.as_bytes());
    assert_eq!(
        read_archive_index(bytes.as_slice(), Some(&wrong_key))
            .err()
            .unwrap()
            .to_string(),
        "snapshot creator does not match expected public key"
    );
    let raw = b"{}";
    let mut bytes = Vec::new();
    outbe_snapshot::archive::write_archive(&mut bytes, raw, &sign_manifest(raw).unwrap(), |_| {
        Ok(())
    })
    .unwrap();
    assert_eq!(
        read_archive_index(bytes.as_slice(), Some(&wrong_key))
            .err()
            .unwrap()
            .to_string(),
        "snapshot creator does not match expected public key"
    );
    assert!(read_archive_index(bytes.as_slice(), None)
        .err()
        .unwrap()
        .to_string()
        .starts_with("invalid manifest JSON:"));
}

#[test]
fn archive_streaming_accepts_short_reads_and_propagates_payload_and_tail_errors() {
    struct Reader<'a> {
        inner: std::io::Cursor<&'a [u8]>,
        chunk: usize,
        failure: Option<(u64, std::io::ErrorKind)>,
    }
    impl std::io::Read for Reader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let mut size = buffer.len().min(self.chunk);
            if let Some((position, kind)) = self.failure {
                if self.inner.position() == position {
                    self.failure = None;
                    return Err(std::io::Error::new(kind, "injected stream error"));
                }
                size = size.min((position - self.inner.position()) as usize);
            }
            std::io::Read::read(&mut self.inner, &mut buffer[..size])
        }
    }
    let mut bytes = archive_fixture();
    let payload = tar::Archive::new(bytes.as_slice())
        .entries()
        .unwrap()
        .nth(3)
        .unwrap()
        .unwrap()
        .raw_file_position();
    let tail = bytes.len() as u64 + 7;
    bytes.extend_from_slice(&[0; 4096]);
    for chunk in [1, 17, 64 * 1024] {
        read_archive_index(
            Reader {
                inner: std::io::Cursor::new(bytes.as_slice()),
                chunk,
                failure: None,
            },
            None,
        )
        .unwrap();
    }
    for position in [payload + 2, tail] {
        for kind in [std::io::ErrorKind::Interrupted, std::io::ErrorKind::Other] {
            let error = read_archive_index(
                Reader {
                    inner: std::io::Cursor::new(bytes.as_slice()),
                    chunk: 64 * 1024,
                    failure: Some((position, kind)),
                },
                None,
            )
            .err()
            .unwrap();
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "injected stream error");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn creation_preserves_wire_bytes_and_checks_inventory_after_streaming_before_publish() {
    use sha2::{Digest, Sha256};
    use std::{cell::RefCell, os::unix::fs::PermissionsExt};

    let temp = tempfile::tempdir().unwrap();
    let donor = temp.path().join("donor");
    fs::create_dir_all(donor.join("a.empty")).unwrap();
    fs::write(donor.join("z.data"), b"original").unwrap();
    for (path, mode) in [
        (&donor, 0o700),
        (&donor.join("z.data"), 0o640),
        (&donor.join("a.empty"), 0o750),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    let output = temp.path().join("snapshot.tar");
    let events = RefCell::new(Vec::new());
    let manifest = create_snapshot(
        &output,
        manifest_template(),
        std::slice::from_ref(&donor),
        |raw| {
            assert!(!output.exists());
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
            let manifest = SnapshotManifestV1::from_bytes(raw).unwrap();
            assert_eq!((manifest.file_count, manifest.total_bytes), (1, 8));
            events.borrow_mut().push("sign");
            sign_manifest(raw)
        },
        || {
            assert!(!output.exists());
            let pending = fs::read_dir(temp.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_file())
                .unwrap();
            let index = read_archive_index(fs::File::open(pending).unwrap(), None).unwrap();
            assert_eq!(
                (index.manifest.file_count, index.manifest.total_bytes),
                (1, 8)
            );
            events.borrow_mut().push("inventory");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(events.into_inner(), ["sign", "inventory"]);
    assert_eq!(manifest.domains[0].entries[0].path, "z.data");
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 2);
    let bytes = fs::read(output).unwrap();
    assert_eq!(bytes, archive_fixture());
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        "e9163ffb3d29e7bbddc467e423043660d9ffbcd40b5e3df0ec45179b1eeffe8e"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn empty_domains_are_archived_without_opening_their_source_paths() {
    let temp = tempfile::tempdir().unwrap();
    let mut manifest = manifest_template();
    manifest.domains[0].entries.clear();
    let output = temp.path().join("snapshot.tar");
    let result = create_snapshot(
        &output,
        manifest,
        &[temp.path().join("absent")],
        sign_manifest,
        || Ok(()),
    )
    .unwrap();
    assert_eq!((result.file_count, result.total_bytes), (0, 0));
    let index = read_archive_index(fs::File::open(output).unwrap(), None).unwrap();
    assert!(index.manifest.domains[0].entries.is_empty());
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
