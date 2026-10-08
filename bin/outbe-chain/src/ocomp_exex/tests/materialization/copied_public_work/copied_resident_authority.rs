use super::*;
use alloy_consensus::{transaction::SignerRecoverable as _, EthereumTxEnvelope, TxEip4844};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::keccak256;
use outbe_ocomp::{
    nod_materialization::MaterializationReferenceStoreV1,
    nod_materialization_submitter::{
        reconcile_finalized_materialization_references, NodMaterializationSubmissionConfigV1,
        NodMaterializationSubmissionOutcomeV1, NodMaterializationSubmitterV1,
    },
    result_signer::OcompSigner,
    sign_once::{SignOnceError, SignOnceStore, SignOnceSubjectV1},
    vote_submitter::{VoteBlockV1, VoteReceiptV1, VoteSubmissionRpcV1},
};
use outbe_ocomp_protocol::committee::verify_low_s_prehash;
use outbe_primitives::{projection::ProjectionCheckpoint, signer::OutbeEvmSigner};
use std::{
    collections::BTreeMap,
    io::Write as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    sync::{Arc, Mutex},
};

struct PublicCopy {
    donor: tempfile::TempDir,
    fixture: Fixture,
    closed: ProjectionCheckpoint,
}

impl PublicCopy {
    fn new() -> Self {
        let donor = tempfile::tempdir().unwrap();
        let chain_root = donor.path().join("chain");
        let points = copied_native::write_frames(&chain_root, 0, 100);
        let public = donor.path().join("ocomp");
        let fixture = fixture(&public, 0x51, WorldwideDay::new(20_260_725), 257);
        write_native_pending_head(&chain_root, &fixture);
        let mut runtime = copied_native::runtime(
            copied_native::provider(&chain_root),
            &public,
            fixture.bundle.clone(),
        );
        copied_native::catch_up(&mut runtime, points[100]);
        assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
        drop(runtime);
        Self {
            donor,
            fixture,
            closed: points[100],
        }
    }

    fn place_public_files(&self, recipient: &Path) {
        copied_native::copy_tree(&self.donor.path().join("chain"), &recipient.join("chain"));
        let source = self.donor.path().join("ocomp");
        let target = recipient.join("ocomp");
        // Public roots only. Resident keys, sign-once and sender journals
        // are deliberately absent from this placement list.
        for relative in [
            "cas-v1",
            "protocol-bundles-v1",
            "exporter-v1/input-refs",
            "exporter-v1/discovery",
            "supervisor-v1/jobs",
            "supervisor-v1/materialization-references",
        ] {
            let from = source.join(relative);
            if from.exists() {
                copied_native::copy_tree(&from, &target.join(relative));
            }
        }
    }
}

fn write_key(path: &Path, byte: u8) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    writeln!(file, "{}", hex::encode([byte; 32])).unwrap();
    file.sync_all().unwrap();
}

fn resident_keys(public: &Path) -> (OutbeEvmSigner, OcompSigner, u32) {
    fs::create_dir_all(public).unwrap();
    let evm = public.join("ocomp-evm-key.hex");
    let result = public.join("ocomp-key-v1.hex");
    write_key(&evm, 0x21);
    write_key(&result, 0x31);
    let uid = fs::metadata(&evm).unwrap().uid();
    (
        outbe_primitives::signer::load::from_strict_file(evm, uid).unwrap(),
        OcompSigner::from_file(result, uid).unwrap(),
        uid,
    )
}

fn submission_root(public: &Path) -> PathBuf {
    public.join("supervisor-v1/materialization-submissions")
}

fn journal_root(public: &Path, f: &Fixture) -> PathBuf {
    submission_root(public)
        .join(hex::encode(f.job_id))
        .join("256")
}

// Scripted RPC completion exercises the real durable submitter. It is
// not evidence of transaction execution/inclusion in the native fixture.
#[derive(Clone)]
struct CompletionRpc {
    enabled: bool,
    sender: Address,
    block: VoteBlockV1,
    sent: Arc<Mutex<Vec<B256>>>,
}

impl CompletionRpc {
    #[track_caller]
    fn require_enabled(&self, reason: Option<&str>) {
        match reason {
            Some(reason) => assert!(self.enabled, "{reason}"),
            None => assert!(self.enabled),
        }
    }
}

impl VoteSubmissionRpcV1 for CompletionRpc {
    type Error = std::io::Error;
    fn chain_id(&self) -> Result<u64, Self::Error> {
        self.require_enabled(Some("resident finalized journal must not call RPC"));
        Ok(copied_native::chain().chain().id())
    }
    fn canonical_nonce(&self, sender: Address) -> Result<u64, Self::Error> {
        self.require_enabled(None);
        assert_eq!(sender, self.sender);
        Ok(7)
    }
    fn gas_price(&self) -> Result<u128, Self::Error> {
        self.require_enabled(None);
        Ok(1)
    }
    fn send_raw_transaction(&self, raw: &[u8], expected: B256) -> Result<B256, Self::Error> {
        self.require_enabled(None);
        assert!(!raw.is_empty());
        assert_eq!(keccak256(raw), expected);
        let mut encoded = raw;
        let transaction = EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut encoded).unwrap();
        assert!(encoded.is_empty());
        assert_eq!(transaction.recover_signer().unwrap(), self.sender);
        self.sent.lock().unwrap().push(expected);
        Ok(expected)
    }
    fn transaction_receipt(&self, tx: B256) -> Result<Option<VoteReceiptV1>, Self::Error> {
        self.require_enabled(None);
        assert_eq!(*self.sent.lock().unwrap().last().unwrap(), tx);
        Ok(Some(VoteReceiptV1 {
            transaction_hash: tx,
            block_number: self.block.number,
            block_hash: self.block.hash,
            success: false,
        }))
    }
    fn canonical_block(&self, number: u64) -> Result<Option<VoteBlockV1>, Self::Error> {
        self.require_enabled(None);
        assert_eq!(number, self.block.number);
        Ok(Some(self.block))
    }
    fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error> {
        self.require_enabled(None);
        Ok(self.block)
    }
}

fn rpc(signer: &OutbeEvmSigner, point: ProjectionCheckpoint, enabled: bool) -> CompletionRpc {
    CompletionRpc {
        enabled,
        sender: signer.address(),
        block: VoteBlockV1 {
            number: point.block_number,
            hash: point.block_hash,
        },
        sent: Arc::new(Mutex::new(Vec::new())),
    }
}

fn submitter(
    public: &Path,
    f: &Fixture,
    signer: OutbeEvmSigner,
    rpc: CompletionRpc,
) -> NodMaterializationSubmitterV1<CompletionRpc> {
    NodMaterializationSubmitterV1::open(
        NodMaterializationSubmissionConfigV1 {
            journal_root: journal_root(public, f),
            expected_chain_id: copied_native::chain().chain().id(),
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        rpc,
        signer,
    )
    .unwrap()
}

enum JournalStage {
    Prepared,
    FinalizedRevert,
}
struct JournalPreparation {
    point: ProjectionCheckpoint,
    stage: JournalStage,
}

fn prepare_resident_journal(
    public: &Path,
    f: &Fixture,
    batch: &outbe_ocomp_protocol::nod_materialization::NodMaterializationBatchV1,
    signer: OutbeEvmSigner,
    preparation: JournalPreparation,
) {
    let rpc = rpc(&signer, preparation.point, true);
    let sent = rpc.sent.clone();
    let mut submitter = submitter(public, f, signer, rpc);
    assert_eq!(
        submitter
            .reconcile(f.job_id, &protected_batch(f, batch))
            .unwrap(),
        NodMaterializationSubmissionOutcomeV1::Pending
    );
    if matches!(preparation.stage, JournalStage::FinalizedRevert) {
        // Prepared -> Submitted -> Included -> Finalized, entirely via
        // the public owner. A failed tx leaves the pending NOD unchanged.
        for _ in 0..2 {
            assert_eq!(
                submitter
                    .reconcile(f.job_id, &protected_batch(f, batch))
                    .unwrap(),
                NodMaterializationSubmissionOutcomeV1::Pending
            );
        }
        assert_eq!(
            submitter
                .reconcile(f.job_id, &protected_batch(f, batch))
                .unwrap(),
            NodMaterializationSubmissionOutcomeV1::Finalized { success: false }
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
    } else {
        assert!(sent.lock().unwrap().is_empty());
    }
}

fn assert_public_reopen(
    recipient: &Path,
    f: &Fixture,
    point: ProjectionCheckpoint,
    expected: &outbe_ocomp::nod_materialization::BuiltNodMaterializationBatchV1,
) {
    let public = recipient.join("ocomp");
    let runtime = copied_native::runtime(
        copied_native::provider(&recipient.join("chain")),
        &public,
        f.bundle.clone(),
    );
    assert_eq!(runtime.closure_checkpoint.current().unwrap(), point);
    assert_eq!(
        build_remaining(&public, f, &read_native_pending_head(&runtime.provider)).unwrap(),
        *expected
    );
}

fn advance_to_k(recipient: &Path, f: &Fixture) -> ProjectionCheckpoint {
    let points = copied_native::write_frames(&recipient.join("chain"), 101, 102);
    let mut runtime = copied_native::runtime(
        copied_native::provider(&recipient.join("chain")),
        &recipient.join("ocomp"),
        f.bundle.clone(),
    );
    copied_native::catch_up(&mut runtime, points[1]);
    assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[1]);
    points[1]
}

#[derive(Clone, Copy, Debug)]
enum ReferencesAtCopy {
    LivePrepared,
    StaleFinalized,
    Released,
}

#[test]
fn copied_nested_nod_refs_release_only_resident_finalized_journals_and_stay_released_at_k() {
    for state in [
        ReferencesAtCopy::LivePrepared,
        ReferencesAtCopy::StaleFinalized,
        ReferencesAtCopy::Released,
    ] {
        let image = PublicCopy::new();
        let recipient = tempfile::tempdir().unwrap();
        let public = recipient.path().join("ocomp");
        let (signer, _, _) = resident_keys(&public);
        let donor_public = image.donor.path().join("ocomp");
        let built =
            build_remaining(&donor_public, &image.fixture, &pending_head(&image.fixture)).unwrap();
        assert!(!built.dependencies.is_empty());
        let refs = MaterializationReferenceStoreV1::open(reference_root(
            &donor_public,
            image.fixture.job_id,
            256,
        ))
        .unwrap();
        refs.pin_exact(image.fixture.job_id, &built.dependencies)
            .unwrap();
        if matches!(state, ReferencesAtCopy::Released) {
            refs.release(image.fixture.job_id).unwrap();
        }
        drop(refs);
        // This journal belongs to the receiver before file placement.
        prepare_resident_journal(
            &public,
            &image.fixture,
            &built.batch,
            signer,
            JournalPreparation {
                point: image.closed,
                stage: if matches!(state, ReferencesAtCopy::LivePrepared) {
                    JournalStage::Prepared
                } else {
                    JournalStage::FinalizedRevert
                },
            },
        );
        let resident_journal = journal_root(&public, &image.fixture).join("submission-v1.json");
        let journal_before = fs::read(&resident_journal).unwrap();
        image.place_public_files(recipient.path());
        let PublicCopy {
            donor,
            fixture,
            closed,
        } = image;
        donor.close().unwrap();
        assert_eq!(fs::read(&resident_journal).unwrap(), journal_before);
        let ref_path = reference_root(&public, fixture.job_id, 256);
        let references = MaterializationReferenceStoreV1::open(&ref_path).unwrap();
        assert_eq!(
            references.load_exact(fixture.job_id).unwrap().is_some(),
            !matches!(state, ReferencesAtCopy::Released)
        );
        let cas = FilesystemCasReader::open(public.join("cas-v1"), CAS_LIMITS).unwrap();
        let before: Vec<_> = built
            .dependencies
            .iter()
            .map(|reference| cas.read_verified(reference).unwrap().bytes().to_vec())
            .collect();
        let expected_releases = usize::from(matches!(state, ReferencesAtCopy::StaleFinalized));
        assert_eq!(
            reconcile_finalized_materialization_references(
                &submission_root(&public),
                &public.join("supervisor-v1/materialization-references")
            )
            .unwrap(),
            expected_releases
        );
        drop(references);
        for restart in 0..2 {
            let point = if restart == 0 {
                closed
            } else {
                advance_to_k(recipient.path(), &fixture)
            };
            assert_public_reopen(recipient.path(), &fixture, point, &built);
            assert_eq!(
                reconcile_finalized_materialization_references(
                    &submission_root(&public),
                    &public.join("supervisor-v1/materialization-references")
                )
                .unwrap(),
                0
            );
            let references = MaterializationReferenceStoreV1::open(&ref_path).unwrap();
            assert_eq!(
                references.load_exact(fixture.job_id).unwrap(),
                matches!(state, ReferencesAtCopy::LivePrepared).then(|| built.dependencies.clone())
            );
            assert_eq!(fs::read(&resident_journal).unwrap(), journal_before);
        }
        let after: Vec<_> = built
            .dependencies
            .iter()
            .map(|reference| cas.read_verified(reference).unwrap().bytes().to_vec())
            .collect();
        assert_eq!(
            after, before,
            "reference retirement must not remove public CAS data"
        );
    }
}

fn protected_files(public: &Path) -> BTreeMap<PathBuf, (Vec<u8>, u32)> {
    fn collect(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (Vec<u8>, u32)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                collect(root, &entry.unwrap().path(), result);
            }
        } else {
            assert!(metadata.is_file());
            result.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (
                    fs::read(path).unwrap(),
                    metadata.permissions().mode() & 0o777,
                ),
            );
        }
    }
    let mut result = BTreeMap::new();
    for relative in [
        "ocomp-evm-key.hex",
        "ocomp-key-v1.hex",
        "supervisor-v1/sign-once",
        "supervisor-v1/materialization-submissions",
    ] {
        collect(public, &public.join(relative), &mut result);
    }
    result
}

#[test]
fn copied_public_files_preserve_own_real_signatures_and_equivocation_guard_through_k() {
    let image = PublicCopy::new();
    let recipient = tempfile::tempdir().unwrap();
    let public = recipient.path().join("ocomp");
    let (evm, signer, uid) = resident_keys(&public);
    let built = build_remaining(
        &image.donor.path().join("ocomp"),
        &image.fixture,
        &pending_head(&image.fixture),
    )
    .unwrap();
    prepare_resident_journal(
        &public,
        &image.fixture,
        &built.batch,
        evm.clone(),
        JournalPreparation {
            point: image.closed,
            stage: JournalStage::FinalizedRevert,
        },
    );
    // A typed resident signing subject, not a claim of an authenticated
    // canonical Completed job or an on-chain result vote.
    let subject = SignOnceSubjectV1 {
        chain_id: copied_native::chain().chain().id(),
        genesis_hash: copied_native::chain().genesis_hash(),
        fork_id: image.fixture.bundle.bundle().fork_id,
        job_id: image.fixture.job_id,
        attempt: 0,
        protocol_bundle_hash: image.fixture.bundle.hash(),
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash(0x81),
        result_ocomp_binding_hash: hash(0x82),
        ocomp_key_hash: keccak256(signer.public_key_sec1()),
        key_epoch: signer.key_epoch(),
        result_digest: keccak256(built.batch.encode_canonical(&poc_schema_limits()).unwrap()),
    };
    let sign_root = public.join("supervisor-v1/sign-once");
    let sign_once = SignOnceStore::open(sign_root.clone(), uid, poc_schema_limits()).unwrap();
    let mut signing_digest = None;
    let signed = sign_once
        .record_or_replay(subject, |digest| {
            signing_digest = Some(digest);
            signer
                .sign_result_digest(digest)
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let digest = signing_digest.expect("the initial record must really sign");
    verify_low_s_prehash(&signer.public_key_sec1(), digest, &signed.signature_rs).unwrap();
    drop(sign_once);
    drop(signer);
    let before = protected_files(&public);
    assert_eq!(
        before.len(),
        4,
        "two keys, one sign-once record and one signed journal"
    );
    image.place_public_files(recipient.path());
    let PublicCopy {
        donor,
        fixture,
        closed,
    } = image;
    donor.close().unwrap();
    assert_eq!(protected_files(&public), before);
    for restart in 0..2 {
        let point = if restart == 0 {
            closed
        } else {
            advance_to_k(recipient.path(), &fixture)
        };
        assert_public_reopen(recipient.path(), &fixture, point, &built);
        let own_evm =
            outbe_primitives::signer::load::from_strict_file(public.join("ocomp-evm-key.hex"), uid)
                .unwrap();
        assert_eq!(own_evm.address(), evm.address());
        let own_signer = OcompSigner::from_file(public.join("ocomp-key-v1.hex"), uid).unwrap();
        let store = SignOnceStore::open(sign_root.clone(), uid, poc_schema_limits()).unwrap();
        let replay = store
            .record_or_replay(subject, |_| {
                panic!("copied public placement must not trigger another signature")
            })
            .unwrap();
        assert_eq!(replay, signed);
        verify_low_s_prehash(&own_signer.public_key_sec1(), digest, &replay.signature_rs).unwrap();
        assert!(matches!(
            store.record_or_replay(
                SignOnceSubjectV1 {
                    result_digest: hash(0xfe),
                    ..subject
                },
                |_| panic!("equivocation must fail before signing")
            ),
            Err(SignOnceError::Equivocation { .. })
        ));
        drop(store);
        let no_rpc = rpc(&own_evm, point, false);
        let mut journal = submitter(&public, &fixture, own_evm, no_rpc);
        assert_eq!(
            journal
                .reconcile(fixture.job_id, &protected_batch(&fixture, &built.batch))
                .unwrap(),
            NodMaterializationSubmissionOutcomeV1::Finalized { success: false }
        );
        drop(journal);
        assert_eq!(protected_files(&public), before);
    }
}

// Paste inside copied_public_work::copied_resident_authority.
// Component evidence only: native MDBX/static frames + NOD/Intex/ValidatorSet
// owner state, actual EmbeddedOcompExExV1 dispatch, real signer/journal/HTTP client.
// ActiveGeneration and receipt/finality RPC replies are scripted. No Metadosis
// quorum, native EVM execution, real Lysis pipeline, or E2E09 claim is made.
mod pending_spawned;
