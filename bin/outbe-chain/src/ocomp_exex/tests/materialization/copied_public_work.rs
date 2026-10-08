use super::super::recovery::copied_native;
use super::*;
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::TributeBodyV1;
use outbe_lysis::program_v1::{
    planner::{LysisPlanTopologyV1, PlannedUnitPositionV1},
    result::{encode_root_reduce_output, RootReduceOutputV1},
};
use outbe_ocomp::nod_materialization::build_nod_materialization_batch_with_references;
use outbe_ocomp::{
    admission_catalog::{AdmissionCatalogReader, AdmissionPositionV1, VerifiedAdmissionCatalog},
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    lysis_plan_audit::LocalLysisPlanAuditV1,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    input::{CheckpointIdentityV1, InputChunkKind, InputManifestV1},
    registry::{
        ObjectKind, FIDELITY_OPENING_CODEC_ID, ORACLE_OPENING_CODEC_ID, TRIBUTE_BODY_CODEC_ID,
    },
    result::{ContributorActionV1, OutputManifestEntryV1, ResultChunkV1},
    unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
    ListKind,
};
use outbe_ocomp_protocol::{
    nod_materialization::NodMaterializationHeadV1, profile::ProtocolBundleV1, CasObjectRefV1,
    StreamingOrderedListRoot,
};
use outbe_primitives::time::WorldwideDay;
use std::{
    fs,
    path::{Path, PathBuf},
};
const CAS_LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1_048_576,
    max_total_bytes: 64 * 1_048_576,
};
fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}
struct Fixture {
    job_id: B256,
    day: WorldwideDay,
    bundle: PinnedProtocolBundle,
    nod_root: B256,
    bucket_root: B256,
    output_manifest_root: B256,
    nod_count: u32,
    result_chunk_refs: Vec<CasObjectRefV1>,
    protected_sources: outbe_nodfactory::test_support::MaterializationFixture,
}
// Protocol-shaped native CAS/planner fixture. Minimal non-root phase payloads
// support structural/proof tests. This is not real worker-pipeline E2E evidence.
fn protocol_bundle() -> ProtocolBundleV1 {
    ProtocolBundleV1 {
        protocol_version: 1,
        fork_id: B256::repeat_byte(1),
        intent_codec_id: B256::repeat_byte(2),
        finalized_intent_proof_codec_id: B256::repeat_byte(3),
        tribute_body_codec_id: TRIBUTE_BODY_CODEC_ID,
        fidelity_opening_codec_id: FIDELITY_OPENING_CODEC_ID,
        oracle_opening_codec_id: ORACLE_OPENING_CODEC_ID,
        result_codec_id: B256::repeat_byte(4),
        action_codec_id: B256::repeat_byte(5),
        activation_codec_id: B256::repeat_byte(6),
        evidence_codec_id: B256::repeat_byte(7),
        request_semantics_version: 1,
        lysis_program_semantics_hash: B256::repeat_byte(8),
        planner_spec_version: 1,
        reducer_spec_version: 1,
        activation_apply_semantics_hash: B256::repeat_byte(9),
        effect_contract_registry_hash: B256::repeat_byte(10),
        object_codec_registry_hash: B256::repeat_byte(11),
        correctness_profile_id: B256::repeat_byte(12),
        capacity_profile_id: B256::repeat_byte(13),
        result_signature_profile_id: B256::repeat_byte(14),
        finality_verifier_and_vote_domain_id: B256::repeat_byte(15),
        consensus_committee_history_schema_version: 1,
        ocomp_committee_schema_version: 1,
        proof_system_and_verifier_key_id: None,
        da_codec_and_binding_verifier_id: None,
        anti_equivocation_journal_schema_hash: B256::repeat_byte(16),
        mode_pause_revocation_semantics_hash: B256::repeat_byte(17),
        upgrade_fsm_semantics_hash: B256::repeat_byte(18),
        release_requirement_catalog_sequence: 1,
        release_requirement_catalog_hash: B256::repeat_byte(19),
        release_requirement_catalog_parent_hash: B256::repeat_byte(20),
        release_gate_authority_envelope_hash: B256::repeat_byte(21),
        release_approval_policy_hash: B256::repeat_byte(22),
        release_validator_command_artifact_hash: B256::repeat_byte(23),
        consensus_state_schema_version: 1,
        migration_manifest_hash: B256::repeat_byte(24),
        required_upgrade_handler_set_hash: B256::repeat_byte(25),
    }
}
mod fixture;
use fixture::fixture;

fn pending_head(f: &Fixture) -> NodMaterializationHeadV1 {
    NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: f.job_id,
        program_semantics_hash: f.bundle.bundle().lysis_program_semantics_hash,
        worldwide_day: f.day.value(),
        generation: 1,
        nod_root: f.nod_root,
        nod_count: f.nod_count,
        next_nod_ordinal: 256,
        last_progress_height: 100,
    }
}

fn write_native_pending_head(root: &Path, f: &Fixture) {
    use outbe_nod::schema::{NodCertifiedGenerationProjection, NodContract};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use reth_ethereum::provider::db::{
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        tables,
        transaction::{DbTx, DbTxMut},
    };
    use reth_primitives_traits::StorageEntry;
    let mut owner = HashMapStorageProvider::new_with_chain_identity(
        copied_native::chain().chain().id(),
        copied_native::chain().genesis_hash(),
    );
    StorageHandle::enter(&mut owner, |storage| {
        f.protected_sources.seed_source_root(&storage).unwrap();
        let nod = NodContract::new(storage);
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
    });
    let db = init_db(root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    for ((address, slot), value) in owner.storage {
        if !value.is_zero() {
            tx.put::<tables::PlainStorageState>(
                address,
                StorageEntry {
                    key: alloy_primitives::B256::from(slot.to_be_bytes::<32>()),
                    value,
                },
            )
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn read_native_pending_head<P: reth_provider::StateProviderFactory>(
    provider: &P,
) -> NodMaterializationHeadV1 {
    use outbe_primitives::storage::{readonly::ReadOnlyStorageProvider, StorageHandle};
    let state = provider.latest().unwrap();
    let reader = OcompExExStateReaderV1 {
        state: state.as_ref(),
    };
    let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
        reader,
        copied_native::chain().chain().id(),
        copied_native::chain().genesis_hash(),
    );
    outbe_nod::NodContract::new(StorageHandle::new(&mut readonly))
        .ocomp_materialization_head()
        .unwrap()
        .unwrap()
}

fn build_remaining(
    root: &Path,
    f: &Fixture,
    head: &NodMaterializationHeadV1,
) -> eyre::Result<outbe_ocomp::nod_materialization::BuiltNodMaterializationBatchV1> {
    let _tribute_enclave = outbe_tribute::enclave_client::test_enclave::scope();
    let limits = poc_schema_limits();
    let cas = FilesystemCasReader::open(root.join("cas-v1"), CAS_LIMITS)?;
    let job = hex::encode(f.job_id);
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        root.join("exporter-v1/input-refs").join(&job),
        &cas,
        limits,
        poc_input_list_limits(),
    )?;
    let admissions = AdmissionCatalogReader::open_existing(
        root.join("supervisor-v1/jobs")
            .join(&job)
            .join("admissions"),
        &cas,
        limits,
    )?;
    let audit =
        LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)?;
    Ok(build_nod_materialization_batch_with_references(
        &audit, head, 3,
    )?)
}

fn protected_batch(
    f: &Fixture,
    batch: &outbe_ocomp_protocol::nod_materialization::NodMaterializationBatchV1,
) -> outbe_ocomp_protocol::nod_materialization::ProtectedNodMaterializationV2 {
    let mut head = pending_head(f);
    head.next_nod_ordinal = batch.first_nod_ordinal;
    f.protected_sources
        .protect(
            &outbe_tee::nod_materialization::NodMaterializationAuthorityV2 {
                chain_id: copied_native::chain().chain().id(),
                head: head.encode_canonical(&poc_schema_limits()).unwrap(),
                subtree_height: 3,
                sealed_tribute_root: f.protected_sources.source_root(),
            },
            batch,
        )
        .unwrap()
}

fn reference_root(root: &Path, job: B256, first_nod_ordinal: u32) -> PathBuf {
    root.join("supervisor-v1/materialization-references")
        .join(hex::encode(job))
        .join(first_nod_ordinal.to_string())
}

fn chunk_path(root: &Path, f: &Fixture, ordinal: usize) -> PathBuf {
    let digest = hex::encode(f.result_chunk_refs[ordinal].transport_digest);
    root.join("cas-v1/objects")
        .join(&digest[..2])
        .join(&digest[2..])
}

#[test]
fn copied_pending_nod_remains_buildable_after_terminal_pruning_and_consumed_chunk_removal() {
    use outbe_ocomp::embedded::EmbeddedJobEventV1;
    use outbe_ocomp::nod_materialization::MaterializationReferenceStoreV1;
    for remove_required in [false, true] {
        let donor = tempfile::tempdir().unwrap();
        let receiver = tempfile::tempdir().unwrap();
        let points = copied_native::write_frames(&donor.path().join("chain"), 0, 100);
        let public = donor.path().join("ocomp");
        let f = fixture(&public, 0x41, WorldwideDay::new(20_260_725), 257);
        write_native_pending_head(&donor.path().join("chain"), &f);
        let mut runtime = copied_native::runtime(
            copied_native::provider(&donor.path().join("chain")),
            &public,
            f.bundle.clone(),
        );
        let generation = runtime.state.observe_job(f.job_id, 90).unwrap();
        let digest = B256::repeat_byte(0x91);
        runtime
            .state
            .reduce(
                f.job_id,
                EmbeddedJobEventV1::LocalCompleted {
                    generation,
                    result_digest: digest,
                },
            )
            .unwrap();
        runtime
            .state
            .reduce(
                f.job_id,
                EmbeddedJobEventV1::CanonicalCompleted {
                    result_digest: digest,
                },
            )
            .unwrap();
        copied_native::catch_up(&mut runtime, points[100]);
        assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
        runtime.state.prune_terminal_job(f.job_id).unwrap();
        assert!(runtime.state.state(f.job_id).is_none());
        assert!(runtime.jobs.is_empty());
        let head = read_native_pending_head(&runtime.provider);
        assert_eq!(head, pending_head(&f));
        let built = build_remaining(&public, &f, &head).unwrap();
        assert_eq!(built.batch.actions.len(), 1);
        assert_eq!(built.batch.actions[0].raw_ordinal, 256);
        let references = MaterializationReferenceStoreV1::open(reference_root(
            &public,
            f.job_id,
            head.next_nod_ordinal,
        ))
        .unwrap();
        references.pin_exact(f.job_id, &built.dependencies).unwrap();
        let reference_member = reference_root(Path::new(""), f.job_id, head.next_nod_ordinal).join(
            format!("{}.materialization-refs-v1.json", hex::encode(f.job_id)),
        );
        let reference_bytes = fs::read(public.join(&reference_member)).unwrap();
        assert!(!reference_bytes.is_empty());
        drop(references);
        drop(runtime);
        copied_native::copy_tree(donor.path(), receiver.path());
        donor.close().unwrap();
        let public = receiver.path().join("ocomp");
        assert_eq!(
            fs::read(public.join(&reference_member)).unwrap(),
            reference_bytes
        );
        // This is a dependency-absence control, not an invocation of the GC scheduler.
        fs::remove_file(chunk_path(&public, &f, 0)).unwrap();
        if remove_required {
            fs::remove_file(chunk_path(&public, &f, 1)).unwrap();
        }
        let runtime = copied_native::runtime(
            copied_native::provider(&receiver.path().join("chain")),
            &public,
            f.bundle.clone(),
        );
        assert!(runtime.jobs.is_empty());
        let head = read_native_pending_head(&runtime.provider);
        let actual = build_remaining(&public, &f, &head);
        if remove_required {
            assert!(
                actual.is_err(),
                "pending NOD must not silently discard a missing required chunk"
            );
            assert_eq!(runtime.closure_checkpoint.current().unwrap(), points[100]);
            continue;
        }
        assert_eq!(actual.unwrap(), built);
        drop(runtime);
        let later = copied_native::write_frames(&receiver.path().join("chain"), 101, 102);
        let mut runtime = copied_native::runtime(
            copied_native::provider(&receiver.path().join("chain")),
            &public,
            f.bundle.clone(),
        );
        copied_native::catch_up(&mut runtime, later[1]);
        drop(runtime);
        let restarted = copied_native::runtime(
            copied_native::provider(&receiver.path().join("chain")),
            &public,
            f.bundle.clone(),
        );
        assert_eq!(restarted.closure_checkpoint.current().unwrap(), later[1]);
        assert_eq!(
            fs::read(public.join(&reference_member)).unwrap(),
            reference_bytes
        );
        assert_eq!(
            build_remaining(&public, &f, &read_native_pending_head(&restarted.provider)).unwrap(),
            built
        );
        assert_eq!(
            MaterializationReferenceStoreV1::open(reference_root(
                &public,
                f.job_id,
                head.next_nod_ordinal
            ))
            .unwrap()
            .load_exact(f.job_id)
            .unwrap(),
            Some(built.dependencies)
        );
    }
}
struct PrepareOnlyRpc {
    allow_prepare: bool,
}
impl outbe_ocomp::vote_submitter::VoteSubmissionRpcV1 for PrepareOnlyRpc {
    type Error = std::io::Error;
    fn chain_id(&self) -> Result<u64, Self::Error> {
        assert!(self.allow_prepare);
        Ok(copied_native::chain().chain().id())
    }
    fn canonical_nonce(&self, _: Address) -> Result<u64, Self::Error> {
        assert!(self.allow_prepare);
        Ok(0)
    }
    fn gas_price(&self) -> Result<u128, Self::Error> {
        assert!(self.allow_prepare);
        Ok(1)
    }
    fn send_raw_transaction(&self, _: &[u8], _: B256) -> Result<B256, Self::Error> {
        panic!("this fixture must never submit a transaction")
    }
    fn transaction_receipt(
        &self,
        _: B256,
    ) -> Result<Option<outbe_ocomp::vote_submitter::VoteReceiptV1>, Self::Error> {
        panic!("foreign journal must fail before RPC")
    }
    fn canonical_block(
        &self,
        _: u64,
    ) -> Result<Option<outbe_ocomp::vote_submitter::VoteBlockV1>, Self::Error> {
        panic!("foreign journal must fail before RPC")
    }
    fn finalized_block(&self) -> Result<outbe_ocomp::vote_submitter::VoteBlockV1, Self::Error> {
        panic!("foreign journal must fail before RPC")
    }
}

#[test]
fn copied_foreign_signed_materialization_journal_is_rejected_without_rewriting_recipient_key() {
    use outbe_ocomp::nod_materialization_submitter::{
        NodMaterializationSubmissionConfigV1, NodMaterializationSubmissionErrorV1,
        NodMaterializationSubmitterV1,
    };
    use outbe_primitives::signer::OutbeEvmSigner;
    let donor = tempfile::tempdir().unwrap();
    let receiver = tempfile::tempdir().unwrap();
    let public = donor.path().join("public");
    let f = fixture(&public, 0x71, WorldwideDay::new(20_260_725), 257);
    let built = build_remaining(&public, &f, &pending_head(&f)).unwrap();
    let signer = OutbeEvmSigner::from_secret_bytes([0x11; 32]).unwrap();
    let journal = donor.path().join("signed-journal");
    let mut submitter = NodMaterializationSubmitterV1::open(
        NodMaterializationSubmissionConfigV1 {
            journal_root: journal.clone(),
            expected_chain_id: copied_native::chain().chain().id(),
            sender_address: signer.address(),
            limits: poc_schema_limits(),
        },
        PrepareOnlyRpc {
            allow_prepare: true,
        },
        signer,
    )
    .unwrap();
    submitter
        .reconcile(f.job_id, &protected_batch(&f, &built.batch))
        .unwrap();
    drop(submitter);
    let original = fs::read(journal.join("submission-v1.json")).unwrap();
    copied_native::copy_tree(&journal, &receiver.path().join("signed-journal"));
    donor.close().unwrap();
    // Deliberately copied sender-owned data is a negative control, never a portable authority.
    let key_path = receiver.path().join("recipient-key.hex");
    fs::write(&key_path, format!("{}\n", hex::encode([0x22; 32]))).unwrap();
    let key_before = fs::read(&key_path).unwrap();
    let recipient = OutbeEvmSigner::from_secret_bytes([0x22; 32]).unwrap();
    let mut reopened = NodMaterializationSubmitterV1::open(
        NodMaterializationSubmissionConfigV1 {
            journal_root: receiver.path().join("signed-journal"),
            expected_chain_id: copied_native::chain().chain().id(),
            sender_address: recipient.address(),
            limits: poc_schema_limits(),
        },
        PrepareOnlyRpc {
            allow_prepare: false,
        },
        recipient,
    )
    .unwrap();
    assert!(matches!(
        reopened.reconcile(f.job_id, &protected_batch(&f, &built.batch)),
        Err(NodMaterializationSubmissionErrorV1::ConflictingReplay)
    ));
    assert_eq!(fs::read(&key_path).unwrap(), key_before);
    assert_eq!(
        fs::read(receiver.path().join("signed-journal/submission-v1.json")).unwrap(),
        original
    );
}

mod copied_resident_authority;
