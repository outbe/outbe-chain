//! Shared stopped native fixture and process helpers; no registered tests.

use std::{
    collections::BTreeMap,
    fs,
    hash::Hasher,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use alloy_consensus::{Header, Sealable};
use alloy_primitives::B256;
use outbe_compressed_entities::{
    CeMdbx, EnvironmentIdentity, FinalizedMarker, ACTIVE_COMMITMENT_SCHEME,
};
use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_offchain_storage::{Key, Namespace, RocksDbStorage, StorageWriter, Value};
use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    table::Table,
    tables::{self, ChainStateKey},
    transaction::{DbTx, DbTxMut},
};

type StageCheckpoint = <tables::StageCheckpoints as Table>::Value;

pub(crate) fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_outbe-chain"));
    command.env("RAYON_NUM_THREADS", "2");
    command
}

pub(crate) fn run(command: &mut Command) -> Output {
    // These real MDBX fixtures contain a 4 GiB CE file. Debug hashing may exceed
    // the small CLI fixture timeout; this watchdog is test-only, not a node limit.
    // File-backed output avoids blocking a child while waiting for a large report.
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = command
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(600) {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    stderr.seek(SeekFrom::Start(0)).unwrap();
    let mut output = Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    stdout.read_to_end(&mut output.stdout).unwrap();
    stderr.read_to_end(&mut output.stderr).unwrap();
    assert!(
        !timed_out,
        "offline command timed out: {command:?}\n{}",
        transcript(&output)
    );
    output
}

pub(crate) fn transcript(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub(crate) struct StoppedFixture {
    pub(crate) donor: PathBuf,
    pub(crate) chain: PathBuf,
    pub(crate) genesis: PathBuf,
    pub(crate) projection_config: PathBuf,
    pub(crate) signing_key: PathBuf,
    pub(crate) public_key: [u8; 33],
    pub(crate) finalized_hash: B256,
    pub(crate) execution_hash: B256,
    pub(crate) genesis_hash: B256,
    pub(crate) pending_result: String,
}

pub(crate) fn stopped_fixture(donor: &Path) -> StoppedFixture {
    fs::create_dir_all(donor.join("configuration")).unwrap();
    let genesis_path = donor.join("genesis.json");
    let materialize = run(binary()
        .args(["tee", "genesis", "--input"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../testing/e2e-harness/fixtures/ocomp-final-v1/artifacts/genesis-final.json"
        ))
        .arg("--output")
        .arg(&genesis_path)
        .args(["--mode", "gramine-direct-dev"]));
    assert!(materialize.status.success(), "{}", transcript(&materialize));
    let chain_spec =
        reth_ethereum::cli::chainspec::chain_value_parser(genesis_path.to_str().unwrap())
            .unwrap()
            .as_ref()
            .clone()
            .map_header(OutbeHeader::new);
    let genesis_hash = chain_spec.genesis_hash();
    let chain = donor.join("chain");
    fs::create_dir_all(chain.join("static_files")).unwrap();
    let db = init_db(chain.join("db"), DatabaseArguments::test()).unwrap();
    let h = OutbeHeader::new(Header {
        number: 100,
        ..Default::default()
    });
    let e = OutbeHeader::new(Header {
        number: 101,
        parent_hash: h.hash_slow(),
        ..Default::default()
    });
    let tx = db.tx_mut().unwrap();
    for header in [chain_spec.genesis_header().clone(), h.clone(), e.clone()] {
        tx.put::<tables::CanonicalHeaders>(header.inner.number, header.hash_slow())
            .unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(header.inner.number, header)
            .unwrap();
    }
    tx.put::<tables::ChainState>(ChainStateKey::LastFinalizedBlock, 100)
        .unwrap();
    tx.put::<tables::StageCheckpoints>("Execution".into(), StageCheckpoint::new(101))
        .unwrap();
    tx.put::<tables::StageCheckpoints>("Finish".into(), StageCheckpoint::new(100))
        .unwrap();
    tx.put::<tables::Metadata>(
        "storage_settings".into(),
        br#"{"storage_v2":true}"#.to_vec(),
    )
    .unwrap();
    tx.put::<tables::Metadata>(
        "partial_state_trie_unwind".into(),
        br#"{"finish_block_number":100,"partial_state_trie":99}"#.to_vec(),
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);

    let empty_root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
    let genesis_marker = FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: empty_root,
    };
    // Small test geometry; the normal CE owner still initializes its native schema.
    drop(
        reth_ethereum::provider::db::create_db(
            chain.join("compressed_entities/smt"),
            DatabaseArguments::test(),
        )
        .unwrap(),
    );
    let ce = CeMdbx::open(
        &chain,
        EnvironmentIdentity {
            local_storage_schema_version: outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: chain_spec.chain().id(),
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: outbe_compressed_entities::CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
        },
        genesis_marker,
    )
    .unwrap();
    ce.test_seed_finalized_marker(FinalizedMarker {
        height: 100,
        block_hash: h.hash_slow(),
        parent_root: empty_root,
        ..genesis_marker
    })
    .unwrap();
    drop(ce);

    let projection_config = donor.join("configuration/offchain.toml");
    fs::write(&projection_config, "version = 1\nbackend = 'rocksdb'\nstart_block = 17\n[rocksdb]\npath = '../projection'\nsecondary_path = '../secondary'\n").unwrap();
    let projection = RocksDbStorage::open(donor.join("projection")).unwrap();
    let state = outbe_offchain_data::ProjectionState {
        chain_id: chain_spec.chain().id(),
        genesis_hash,
        storage_schema_version: outbe_offchain_data::STORAGE_SCHEMA_VERSION,
        start_block: 17,
        checkpoint: Some(ProjectionCheckpoint {
            block_number: 98,
            block_hash: B256::repeat_byte(98),
        }),
    };
    projection
        .put(
            Namespace::new("projection_state").unwrap(),
            &Key::new(b"offchain_data".to_vec()).unwrap(),
            &Value::new(postcard::to_stdvec(&state).unwrap()).unwrap(),
        )
        .unwrap();
    drop(projection);

    let ocomp = donor.join("ocomp/domain-v1");
    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: genesis_hash,
    };
    let closure = ContiguousCheckpointStoreV1::open(
        ocomp.join("exporter-v1/discovery/closure-checkpoint-v1"),
        baseline,
    )
    .unwrap();
    closure
        .compare_and_advance_to(
            baseline,
            ProjectionCheckpoint {
                block_number: 97,
                block_hash: B256::repeat_byte(97),
            },
        )
        .unwrap();
    drop(closure);

    // Opaque unfinished public bytes must survive creation without a semantic gate.
    let pending_result = format!("node-v1/local-results/.{}.pending", "11".repeat(32));
    fs::create_dir_all(ocomp.join("node-v1/local-results")).unwrap();
    fs::write(ocomp.join(&pending_result), b"unfinished native fixture").unwrap();
    fs::create_dir_all(chain.join("keys")).unwrap();
    fs::write(chain.join("keys/private.hex"), b"recipient authority").unwrap();
    fs::write(ocomp.join("ocomp-evm-key.hex"), b"private sentinel").unwrap();

    let secret = [1u8; 32];
    let signer = k256::ecdsa::SigningKey::from_bytes((&secret).into()).unwrap();
    let public_key = signer
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let signing_key = donor.join("snapshot-signing-key.hex");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&signing_key)
        .unwrap()
        .write_all(hex::encode(secret).as_bytes())
        .unwrap();
    StoppedFixture {
        donor: donor.to_path_buf(),
        chain,
        genesis: genesis_path,
        projection_config,
        signing_key,
        public_key,
        finalized_hash: h.hash_slow(),
        execution_hash: e.hash_slow(),
        genesis_hash,
        pending_result,
    }
}

// Preserve directory/file membership too; only existing MDBX reader-slot bytes may change.
pub(crate) fn fingerprint(root: &Path) -> BTreeMap<PathBuf, Option<u64>> {
    fn visit(root: &Path, at: &Path, found: &mut BTreeMap<PathBuf, Option<u64>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                found.insert(relative, None);
                visit(root, &path, found);
            } else if path.file_name().unwrap() == "mdbx.lck" {
                found.insert(relative, Some(0));
            } else {
                let mut file = fs::File::open(&path).unwrap();
                let mut digest = std::hash::DefaultHasher::new();
                let mut buffer = [0; 65536];
                loop {
                    let count = file.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    digest.write(&buffer[..count]);
                }
                found.insert(relative, Some(digest.finish()));
            }
        }
    }
    let mut found = BTreeMap::new();
    visit(root, root, &mut found);
    assert!(!found.is_empty());
    found
}
