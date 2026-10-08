use super::*;
use alloy_consensus::{BlockHeader as _, Header, Sealable, Transaction as _};
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
    use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
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
        f.protected_sources.seed_source_root(&storage).unwrap();
        let nod = NodContract::new(storage.clone());
        let p = NodCertifiedGenerationProjection {
            worldwide_day: f.day,
            generation: 1,
            job_id: f.job_id,
            program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
            protocol_bundle_hash: f.bundle.hash(),
            nod_root: f.nod_root,
            bucket_root: f.bucket_root,
            output_manifest_root: f.output_manifest_root,
            tribute_count: f.nod_count,
            nod_count: f.nod_count,
            bucket_count: f.nod_count,
            nod_amount_total: U256::from(f.nod_count) * U256::from(2),
            lysis_allocation_minor: U256::from(f.nod_count),
            issued_at: 1_000,
            next_nod_ordinal: 256,
            last_progress_height: 100,
        };
        nod.ocomp_materialization_head_sequence.write(1).unwrap();
        nod.ocomp_materialization_tail_sequence.write(2).unwrap();
        nod.ocomp_materialization_queue_wwd
            .write(&1, f.day)
            .unwrap();
        nod.ocomp_target_generation.write(&f.day, 1).unwrap();
        nod.ocomp_namespace_root.write(&f.day, p.nod_root).unwrap();
        nod.ocomp_bucket_root.write(&f.day, p.bucket_root).unwrap();
        nod.ocomp_output_manifest_root
            .write(&f.day, p.output_manifest_root)
            .unwrap();
        nod.ocomp_generation_metadata
            .write(&f.day, p.metadata_word())
            .unwrap();
        nod.ocomp_nod_amount_total
            .write(&f.day, p.nod_amount_total)
            .unwrap();
        nod.ocomp_lysis_allocation_minor
            .write(&f.day, p.lysis_allocation_minor)
            .unwrap();
        nod.ocomp_materialization_job_id
            .write(&f.day, p.job_id)
            .unwrap();
        nod.ocomp_materialization_protocol_bundle_hash
            .write(&f.day, p.protocol_bundle_hash)
            .unwrap();
        nod.ocomp_materialization_program_semantics_hash
            .write(&f.day, p.program_semantics_hash)
            .unwrap();
        nod.ocomp_materialization_next_nod_ordinal
            .write(&f.day, p.next_nod_ordinal)
            .unwrap();
        nod.ocomp_materialization_last_progress_height
            .write(&f.day, p.last_progress_height)
            .unwrap();
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
    let settings = reth_provider::StorageSettings::v1();
    match tx
        .get::<tables::Metadata>("storage_settings".into())
        .unwrap()
    {
        Some(bytes) => assert!(
            !serde_json::from_slice::<reth_provider::StorageSettings>(&bytes)
                .unwrap()
                .is_v2()
        ),
        None => tx
            .put::<tables::Metadata>(
                "storage_settings".into(),
                serde_json::to_vec(&settings).unwrap(),
            )
            .unwrap(),
    }
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
            OutbeHeader::new(Header {
                number: height,
                parent_hash: parent,
                timestamp: height,
                gas_limit: 30_000_000,
                gas_used: 21_000,
                transactions_root: alloy_consensus::proofs::calculate_transaction_root(
                    std::slice::from_ref(&transaction),
                ),
                receipts_root: alloy_consensus::proofs::calculate_receipt_root(&[
                    alloy_consensus::TxReceipt::with_bloom_ref(&receipt),
                ]),
                ..Default::default()
            })
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

#[derive(Default)]
struct RpcEvidence {
    calls: usize,
    sent: Vec<(Address, Vec<u8>, B256)>,
    round_days: Vec<u32>,
}
struct ScriptedResponses {
    own_sender: Address,
    replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
    expected: BTreeMap<Address, Vec<u8>>,
}
impl ScriptedResponses {
    fn respond(
        &self,
        request: &serde_json::Value,
        state: &mut RpcEvidence,
        point: ProjectionCheckpoint,
    ) -> serde_json::Value {
        let params = &request["params"];
        let method = request["method"].as_str().unwrap();
        match method {
            "eth_chainId" => {
                serde_json::json!(format!("0x{:x}", copied_native::chain().chain().id()))
            }
            "eth_getTransactionCount" => {
                assert_eq!(
                    params[0].as_str().unwrap().parse::<Address>().unwrap(),
                    self.own_sender
                );
                assert_eq!(params[1], "latest");
                // Stable scripted nonce: this component does not model native
                // nonce advancement or order concurrent worker scheduling.
                serde_json::json!("0x7")
            }
            "eth_gasPrice" => serde_json::json!("0x1"),
            "eth_call" => self.read_call(params, state),
            "eth_sendRawTransaction" => self.submit_transaction(params, state),
            "eth_getTransactionReceipt" => {
                let hash = params[0].as_str().unwrap().parse::<B256>().unwrap();
                if state.sent.iter().any(|(_, _, sent)| *sent == hash) {
                    // Deliberately reverted scripted receipts: proves actual delivery
                    // and completion without pretending native state was advanced.
                    serde_json::json!({"transactionHash": format!("{hash:#x}"), "blockNumber": format!("0x{:x}", point.block_number), "blockHash": format!("{:#x}", point.block_hash), "status":"0x0"})
                } else {
                    serde_json::Value::Null
                }
            }
            "eth_getBlockByNumber" => {
                assert!(
                    params[0] == "finalized" || params[0] == format!("0x{:x}", point.block_number)
                );
                serde_json::json!({"number":format!("0x{:x}", point.block_number),"hash":format!("{:#x}",point.block_hash)})
            }
            other => panic!("unexpected RPC method: {other}"),
        }
    }
    fn read_call(&self, params: &serde_json::Value, state: &mut RpcEvidence) -> serde_json::Value {
        assert_eq!(params[1], "finalized");
        let to = params[0]["to"]
            .as_str()
            .unwrap()
            .parse::<Address>()
            .unwrap();
        let data = hex::decode(
            params[0]["data"]
                .as_str()
                .unwrap()
                .strip_prefix("0x")
                .unwrap(),
        )
        .unwrap();
        if data.starts_with(&contributorPayoutRoundCall::SELECTOR) {
            let call = contributorPayoutRoundCall::abi_decode(&data).unwrap();
            state.round_days.push(call.worldwideDay);
        }
        let bytes = self
            .replies
            .get(&(to, data))
            .expect("RPC read outside native fixture/lookback or scripted ActiveGeneration");
        serde_json::json!(format!("0x{}", hex::encode(bytes)))
    }
    fn submit_transaction(
        &self,
        params: &serde_json::Value,
        state: &mut RpcEvidence,
    ) -> serde_json::Value {
        let raw = hex::decode(params[0].as_str().unwrap().strip_prefix("0x").unwrap()).unwrap();
        let mut slice = raw.as_slice();
        let tx = EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut slice).unwrap();
        assert!(slice.is_empty());
        assert!(matches!(&tx, EthereumTxEnvelope::Eip1559(_)));
        assert_eq!(tx.recover_signer().unwrap(), self.own_sender);
        assert_eq!(tx.chain_id(), Some(copied_native::chain().chain().id()));
        assert_eq!(tx.nonce(), 7, "must sign the nonce returned by this RPC");
        assert_eq!(tx.value(), U256::ZERO);
        let TxKind::Call(to) = tx.kind() else {
            panic!("unexpected contract creation")
        };
        assert_eq!(
            tx.input().as_ref(),
            self.expected
                .get(&to)
                .expect("unexpected submission destination")
                .as_slice()
        );
        assert!(
            !state.sent.iter().any(|(previous, _, _)| *previous == to),
            "duplicate submission"
        );
        let hash = keccak256(&raw);
        state.sent.push((to, raw, hash));
        serde_json::json!(format!("{hash:#x}"))
    }
}

struct ScriptedRpc {
    url: String,
    address: std::net::SocketAddr,
    stopped: Arc<AtomicBool>,
    point: Arc<Mutex<ProjectionCheckpoint>>,
    evidence: Arc<Mutex<RpcEvidence>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl ScriptedRpc {
    fn start(
        point: ProjectionCheckpoint,
        own_sender: Address,
        replies: BTreeMap<(Address, Vec<u8>), Vec<u8>>,
        expected: BTreeMap<Address, Vec<u8>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let evidence = Arc::new(Mutex::new(RpcEvidence::default()));
        let point = Arc::new(Mutex::new(point));
        let stop = Arc::clone(&stopped);
        let seen = Arc::clone(&evidence);
        let tip = Arc::clone(&point);
        let responses = ScriptedResponses {
            own_sender,
            replies,
            expected,
        };
        let thread = std::thread::spawn(move || {
            // Joining is unblocked explicitly with a loopback connection.
            // Every accepted request also has a strict byte/time bound.
            while !stop.load(Ordering::Acquire) {
                let (mut stream, _) = listener.accept().unwrap();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_http_json(&mut stream);
                let mut state = seen.lock().unwrap();
                state.calls += 1;
                assert!(state.calls <= 256, "unexpected RPC loop");
                let point = *tip.lock().unwrap();
                let result = responses.respond(&request, &mut state, point);
                let body = serde_json::to_vec(
                    &serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result}),
                )
                .unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            url: format!("http://{address}"),
            address,
            stopped,
            point,
            evidence,
            thread: Some(thread),
        }
    }
    fn assert_quiet(&self) {
        let evidence = self.evidence.lock().unwrap();
        assert_eq!(evidence.calls, 0);
        assert!(evidence.sent.is_empty());
    }
    fn finish(mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread
            .take()
            .unwrap()
            .join()
            .expect("scripted RPC failed");
    }
}
impl Drop for ScriptedRpc {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn read_http_json(stream: &mut TcpStream) -> serde_json::Value {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST "));
    let mut length = None;
    let mut header_bytes = line.len();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        header_bytes += line.len();
        assert!(header_bytes < 16_384);
        if line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = length.expect("content length");
    assert!(length <= 256 * 1024);
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
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

async fn exercise(validator: bool) {
    let _tribute_enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let _enclave = outbe_nodfactory::test_support::enclave_scope();
    let receiver = tempfile::tempdir().unwrap();
    let public = receiver.path().join("ocomp");
    let chain_root = receiver.path().join("chain");
    let (signer, _, _) = resident_keys(&public);
    let own_evm_key = fs::read(public.join("ocomp-evm-key.hex")).unwrap();
    let own_result_key = fs::read(public.join("ocomp-key-v1.hex")).unwrap();
    let donor = tempfile::tempdir().unwrap();
    let donor_chain = donor.path().join("chain");
    let donor_public = donor.path().join("ocomp");
    let h = signed_frames(&donor_chain, 0, H, &signer)[H as usize];
    let day = outbe_primitives::time::worldwide_day_from_timestamp(H);
    let f = fixture(&donor_public, 0x71, WorldwideDay::from(day), 257);
    let leaves = artifact(&donor_public, &f);
    seed_native(&donor_chain, &f, signer.address(), &leaves);
    let mut donor_runtime = copied_native::runtime(
        copied_native::provider(&donor_chain),
        &donor_public,
        f.bundle.clone(),
    );
    copied_native::catch_up(&mut donor_runtime, h);
    assert!(donor_runtime.jobs.is_empty());
    assert_eq!(donor_runtime.closure_checkpoint.current().unwrap(), h);
    drop(donor_runtime);
    // A distinct donor key makes accidental identity copying detectable.
    write_key(&donor_public.join("ocomp-evm-key.hex"), 0x61);
    write_key(&donor_public.join("ocomp-key-v1.hex"), 0x62);
    let copy = PublicCopy {
        donor,
        fixture: f,
        closed: h,
    };
    copy.place_public_files(receiver.path());
    let PublicCopy {
        donor,
        fixture: f,
        closed: _,
    } = copy;
    donor.close().unwrap();
    assert_eq!(
        fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
        own_evm_key
    );
    assert_eq!(
        fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
        own_result_key
    );
    let head = read_native_pending_head(&copied_native::provider(&chain_root));
    assert_eq!(head.next_nod_ordinal, 256);
    assert_eq!(head.nod_count, 257);
    assert_eq!(head.last_progress_height, H);
    let subtree = outbe_chain_constants::get_nod_materialization_batch_subtree_height();
    // Existing build_remaining fixture currently uses subtree height 3.
    // This is the ordinary default, not a production timing override.
    assert_eq!(subtree, 3);
    let expected_nod = encode_protected_materialize_certified_nods_calldata(
        &protected_batch(&f, &build_remaining(&public, &f, &head).unwrap().batch),
        &poc_schema_limits(),
    )
    .unwrap();
    let expected_pay = expected_payout(&f, &leaves);
    let active = scripted_active(&f, &leaves);
    let replies = native_payout_replies(&chain_root, &f, active.clone());
    let rpc = ScriptedRpc::start(
        h,
        signer.address(),
        replies.clone(),
        BTreeMap::from([
            (NOD_FACTORY_ADDRESS, expected_nod),
            (INTEX_FACTORY_ADDRESS, expected_pay),
        ]),
    );
    let mut quiet = copied_native::runtime(
        copied_native::provider(&chain_root),
        &public,
        f.bundle.clone(),
    );
    if validator {
        enable_validator(&mut quiet, &public, &f.bundle, &rpc.url);
    }
    assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
    assert!(quiet.jobs.is_empty() && quiet.requests.is_empty());
    // The quiet C=H branch in run.rs refreshes jobs. It does not call the
    // effect drivers without a new finalized frame. Empty requests model
    // pruned terminal jobs, not successful Completed verification.
    quiet.refresh_jobs(H, h.block_hash, true).await.unwrap();
    quiet.flush_closure_checkpoint().unwrap();
    assert_eq!(quiet.closure_checkpoint.current().unwrap(), h);
    assert!(quiet.materialization_active.is_none() && !quiet.payout_active);
    assert!(matches!(
        quiet.materialization_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        quiet.payout_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    rpc.assert_quiet();
    assert_no_submission_files(&public);
    drop(quiet);

    let retry = outbe_chain_constants::get_nod_materialization_retry_interval_blocks();
    assert!(retry > 0);
    let k_height = H.checked_add(retry).unwrap();
    let k = *signed_frames(&chain_root, H + 1, k_height, &signer)
        .last()
        .unwrap();
    assert_eq!(
        outbe_primitives::time::worldwide_day_from_timestamp(k_height),
        day
    );
    // Native state remains unchanged in these non-executed frame fixtures.
    // Assert the RPC table still matches copied native round/paid authority.
    assert_eq!(native_payout_replies(&chain_root, &f, active), replies);
    *rpc.point.lock().unwrap() = k;
    let mut resumed = copied_native::runtime(
        copied_native::provider(&chain_root),
        &public,
        f.bundle.clone(),
    );
    if validator {
        enable_validator(&mut resumed, &public, &f.bundle, &rpc.url);
    }
    assert_eq!(resumed.closure_checkpoint.current().unwrap(), h);
    let source = RethFinalizedFrameSource::new(resumed.provider.clone());
    let mut visited = Vec::new();
    while let Some(batch) = read_bounded_finalized_frames(
        &source,
        resumed.scanned_height + 1,
        (k.block_number, k.block_hash).into(),
    )
    .unwrap()
    {
        for frame in batch.frames() {
            visited.push(frame.identity().number);
            resumed.record_scanned_frame(frame).unwrap();
            if frame.identity().number == k.block_number {
                resumed
                    .refresh_jobs(k.block_number, k.block_hash, false)
                    .await
                    .unwrap();
                // Actual effect entrypoints, including finalized proposer
                // recovery, native role resolution, and detached workers.
                resumed.reconcile_materialization(frame).unwrap();
                resumed.drive_payout(frame.block().header.timestamp());
            }
        }
        resumed.flush_closure_checkpoint().unwrap();
    }
    // The frame reader owns a provider clone. Close it before the
    // final ordinary reopen of this same native MDBX environment.
    drop(source);
    assert_eq!(visited, (H + 1..=k.block_number).collect::<Vec<_>>());
    assert_eq!(resumed.closure_checkpoint.current().unwrap(), k);
    if validator {
        assert!(resumed.materialization_active.is_some() && resumed.payout_active);
        // Drop the originals: after each result, Disconnected proves the
        // producer released its channel. The isolated process bounds a
        // worker stuck before that point. No worker join API is invented.
        drop(std::mem::replace(
            &mut resumed.materialization_tx,
            std::sync::mpsc::channel().0,
        ));
        drop(std::mem::replace(
            &mut resumed.payout_tx,
            std::sync::mpsc::channel().0,
        ));
        let nod = resumed
            .materialization_rx
            .recv_timeout(Duration::from_secs(45))
            .expect("actual materialization result");
        match &nod {
            EmbeddedMaterializationOutcomeV1::Finalized {
                job_id,
                queue_sequence,
                first_nod_ordinal,
                success,
            } => {
                assert_eq!(*job_id, f.job_id);
                assert_eq!(*queue_sequence, 1);
                assert_eq!(*first_nod_ordinal, 256);
                assert!(!success);
            }
            EmbeddedMaterializationOutcomeV1::Unavailable { detail, .. } => {
                panic!("actual materialization unavailable: {detail}")
            }
        }
        resumed.handle_materialization(nod);
        assert!(matches!(
            resumed
                .materialization_rx
                .recv_timeout(Duration::from_secs(2)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        let payout = resumed
            .payout_rx
            .recv_timeout(Duration::from_secs(45))
            .expect("actual payout result");
        assert!(
            matches!(&payout, EmbeddedPayoutOutcomeV1::Ticked(PayoutTickOutcomeV1::Finalized { worldwide_day, start_index: 0, success: false }) if *worldwide_day == day),
            "actual payout did not finalize: {payout:?}"
        );
        resumed.handle_payout(payout);
        assert!(matches!(
            resumed.payout_rx.recv_timeout(Duration::from_secs(2)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(resumed.materialization_active.is_none() && !resumed.payout_active);
        let evidence = rpc.evidence.lock().unwrap();
        assert_eq!(evidence.sent.len(), 2);
        // These early Unix-time frames map to the first supported
        // day. Earlier candidate dates clamp to that same day, and
        // the submitter stops at its first unpaid round.
        assert_eq!(
            evidence.round_days,
            vec![day],
            "the copied current unpaid round must be selected"
        );
    } else {
        assert!(resumed.materialization_active.is_none() && !resumed.payout_active);
        assert!(matches!(
            resumed.materialization_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            resumed.payout_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        rpc.assert_quiet();
        assert_no_submission_files(&public);
        assert_eq!(resumed.domain.validator_sender_address(), None);
    }
    // Reverted scripted receipts leave both real pending owners untouched.
    assert_eq!(
        read_native_pending_head(&resumed.provider).next_nod_ordinal,
        256
    );
    drop(resumed);
    rpc.finish();
    let reopened = copied_native::runtime(
        copied_native::provider(&chain_root),
        &public,
        f.bundle.clone(),
    );
    assert_eq!(reopened.closure_checkpoint.current().unwrap(), k);
    assert_eq!(
        read_native_pending_head(&reopened.provider).next_nod_ordinal,
        256
    );
    assert_eq!(
        fs::read(public.join("ocomp-evm-key.hex")).unwrap(),
        own_evm_key
    );
    assert_eq!(
        fs::read(public.join("ocomp-key-v1.hex")).unwrap(),
        own_result_key
    );
    drop(reopened);
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
