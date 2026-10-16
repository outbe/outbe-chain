//! Shared stopped native fixture and process helpers. This file has no registered tests.

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
use outbe_primitives::OutbeHeader;
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
    // This watchdog limits the offline CLI fixture process.
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
    if !output.status.success() {
        eprintln!(
            "offline command failed after {:?}: {command:?}; status: {}",
            started.elapsed(),
            output.status
        );
    }
    assert!(
        !timed_out,
        "offline command timed out: {command:?}\n{}",
        transcript(&output)
    );
    output
}

pub(crate) fn transcript(output: &Output) -> String {
    format!(
        "exit status: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub(crate) struct StoppedFixture {
    pub(crate) donor: PathBuf,
    pub(crate) chain: PathBuf,
    pub(crate) genesis: PathBuf,
    pub(crate) signing_key: PathBuf,
    pub(crate) public_key: [u8; 33],
    pub(crate) finalized_hash: B256,
    pub(crate) execution_hash: B256,
    pub(crate) genesis_hash: B256,
}

/// Native snapshot guards reject symlinked ancestors. macOS's default /var
/// temporary directory aliases /private/var, so fixtures need a physical root.
pub(crate) fn physical_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

pub(crate) fn stopped_fixture(donor: &Path) -> StoppedFixture {
    fs::create_dir_all(donor.join("configuration")).unwrap();
    let genesis_path = donor.join("genesis.json");
    let materialize = run(binary()
        .args(["tee", "genesis", "--input"])
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/native-genesis.json"
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

    fs::create_dir_all(chain.join("keys")).unwrap();
    fs::write(chain.join("keys/private.hex"), b"recipient authority").unwrap();

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
        signing_key,
        public_key,
        finalized_hash: h.hash_slow(),
        execution_hash: e.hash_slow(),
        genesis_hash,
    }
}

// Preserve directory/file membership too. Only existing MDBX reader-slot bytes may change.
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
