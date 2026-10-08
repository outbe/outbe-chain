use super::*;
use alloy_consensus::{BlockHeader as _, Sealable, Transaction as _};
use alloy_primitives::{Bytes, TxKind};
use alloy_sol_types::{sol, SolCall, SolValue};
use outbe_intex::payout::{
    build_contributor_range_proof, contributor_list_root, decode_contributor_leaf,
    CONTRIBUTOR_LEAF_BYTES,
};
use outbe_node::finalized_frame::{read_bounded_finalized_frames, RethFinalizedFrameSource};
use outbe_ocomp::{
    embedded_runtime::{EmbeddedOcompBundleConfigV1, EmbeddedOcompDomainConfigV1},
    payout_artifact::{write_contributor_payout_artifact, CONTRIBUTOR_PAYOUT_ARTIFACT_FILE},
    payout_submitter::PayoutTickOutcomeV1,
};
use outbe_ocomp_protocol::{
    abi::{encode_protected_materialize_certified_nods_calldata, NOD_FACTORY_ADDRESS},
    result::ExactCountsV1,
    state::ActiveGenerationV1,
};
use outbe_primitives::{
    addresses::{INTEX_ADDRESS, INTEX_FACTORY_ADDRESS, METADOSIS_ADDRESS},
    storage::{readonly::ReadOnlyStorageProvider, StorageHandle},
    OutbeHeader, OutbePrimitives, OutbeReceipt, OutbeTxEnvelope,
};
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    models::{ShardedKey, StoredBlockBodyIndices},
    table::Table,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_provider::{
    providers::{RocksDBProvider, StaticFileProviderBuilder},
    StateProviderFactory, StaticFileSegment, StaticFileWriter,
};
use std::{
    io::{BufRead as _, BufReader, Read as _},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const H: u64 = 100;
// Local ABI copies only: avoids adding an outbe-intexfactory dev dependency.
// Signatures match its public Solidity interface exactly.
sol! {
    struct Round { uint256 amount; uint32 contributorCount; uint256 paidSoFar; uint32 paidLeafCount; }
    struct Certified { uint64 seriesVersion; bytes32 contributorRoot; uint32 contributorCount; uint256 eligibleNominalTotal; }
    struct Leaf { address owner; uint256 sourceTributeId; uint256 nominal; }
    function contributorPayoutRound(uint32 worldwideDay) external view returns (Round);
    function certifiedContributorGeneration(uint32 worldwideDay) external view returns (Certified);
    function contributorPaidWord(uint32 worldwideDay, uint32 wordIndex) external view returns (uint256);
    function getActiveLysisGeneration(uint32 wwd) external view returns (bytes);
    function payContributorBatch(uint32 worldwideDay, uint32 startIndex, Leaf[] leaves, bytes32[] proof) external;
}

fn isolated(name: &str) -> bool {
    const CASE: &str = "OUTBE_TEST_COPIED_PENDING_CASE";
    const STARTED: &str = "OUTBE_TEST_COPIED_PENDING_STARTED";
    if std::env::var(CASE).ok().as_deref() == Some(name) {
        outbe_consensus::proof::init_consensus_chain_id(copied_native::chain().chain().id())
            .unwrap();
        outbe_chain_constants::initialize(None).unwrap();
        fs::write(std::env::var_os(STARTED).unwrap(), name).unwrap();
        return true;
    }
    let witness = tempfile::tempdir().unwrap();
    let started = witness.path().join("started");
    let exact = format!(
        "ocomp_exex::tests::materialization::copied_public_work::copied_resident_authority::pending_spawned::{name}"
    );
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &exact, "--nocapture", "--test-threads=1"])
        .env(CASE, name)
        .env(STARTED, &started)
        .env("RAYON_NUM_THREADS", "2")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "pending component child failed: {status}");
                assert_eq!(fs::read_to_string(&started).unwrap(), name);
                return false;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("pending component timed out or wait failed: {result:?}");
            }
        }
    }
}

fn artifact(public: &Path, f: &Fixture) -> Vec<[u8; CONTRIBUTOR_LEAF_BYTES]> {
    let _tribute_enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let limits = poc_schema_limits();
    let cas = FilesystemCasReader::open(public.join("cas-v1"), CAS_LIMITS).unwrap();
    let job = hex::encode(f.job_id);
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        public.join("exporter-v1/input-refs").join(&job),
        &cas,
        limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let job_root = public.join("supervisor-v1/jobs").join(&job);
    let admissions =
        AdmissionCatalogReader::open_existing(job_root.join("admissions"), &cas, limits).unwrap();
    let audit =
        LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)
            .unwrap();
    let count = write_contributor_payout_artifact(&audit, &job_root).unwrap();
    assert_eq!(count, f.nod_count);
    let bytes = fs::read(job_root.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE)).unwrap();
    assert_eq!(bytes.len(), count as usize * CONTRIBUTOR_LEAF_BYTES);
    bytes
        .chunks_exact(CONTRIBUTOR_LEAF_BYTES)
        .map(|chunk| chunk.try_into().unwrap())
        .collect()
}

// Typed ActiveGeneration is a SCRIPTED response, bound to this job and audit.
// It is not written into Metadosis and is not evidence of canonical activation.
fn scripted_active(f: &Fixture, leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]]) -> Vec<u8> {
    ActiveGenerationV1 {
        job_id: f.job_id,
        program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
        nod_root: f.nod_root,
        bucket_root: f.bucket_root,
        contributor_root: contributor_list_root(leaves.len() as u32, leaves.iter()).unwrap(),
        output_manifest_root: f.output_manifest_root,
        exact_counts: ExactCountsV1 {
            tribute_count: f.nod_count,
            nod_count: f.nod_count,
            bucket_count: f.nod_count,
            contributor_count: leaves.len() as u32,
            semantic_event_count: 0,
        },
        result_evidence_hash: f.output_manifest_root,
        availability_certificate_hash: None,
    }
    .encode_canonical(&poc_schema_limits())
    .unwrap()
}

fn expected_payout(f: &Fixture, leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]]) -> Vec<u8> {
    payContributorBatchCall {
        worldwideDay: f.day.into(),
        startIndex: 0,
        leaves: leaves[..256]
            .iter()
            .map(|bytes| {
                let leaf = decode_contributor_leaf(bytes);
                Leaf {
                    owner: leaf.owner,
                    sourceTributeId: leaf.source_tribute_id,
                    nominal: leaf.nominal,
                }
            })
            .collect(),
        proof: build_contributor_range_proof(leaves.len() as u32, 0, leaves.iter()).unwrap(),
    }
    .abi_encode()
}

// Added below: native fixture owners and signed native-frame writer.
fn seed_native(root: &Path, f: &Fixture, sender: Address, leaves: &[[u8; CONTRIBUTOR_LEAF_BYTES]]) {
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    let mut owner = HashMapStorageProvider::new_with_chain_identity(
        copied_native::chain().chain().id(),
        copied_native::chain().genesis_hash(),
    );
    owner.set_block_number(1);
    StorageHandle::enter(&mut owner, |storage| {
        f.seed_pending_head(&storage);
        let intex = outbe_intex::schema::IntexContract::new(storage.clone());
        let count = leaves.len() as u32;
        let root = contributor_list_root(count, leaves.iter()).unwrap();
        let total = leaves.iter().fold(U256::ZERO, |sum, leaf| {
            sum.checked_add(decode_contributor_leaf(leaf).nominal)
                .unwrap()
        });
        intex.ocomp_contributor_root.write(&f.day, root).unwrap();
        intex
            .ocomp_contributor_metadata
            .write(&f.day, U256::ONE | (U256::from(count) << 64))
            .unwrap();
        intex
            .ocomp_eligible_nominal_total
            .write(&f.day, total)
            .unwrap();
        outbe_intex::api::open_certified_payout_round(&storage, f.day.into(), U256::from(10_000))
            .unwrap();
        let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        validators.config_owner.write(sender).unwrap();
        validators.set_config_max_validators(128).unwrap();
        validators.config_epoch_length_blocks.write(10).unwrap();
        // BLS12-381 G1 generator compressed. Only fixture admission uses it.
        // No consensus signer/quorum or SGX identity is constructed here.
        let key: [u8; 48] = hex::decode("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb").unwrap().try_into().unwrap();
        validators.register_validator(sender, sender, &key).unwrap();
        validators
            .activate_validator_via_boundary_for_test(sender)
            .unwrap();
        assert_eq!(
            validators
                .resolve_validator_for_role(
                    sender,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp
                )
                .unwrap(),
            Some(sender)
        );
    });
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    type Word = <tables::PlainStorageState as Table>::Value;
    type HistoryKey = <tables::StoragesHistory as Table>::Key;
    type HistoryBlocks = <tables::StoragesHistory as Table>::Value;
    for ((address, slot), value) in owner.storage {
        if value.is_zero() {
            continue;
        }
        let key = B256::from(slot.to_be_bytes::<32>());
        tx.put::<tables::PlainAccountState>(address, Default::default())
            .unwrap();
        tx.put::<tables::PlainStorageState>(address, Word { key, value })
            .unwrap();
        tx.put::<tables::StorageChangeSets>(
            (0, address).into(),
            Word {
                key,
                value: U256::ZERO,
            },
        )
        .unwrap();
        tx.put::<tables::StoragesHistory>(
            HistoryKey {
                address,
                sharded_key: ShardedKey {
                    key,
                    highest_block_number: u64::MAX,
                },
            },
            HistoryBlocks::new(vec![0]).unwrap(),
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn signed_frames(
    root: &Path,
    first: u64,
    last: u64,
    signer: &OutbeEvmSigner,
) -> Vec<ProjectionCheckpoint> {
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    copied_native::initialize_storage_settings(&tx);
    let mut parent = if first == 0 {
        B256::ZERO
    } else {
        tx.get::<tables::CanonicalHeaders>(first - 1)
            .unwrap()
            .unwrap()
    };
    let files = StaticFileProviderBuilder::read_write(root.join("static_files"))
        .with_blocks_per_file(1_000)
        .build::<OutbePrimitives>()
        .unwrap();
    let mut writer = files
        .get_writer(first, StaticFileSegment::Transactions)
        .unwrap();
    let mut headers = files.get_writer(first, StaticFileSegment::Headers).unwrap();
    let mut points = Vec::new();
    for height in first..=last {
        let input = outbe_primitives::system_tx::SystemTxInputV2::CycleTick;
        let unsigned = outbe_primitives::system_tx::build_unsigned_system_tx(
            input.kind(),
            0,
            height,
            copied_native::chain().chain().id(),
            input.encode().unwrap(),
        )
        .unwrap();
        let transaction: OutbeTxEnvelope = signer.sign_unsigned(unsigned).unwrap();
        let receipt = OutbeReceipt {
            success: true,
            cumulative_gas_used: 21_000,
            ..Default::default()
        };
        let header = if height == 0 {
            copied_native::chain().genesis_header().clone()
        } else {
            copied_native::frame_header(
                copied_native::FrameIdentity {
                    height,
                    parent,
                    timestamp: height,
                },
                &transaction,
                &receipt,
            )
        };
        let hash = header.hash_slow();
        headers.append_header(&header, &hash).unwrap();
        tx.put::<tables::CanonicalHeaders>(height, hash).unwrap();
        tx.put::<tables::HeaderNumbers>(hash, height).unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(height, header)
            .unwrap();
        tx.put::<tables::BlockBodyIndices>(
            height,
            StoredBlockBodyIndices {
                first_tx_num: height.saturating_sub(1),
                tx_count: u64::from(height != 0),
            },
        )
        .unwrap();
        writer.increment_block(height).unwrap();
        if height != 0 {
            writer.append_transaction(height - 1, &transaction).unwrap();
            tx.put::<tables::Receipts<OutbeReceipt>>(height - 1, receipt)
                .unwrap();
        }
        points.push(ProjectionCheckpoint {
            block_number: height,
            block_hash: hash,
        });
        parent = hash;
    }
    drop(writer);
    drop(headers);
    files.commit().unwrap();
    type Stage = <tables::StageCheckpoints as reth_ethereum::provider::db::table::Table>::Value;
    for stage in ["Headers", "Bodies", "Execution", "Finish"] {
        tx.put::<tables::StageCheckpoints>(stage.into(), Stage::new(last))
            .unwrap();
    }
    tx.put::<tables::ChainState>(tables::ChainStateKey::LastFinalizedBlock, last)
        .unwrap();
    tx.commit().unwrap();
    drop(files);
    drop(db);
    drop(
        RocksDBProvider::builder(root.join("rocksdb"))
            .with_default_tables()
            .build()
            .unwrap(),
    );
    points
}

fn native_payout_replies(
    chain_root: &Path,
    f: &Fixture,
    active: Vec<u8>,
) -> BTreeMap<(Address, Vec<u8>), Vec<u8>> {
    let provider = copied_native::provider(chain_root);
    let state = provider.latest().unwrap();
    let reader = OcompExExStateReaderV1 {
        state: state.as_ref(),
    };
    let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
        reader,
        copied_native::chain().chain().id(),
        copied_native::chain().genesis_hash(),
    );
    let storage = StorageHandle::new(&mut readonly);
    let mut replies = BTreeMap::new();
    let mut day: u32 = f.day.into();
    for _ in 0..=PAYOUT_LOOKBACK_DAYS {
        let round = outbe_intex::api::certified_payout_round(&storage, day).unwrap();
        let generation =
            outbe_intex::api::certified_contributor_generation(&storage, WorldwideDay::from(day))
                .unwrap();
        let value = match round {
            Some(round) => Round {
                amount: round.amount,
                contributorCount: generation.as_ref().unwrap().contributor_count,
                paidSoFar: round.paid_so_far,
                paidLeafCount: round.paid_leaf_count,
            },
            None => Round {
                amount: U256::ZERO,
                contributorCount: 0,
                paidSoFar: U256::ZERO,
                paidLeafCount: 0,
            },
        };
        replies.insert(
            (
                INTEX_FACTORY_ADDRESS,
                contributorPayoutRoundCall { worldwideDay: day }.abi_encode(),
            ),
            value.abi_encode(),
        );
        day = outbe_primitives::time::previous_date_key(day);
    }
    let day: u32 = f.day.into();
    let g = outbe_intex::api::certified_contributor_generation(&storage, f.day)
        .unwrap()
        .unwrap();
    assert_eq!(g.contributor_count, 257);
    let round = outbe_intex::api::certified_payout_round(&storage, day)
        .unwrap()
        .unwrap();
    assert_eq!(round.paid_leaf_count, 0);
    assert_eq!(round.paid_so_far, U256::ZERO);
    replies.insert(
        (
            INTEX_ADDRESS,
            certifiedContributorGenerationCall { worldwideDay: day }.abi_encode(),
        ),
        Certified {
            seriesVersion: g.series_version,
            contributorRoot: g.contributor_root,
            contributorCount: g.contributor_count,
            eligibleNominalTotal: g.eligible_nominal_total,
        }
        .abi_encode(),
    );
    let paid = outbe_intex::api::paid_leaves_word(&storage, day, 0).unwrap();
    assert_eq!(paid, U256::ZERO);
    replies.insert(
        (
            INTEX_FACTORY_ADDRESS,
            contributorPaidWordCall {
                worldwideDay: day,
                wordIndex: 0,
            }
            .abi_encode(),
        ),
        paid.abi_encode(),
    );
    // Sole state-view exception: typed fixture response, not a Metadosis read.
    replies.insert(
        (
            METADOSIS_ADDRESS,
            getActiveLysisGenerationCall { wwd: day }.abi_encode(),
        ),
        Bytes::from(active).abi_encode(),
    );
    replies
}

mod rpc;
use rpc::ScriptedRpc;

fn enable_validator<P>(
    runtime: &mut EmbeddedOcompExExV1<P>,
    public: &Path,
    bundle: &PinnedProtocolBundle,
    url: &str,
) {
    // Reuse the existing component fixture. Configure all policy owners
    // consistently. This does not construct another FullNode adapter.
    runtime.domain = EmbeddedOcompDomainV1::open(EmbeddedOcompDomainConfigV1 {
        domain_root: public.to_path_buf(),
        registry_generation: 1,
        bundles: vec![EmbeddedOcompBundleConfigV1 {
            worker_address: "127.0.0.1:0".parse().unwrap(),
            identity: EndpointIdentity {
                chain_id: copied_native::chain().chain().id(),
                genesis_hash: copied_native::chain().genesis_hash(),
                boot_nonce: B256::repeat_byte(0x81),
                protocol_bundle_hash: bundle.hash(),
            },
            protocol_bundle: bundle.clone(),
        }],
        policy: EmbeddedNodePolicyV1::Validator,
        validator_rpc_url: Some(url.to_owned()),
        limits: poc_schema_limits(),
    })
    .unwrap();
    runtime.policy = EmbeddedNodePolicyV1::Validator;
    runtime.state = EmbeddedOcompJobsV1::new(EmbeddedOcompModeV1::Validator);
}

fn assert_no_submission_files(public: &Path) {
    fn walk(path: &Path) {
        if !path.exists() {
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path());
            } else {
                panic!("unexpected submission file {}", entry.path().display());
            }
        }
    }
    for relative in [
        "supervisor-v1/materialization-submissions",
        "supervisor-v1/payout-submissions",
        "supervisor-v1/vote-submissions",
    ] {
        walk(&public.join(relative));
    }
}

mod scenario;
use scenario::{drive_eligible_frames, prepare};

async fn exercise(validator: bool) {
    let _tribute_enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let _enclave = outbe_nodfactory::test_support::enclave_scope();
    let receiver = tempfile::tempdir().unwrap();
    let scenario = prepare(receiver.path());
    let scenario::ScenarioRpc {
        rpc,
        active,
        replies,
    } = scenario.start_rpc();
    scenario.assert_quiet_finality(validator, &rpc).await;
    let k = scenario.advance_retry_frame(&rpc, active, replies);
    let mut resumed = copied_native::runtime(
        copied_native::provider(&scenario.chain_root),
        &scenario.public,
        scenario.fixture.bundle.clone(),
    );
    if validator {
        enable_validator(
            &mut resumed,
            &scenario.public,
            &scenario.fixture.bundle,
            &rpc.url,
        );
    }
    assert_eq!(
        resumed.closure_checkpoint.current().unwrap(),
        scenario.closed
    );
    drive_eligible_frames(&mut resumed, k).await;
    if validator {
        scenario.assert_validator_results(&mut resumed, &rpc);
    } else {
        scenario.assert_fullnode_results(&resumed, &rpc);
    }
    // Reverted receipts must preserve the pending owners.
    assert_eq!(
        read_native_pending_head(&resumed.provider).next_nod_ordinal,
        256
    );
    drop(resumed);
    rpc.finish();
    scenario.assert_reopened(k);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copied_validator_spawns_pending_nod_and_payout_after_eligible_frame() {
    if isolated("copied_validator_spawns_pending_nod_and_payout_after_eligible_frame") {
        exercise(true).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copied_fullnode_with_resident_keys_never_submits_pending_nod_or_payout() {
    if isolated("copied_fullnode_with_resident_keys_never_submits_pending_nod_or_payout") {
        exercise(false).await;
    }
}
