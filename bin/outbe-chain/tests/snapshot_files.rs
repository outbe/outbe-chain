//! Conventional file placement acceptance, without a product restore API or node execution.

use std::{
    collections::BTreeMap,
    fs, io,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use alloy_consensus::{Header, Sealable};
use alloy_primitives::B256;
use outbe_compressed_entities::{CeMdbxReadOnly, EnvironmentIdentity, ACTIVE_COMMITMENT_SCHEME};
use outbe_ocomp::discovery_spool::inspect_closure_checkpoint;
use outbe_offchain_storage::{RocksDbReader, StorageBackend, StorageConfig};
use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader, OutbePrimitives};
use outbe_snapshot::{
    archive::read_archive_index,
    manifest::{DomainKind, EntryKind, NativeRoot, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};
use reth_ethereum::provider::db::{
    database::Database,
    mdbx::DatabaseArguments,
    open_db_read_only,
    tables::{self, ChainStateKey},
    transaction::DbTx,
    ClientVersion,
};
use reth_provider::{
    providers::{RocksDBBuilder, StaticFileProvider},
    BlockHashReader, HeaderProvider, StaticFileSegment, StaticFileWriter,
};

#[path = "common/snapshot.rs"]
mod snapshot;
use snapshot::{binary, fingerprint, run, stopped_fixture, transcript, StoppedFixture};

#[test]
fn conventional_transfer_preserves_native_files_and_opens_without_donor_or_sidecars() {
    let producer = tempfile::tempdir().unwrap();
    let fixture = stopped_fixture(&producer.path().join("donor"));
    let materialization_paths = add_portability_records(&fixture);
    let before = fingerprint(&fixture.donor);
    let archive = producer.path().join("snapshot.tar");
    let created = run(binary()
        .args(["snapshot", "create", "--output"])
        .arg(&archive)
        .arg("--signing-key")
        .arg(&fixture.signing_key)
        .args(["--", "--chain"])
        .arg(&fixture.genesis)
        .arg("--datadir")
        .arg(&fixture.chain)
        .arg("--projection.storage-config")
        .arg(&fixture.projection_config));
    assert!(created.status.success(), "{}", transcript(&created));
    assert_eq!(fingerprint(&fixture.donor), before);

    let receiver = tempfile::tempdir().unwrap();
    let incoming = receiver.path().join("incoming");
    fs::create_dir(&incoming).unwrap();
    let transferred = incoming.join("received.tar");
    successful(Command::new("cp").arg("--").arg(&archive).arg(&transferred));
    let index = read_archive_index(
        fs::File::open(&transferred).unwrap(),
        Some(&fixture.public_key),
    )
    .unwrap();
    let manifest = &index.manifest;
    let extracted = incoming.join("unpacked");
    fs::create_dir(&extracted).unwrap();

    // The recipient no longer has the source tree, signing key, or original archive.
    producer.close().unwrap();
    assert!(!fixture.donor.exists());
    assert!(!archive.exists());
    successful(
        Command::new("tar")
            .args(["--no-same-owner", "-xpf"])
            .arg(&transferred)
            .arg("-C")
            .arg(&extracted),
    );
    assert_eq!(
        fs::read(extracted.join("manifest.json")).unwrap(),
        index.raw_manifest
    );
    check_sidecars(&extracted, &fixture.public_key).unwrap();

    let roots = RecipientRoots::new(receiver.path());
    let protected = provision_recipient(&roots);
    for domain in &manifest.domains {
        if domain.entries.is_empty() {
            continue;
        }
        successful(
            Command::new("cp")
                .args(["-a", "--no-preserve=ownership", "--"])
                .arg(extracted.join("payload").join(&domain.id).join("."))
                .arg(roots.path(domain.native_root)),
        );
    }
    check_files(manifest, &roots).unwrap();
    // Compare actual bytes as well as signed hashes, before any native reader runs.
    for domain in &manifest.domains {
        for entry in &domain.entries {
            if entry.kind == EntryKind::File {
                assert_same_bytes(
                    &roots.path(domain.native_root).join(&entry.path),
                    &extracted.join("payload").join(&domain.id).join(&entry.path),
                );
            }
        }
    }
    for relative in &materialization_paths {
        assert!(roots.ocomp.join(relative).is_file());
    }
    let empty = roots
        .ocomp
        .join(format!("supervisor-v1/jobs/{}/admissions", "33".repeat(32)));
    assert!(empty.is_dir());
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    assert!(roots
        .ocomp
        .join(format!(
            "exporter-v1/input-refs/{}/catalog.lock",
            "11".repeat(32)
        ))
        .is_file());
    assert!(roots
        .ocomp
        .join("exporter-v1/discovery/closure-checkpoint-v1/.lock")
        .is_file());
    let local = roots.ocomp.join("node-v1/local-results");
    assert_eq!(fs::metadata(&local).unwrap().mode() & 0o777, 0o700);
    let result = fs::metadata(roots.ocomp.join(&fixture.pending_result)).unwrap();
    assert_eq!(result.mode() & 0o777, 0o600);
    assert_eq!(result.uid(), fs::metadata(&roots.chain).unwrap().uid());
    assert_protected(&protected);

    // Explicit file-check glue must reject partial placement; this is not a restore protocol.
    let missing = roots.ocomp.join(&materialization_paths[0]);
    fs::remove_file(&missing).unwrap();
    let error = check_files(manifest, &roots).unwrap_err();
    assert!(
        error.to_string().contains(&materialization_paths[0]),
        "{error}"
    );
    let domain = manifest
        .domains
        .iter()
        .find(|domain| domain.kind == DomainKind::MaterializationReferences)
        .unwrap();
    successful(
        Command::new("cp")
            .args(["-p", "--no-preserve=ownership", "--"])
            .arg(
                extracted
                    .join("payload")
                    .join(&domain.id)
                    .join(&materialization_paths[0]),
            )
            .arg(&missing),
    );
    check_files(manifest, &roots).unwrap();

    // Requested provenance checks report absent evidence; native openers do not consume it.
    fs::remove_file(extracted.join("signature.json")).unwrap();
    assert!(check_sidecars(&extracted, &fixture.public_key)
        .unwrap_err()
        .to_string()
        .contains("signature.json"));
    fs::remove_file(extracted.join("manifest.json")).unwrap();
    assert!(check_sidecars(&extracted, &fixture.public_key)
        .unwrap_err()
        .to_string()
        .contains("manifest.json"));
    fs::remove_dir_all(&incoming).unwrap();
    open_native_stores(&roots, &fixture);
    assert_protected(&protected);
}

fn successful(command: &mut Command) {
    let output = run(command);
    assert!(output.status.success(), "{}", transcript(&output));
}

fn assert_same_bytes(left: &Path, right: &Path) {
    let mut left_file = fs::File::open(left).unwrap();
    let mut right_file = fs::File::open(right).unwrap();
    assert_eq!(
        left_file.metadata().unwrap().len(),
        right_file.metadata().unwrap().len()
    );
    let mut left_buffer = [0; 65536];
    let mut right_buffer = [0; 65536];
    loop {
        let count = left_file.read(&mut left_buffer).unwrap();
        if count == 0 {
            assert_eq!(right_file.read(&mut right_buffer[..1]).unwrap(), 0);
            break;
        }
        right_file.read_exact(&mut right_buffer[..count]).unwrap();
        assert_eq!(
            &left_buffer[..count],
            &right_buffer[..count],
            "{}",
            left.display()
        );
    }
}

fn add_portability_records(fixture: &StoppedFixture) -> Vec<String> {
    let db = open_db_read_only(
        fixture.chain.join("db"),
        DatabaseArguments::new(ClientVersion::default()),
    )
    .unwrap();
    let tx = db.tx().unwrap();
    let static_files =
        StaticFileProvider::<OutbePrimitives>::read_write(fixture.chain.join("static_files"))
            .unwrap();
    {
        let mut writer = static_files
            .get_writer(0, StaticFileSegment::Headers)
            .unwrap();
        for number in 0..=101 {
            let header = tx
                .get::<tables::Headers<OutbeHeader>>(number)
                .unwrap()
                .unwrap_or_else(|| {
                    OutbeHeader::new(Header {
                        number,
                        ..Default::default()
                    })
                });
            writer.append_header(&header, &header.hash_slow()).unwrap();
        }
    }
    static_files.commit().unwrap();
    drop(static_files);
    tx.commit().unwrap();
    drop(db);

    let execution = RocksDBBuilder::new(fixture.chain.join("rocksdb"))
        .with_default_tables()
        .with_block_cache_size(1024 * 1024)
        .build()
        .unwrap();
    execution
        .put::<tables::TransactionHashNumbers>(B256::repeat_byte(9), &7)
        .unwrap();
    drop(execution);

    let ocomp = fixture.donor.join("ocomp/domain-v1");
    let mut nested = Vec::new();
    for (job, ordinal) in [
        ("11".repeat(32), 17),
        ("11".repeat(32), 23),
        ("22".repeat(32), 5),
    ] {
        let name = format!("supervisor-v1/materialization-references/{job}/{ordinal}/{job}.materialization-refs-v1.json");
        let path = ocomp.join(&name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("{{\"fixture_job\":\"{job}\",\"fixture_ordinal\":{ordinal}}}\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        nested.push(name);
    }
    // Owner-native temporary filename uses with_extension("tmp"), not .json.tmp.
    let pending = Path::new(&nested[0]).with_extension("tmp");
    fs::write(ocomp.join(&pending), b"unfinished reference fixture").unwrap();
    fs::set_permissions(ocomp.join(&pending), fs::Permissions::from_mode(0o600)).unwrap();
    nested.push(pending.to_str().unwrap().to_owned());
    let admissions = ocomp.join(format!("supervisor-v1/jobs/{}/admissions", "33".repeat(32)));
    fs::create_dir_all(&admissions).unwrap();
    fs::set_permissions(&admissions, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = ocomp.join(format!(
        "exporter-v1/input-refs/{}/catalog.lock",
        "11".repeat(32)
    ));
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, []).unwrap();
    fs::set_permissions(lock, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(
        ocomp.join("node-v1/local-results"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::set_permissions(
        ocomp.join(&fixture.pending_result),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let consensus = fixture.chain.join("consensus");
    fs::create_dir_all(consensus.join("finalized_parent_certs")).unwrap();
    fs::write(
        consensus.join("finalized_parent_certs/100"),
        b"public certificate fixture",
    )
    .unwrap();
    for path in [
        consensus.join("outbe-simplex-3/vote"),
        consensus.join("dkg_share.hex"),
        ocomp.join("supervisor-v1/sign-once/signed"),
        ocomp.join("supervisor-v1/vote-submissions/signed"),
        ocomp.join("supervisor-v1/materialization-submissions/signed"),
        ocomp.join("supervisor-v1/payout-submissions/signed"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"donor authority must not transfer").unwrap();
    }
    nested
}

struct RecipientRoots {
    chain: PathBuf,
    consensus: PathBuf,
    ocomp: PathBuf,
    offchain: PathBuf,
    static_files: PathBuf,
    execution: PathBuf,
    projection_config: PathBuf,
    scratch: PathBuf,
}

impl RecipientRoots {
    fn new(base: &Path) -> Self {
        let roots = Self {
            chain: base.join("network-a/chain"),
            consensus: base.join("consensus-b/data"),
            // Ordinary embedded OCOMP is derived from the chain directory's parent.
            ocomp: base.join("network-a/ocomp/domain-v1"),
            offchain: base.join("projection-c/data"),
            static_files: base.join("headers-d/data"),
            execution: base.join("evm-e/data"),
            projection_config: base.join("operator/offchain.toml"),
            scratch: base.join("inspection-scratch"),
        };
        for kind in [
            NativeRoot::Chain,
            NativeRoot::Consensus,
            NativeRoot::Ocomp,
            NativeRoot::Offchain,
            NativeRoot::StaticFiles,
            NativeRoot::ExecutionRocksDb,
        ] {
            fs::create_dir_all(roots.path(kind)).unwrap();
        }
        roots
    }

    fn path(&self, kind: NativeRoot) -> &Path {
        match kind {
            NativeRoot::Chain => &self.chain,
            NativeRoot::Consensus => &self.consensus,
            NativeRoot::Ocomp => &self.ocomp,
            NativeRoot::Offchain => &self.offchain,
            NativeRoot::StaticFiles => &self.static_files,
            NativeRoot::ExecutionRocksDb => &self.execution,
        }
    }
}

fn provision_recipient(roots: &RecipientRoots) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    let mut protected = BTreeMap::new();
    for path in [
        roots.chain.join("keys/private.hex"),
        roots.chain.join("discovery-secret"),
        roots.chain.join("jwt.hex"),
        roots.chain.join("reth.toml"),
        roots.consensus.join("dkg_share.hex"),
        roots.consensus.join("outbe-simplex-3/vote"),
        roots.ocomp.join("ocomp-evm-key.hex"),
        roots.ocomp.join("ocomp-key-v1.hex"),
        roots.ocomp.join("supervisor-v1/sign-once/signed"),
        roots.ocomp.join("supervisor-v1/vote-submissions/signed"),
        roots
            .ocomp
            .join("supervisor-v1/materialization-submissions/signed"),
        roots.ocomp.join("supervisor-v1/payout-submissions/signed"),
        roots.ocomp.join("node-v1/fatal-evidence/sticky-fatal-v1"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = format!("recipient-owned: {}", path.display()).into_bytes();
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        protected.insert(path, (bytes, 0o600));
    }
    fs::create_dir_all(roots.projection_config.parent().unwrap()).unwrap();
    let config = format!(
        "version = 1\nbackend = 'rocksdb'\nstart_block = 17\n[rocksdb]\npath = {}\nsecondary_path = {}\n",
        serde_json::to_string(&roots.offchain).unwrap(),
        serde_json::to_string(&roots.scratch).unwrap()
    );
    fs::write(&roots.projection_config, config.as_bytes()).unwrap();
    fs::set_permissions(&roots.projection_config, fs::Permissions::from_mode(0o600)).unwrap();
    protected.insert(
        roots.projection_config.clone(),
        (config.into_bytes(), 0o600),
    );
    protected
}

fn assert_protected(protected: &BTreeMap<PathBuf, (Vec<u8>, u32)>) {
    for (path, (bytes, mode)) in protected {
        assert_eq!(&fs::read(path).unwrap(), bytes, "{}", path.display());
        assert_eq!(
            fs::metadata(path).unwrap().mode() & 0o7777,
            *mode,
            "{}",
            path.display()
        );
    }
}

// Deliberately test-local checks: product optional validation belongs to task06.
fn check_files(manifest: &SnapshotManifestV1, roots: &RecipientRoots) -> io::Result<()> {
    for domain in &manifest.domains {
        for entry in &domain.entries {
            let path = roots.path(domain.native_root).join(&entry.path);
            let invalid =
                |reason: String| io::Error::other(format!("{}: {reason}", path.display()));
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| invalid(error.to_string()))?;
            if metadata.mode() & 0o7777 != entry.mode {
                return Err(invalid("native mode differs".into()));
            }
            match entry.kind {
                EntryKind::Directory if metadata.is_dir() => {}
                EntryKind::File if metadata.is_file() => {
                    let output = run(Command::new("sha256sum").arg("--").arg(&path));
                    if !output.status.success() {
                        return Err(invalid(transcript(&output)));
                    }
                    let hash = std::str::from_utf8(&output.stdout)
                        .map_err(|error| invalid(error.to_string()))?
                        .split_whitespace()
                        .next()
                        .ok_or_else(|| invalid("missing sha256sum output".into()))?;
                    if metadata.len() != entry.size || entry.sha256.as_deref() != Some(hash) {
                        return Err(invalid("native file size/checksum differs".into()));
                    }
                }
                _ => return Err(invalid("native file kind differs".into())),
            }
        }
    }
    Ok(())
}

fn check_sidecars(root: &Path, expected: &[u8; 33]) -> io::Result<()> {
    let read = |name: &str| {
        fs::read(root.join(name))
            .map_err(|error| io::Error::new(error.kind(), format!("{name}: {error}")))
    };
    let raw = read("manifest.json")?;
    SignatureEnvelope::from_bytes(&read("signature.json")?)?.verify(&raw, Some(expected))?;
    Ok(())
}

fn open_native_stores(roots: &RecipientRoots, fixture: &StoppedFixture) {
    let db = open_db_read_only(
        roots.chain.join("db"),
        DatabaseArguments::new(ClientVersion::default()),
    )
    .unwrap();
    let tx = db.tx().unwrap();
    assert_eq!(
        tx.get::<tables::ChainState>(ChainStateKey::LastFinalizedBlock)
            .unwrap(),
        Some(100)
    );
    for (number, expected) in [(100, fixture.finalized_hash), (101, fixture.execution_hash)] {
        assert_eq!(
            tx.get::<tables::Headers<OutbeHeader>>(number)
                .unwrap()
                .unwrap()
                .hash_slow(),
            expected
        );
    }
    tx.commit().unwrap();
    drop(db);

    let static_files =
        StaticFileProvider::<OutbePrimitives>::read_only(&roots.static_files).unwrap();
    assert_eq!(
        static_files
            .header_by_number(100)
            .unwrap()
            .unwrap()
            .hash_slow(),
        fixture.finalized_hash
    );
    assert_eq!(
        static_files.block_hash(101).unwrap(),
        Some(fixture.execution_hash)
    );
    drop(static_files);
    let execution = RocksDBBuilder::new(&roots.execution)
        .with_default_tables()
        .with_block_cache_size(1024 * 1024)
        .with_read_only(true)
        .build()
        .unwrap();
    assert_eq!(
        execution
            .get::<tables::TransactionHashNumbers>(B256::repeat_byte(9))
            .unwrap(),
        Some(7)
    );
    drop(execution);

    let ce = CeMdbxReadOnly::open(
        &roots.chain,
        EnvironmentIdentity {
            local_storage_schema_version: outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 54322345,
            genesis_hash: fixture.genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
    )
    .unwrap();
    let marker = ce.marker().unwrap();
    assert_eq!(marker.height, 100);
    assert_eq!(marker.block_hash, fixture.finalized_hash);
    drop(ce);
    let config = StorageConfig::load(&roots.projection_config).unwrap();
    let StorageBackend::RocksDb(rocks) = config.backend else {
        panic!("fixture uses RocksDB")
    };
    assert_eq!(rocks.path, roots.offchain);
    let projection = outbe_offchain_data::read_projection_state(
        outbe_offchain_data::ProjectionConfig {
            chain_id: 54322345,
            genesis_hash: fixture.genesis_hash,
            start_block: config.start_block,
        },
        Arc::new(RocksDbReader::open(&rocks.path, &rocks.secondary_path).unwrap()),
    )
    .unwrap()
    .unwrap()
    .checkpoint
    .unwrap();
    assert_eq!(projection.block_number, 98);
    assert_eq!(projection.block_hash, B256::repeat_byte(98));
    let closure = inspect_closure_checkpoint(
        roots
            .ocomp
            .join("exporter-v1/discovery/closure-checkpoint-v1"),
        ProjectionCheckpoint {
            block_number: 0,
            block_hash: fixture.genesis_hash,
        },
    )
    .unwrap();
    assert_eq!(closure.current.block_number, 97);
    assert_eq!(closure.current.block_hash, B256::repeat_byte(97));
    assert_eq!(closure.previous.block_number, 0);
    assert_eq!(
        fs::read(roots.consensus.join("finalized_parent_certs/100")).unwrap(),
        b"public certificate fixture"
    );
}
