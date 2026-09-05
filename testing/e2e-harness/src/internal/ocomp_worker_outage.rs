//! Evidence for a post-export worker fault, never an alternative compute path.
use alloy_primitives::B256;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct WorkerOutageEvidence {
    pub exports: Vec<ExportEvidence>,
    pub stops: Vec<WorkerStopEvidence>,
    pub cut_heads: Vec<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExportEvidence {
    pub validator_index: u8,
    pub job_id: B256,
    pub receipt_digest: B256,
    pub source_generation: u64,
    pub lease_generation: u64,
    pub manifest_hash: B256,
    pub checkpoint_height: u64,
    pub checkpoint_hash: B256,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkerStopEvidence {
    pub validator_index: u8,
    pub worker_ordinal: u32,
    pub pid: u32,
    pub signal_at_millis: u64,
    pub signal_error: Option<String>,
    pub reaped_at_millis: Option<u64>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub wait_error: Option<String>,
}

#[cfg(feature = "ocomp-integration")]
mod observation {
    use super::*;
    use crate::world::rpc::FinalizedCheckpoint;
    use eyre::{ensure, Result};
    use outbe_ocomp::{
        cas::{CasLimits, FilesystemCasReader},
        export_receipt::{ExportReceiptError, ExportReceiptReader, VerifiedExportReceipt},
    };
    use outbe_ocomp_protocol::{
        input::CheckpointIdentityV1,
        profile::{poc_schema_limits, ProtocolBundleV1},
        state::OcompJobRecordV1,
    };
    use std::path::Path;

    pub(crate) fn publication_pending(error: &ExportReceiptError) -> bool {
        matches!(
            error,
            ExportReceiptError::MissingPreparation
                | ExportReceiptError::MissingReceipt
                | ExportReceiptError::AmbiguousTemporary(_)
        )
    }

    pub(crate) fn load_publication(
        root: &Path,
        job_id: B256,
    ) -> Result<Option<VerifiedExportReceipt>> {
        let limits = poc_schema_limits();
        // A live publisher fsyncs its temporary locator before renaming it.
        // The caller's deadline bounds this pending observation; only a later
        // complete load_exact can count as a successful export.
        let reader = match ExportReceiptReader::try_open(
            root.join("exporter-v1/receipts"),
            job_id,
            limits,
        ) {
            Ok(Some(reader)) => reader,
            Ok(None) => return Ok(None),
            Err(error) if publication_pending(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let cas = FilesystemCasReader::open(
            root.join("cas-v1"),
            CasLimits {
                // Existing compiled decoder allocation ceiling, not a job-data cap.
                max_object_bytes: u64::try_from(limits.codec.max_allocation_bytes)?,
                max_total_bytes: u64::MAX,
            },
        )?;
        let receipt = match reader.load_exact(&cas) {
            Ok(receipt) => receipt,
            Err(error) if publication_pending(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(receipt))
    }

    pub(crate) fn observe_export(
        root: &Path,
        validator_index: u8,
        record: &OcompJobRecordV1,
        checkpoint: FinalizedCheckpoint,
        bundle: &ProtocolBundleV1,
    ) -> Result<Option<ExportEvidence>> {
        let limits = poc_schema_limits();
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("job lacks canonical finality binding"))?;
        ensure!(
            finalized.finalized_request_block_hash == checkpoint.block_hash
                && finalized.finalized_request_state_root == checkpoint.state_root,
            "request checkpoint differs from canonical finality binding"
        );
        let job_id = finalized.job_id;
        let Some(receipt) = load_publication(root, job_id)? else {
            return Ok(None);
        };
        let intent = &record.intent;
        let expected = CheckpointIdentityV1 {
            finalized_block_number: checkpoint.height,
            finalized_block_hash: checkpoint.block_hash,
            finalized_state_root: checkpoint.state_root,
            finalized_ce_root: intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )?,
        };
        ensure!(
            record.intent_height == checkpoint.height,
            "export checkpoint is not the canonical request height"
        );
        let manifest = receipt.manifest();
        ensure!(
            receipt.job_id() == job_id
                && receipt.checkpoint() == &expected
                && manifest.checkpoint == expected
                && manifest.job_id == job_id
                && manifest.protocol_bundle_hash == intent.protocol_bundle_hash
                && bundle.protocol_bundle_hash(&limits)? == intent.protocol_bundle_hash
                && manifest.attempt == intent.attempt
                && manifest.wwd == intent.wwd
                && manifest.sealed_tribute_collection_key == intent.sealed_tribute_collection_key
                && manifest.sealed_tribute_collection_root == intent.sealed_tribute_collection_root
                && manifest.tribute_count == intent.authenticated_day_count
                && manifest.tribute_nominal_total == intent.authenticated_day_nominal
                && manifest.body_codec_id == bundle.tribute_body_codec_id
                && manifest.opening_codec_registry_hash == bundle.opening_codec_registry_hash()?,
            "validator-{validator_index} export differs from canonical job/input authority"
        );
        Ok(Some(ExportEvidence {
            validator_index,
            job_id,
            receipt_digest: receipt.receipt_ref().transport_digest,
            source_generation: receipt.source_pin_generation(),
            lease_generation: receipt.lease_generation(),
            manifest_hash: receipt.manifest_hash(),
            checkpoint_height: checkpoint.height,
            checkpoint_hash: checkpoint.block_hash,
        }))
    }
}

#[cfg(feature = "ocomp-integration")]
pub(crate) use observation::observe_export;

#[cfg(any(test, feature = "ocomp-integration"))]
pub(crate) fn terminate_cohort(
    workers: &mut [&mut crate::internal::proc::ChildGuard],
    evidence: &mut [WorkerStopEvidence],
) -> eyre::Result<()> {
    use std::{os::unix::process::ExitStatusExt as _, time::Duration};
    eyre::ensure!(
        workers.len() == 4 && evidence.len() == workers.len(),
        "incomplete worker cohort"
    );
    for (worker, record) in workers.iter_mut().zip(evidence.iter()) {
        eyre::ensure!(
            worker.pid() == record.pid && worker.exit_status()?.is_none(),
            "worker identity changed or exited before cohort fault"
        );
    }
    // No waits or RPC between signals. Even a partial signalling failure must
    // not prevent signalling the other owned members or collecting outcomes.
    for (worker, record) in workers.iter_mut().zip(evidence.iter_mut()) {
        record.signal_at_millis = now_millis();
        record.signal_error = worker
            .signal_fault()
            .err()
            .map(|error| format!("{error:#}"));
    }
    for (worker, record) in workers.iter_mut().zip(evidence.iter_mut()) {
        match worker.reap_fault(Duration::from_secs(15)) {
            Ok(status) => {
                record.reaped_at_millis = Some(now_millis());
                record.exit_code = status.code();
                record.exit_signal = status.signal();
            }
            Err(error) => record.wait_error = Some(format!("{error:#}")),
        }
    }
    eyre::ensure!(
        evidence.iter().all(|record| record.signal_error.is_none()
            && record.wait_error.is_none()
            && record.exit_signal == Some(9)),
        "worker cohort fault incomplete: {evidence:?}"
    );
    Ok(())
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_millis()
        .try_into()
        .expect("milliseconds fit u64")
}

#[cfg(any(test, feature = "ocomp-integration"))]
pub(crate) fn require_pre_open_cut(heads: &[u64], open: u64) -> eyre::Result<()> {
    eyre::ensure!(
        heads.len() == 4 && heads.iter().all(|height| *height < open),
        "post-export worker fault missed the pre-open boundary: heads={heads:?}, open={open}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_requires_all_four_successfully_observed_heads_strictly_before_open() {
        require_pre_open_cut(&[100, 101, 102, 103], 104).unwrap();
        assert!(require_pre_open_cut(&[100, 101, 102, 104], 104).is_err());
        assert!(require_pre_open_cut(&[100, 101, 102, 105], 104).is_err());
        assert!(require_pre_open_cut(&[100, 101, 102], 104).is_err());
        assert!(require_pre_open_cut(&[], 104).is_err());
    }

    #[test]
    fn every_owned_worker_is_signalled_before_any_reap() {
        use crate::internal::proc::ChildGuard;
        let mut children = (0..4)
            .map(|index| {
                let mut command = std::process::Command::new("sleep");
                command.arg("30");
                ChildGuard::spawn(format!("fault-test-{index}"), command).unwrap()
            })
            .collect::<Vec<_>>();
        let mut evidence = children
            .iter()
            .enumerate()
            .map(|(index, child)| WorkerStopEvidence {
                validator_index: index as u8,
                worker_ordinal: 0,
                pid: child.pid(),
                signal_at_millis: 0,
                signal_error: None,
                reaped_at_millis: None,
                exit_code: None,
                exit_signal: None,
                wait_error: None,
            })
            .collect::<Vec<_>>();
        terminate_cohort(&mut children.iter_mut().collect::<Vec<_>>(), &mut evidence).unwrap();
        let last_signal = evidence
            .iter()
            .map(|record| record.signal_at_millis)
            .max()
            .unwrap();
        assert!(evidence
            .iter()
            .all(|record| record.reaped_at_millis.unwrap() >= last_signal
                && record.exit_signal == Some(9)));
        for child in &mut children {
            assert!(child.exit_status().unwrap().is_some());
            assert!(
                child.signal_fault().is_err(),
                "an exited child cannot count as a new fault"
            );
        }
    }

    #[test]
    fn exited_member_rejects_fault_without_stopping_live_peers_and_wait_timeout_is_an_error() {
        use crate::internal::proc::ChildGuard;
        use std::time::Duration;
        let mut children = (0..4)
            .map(|index| {
                let mut command = std::process::Command::new("sleep");
                command.arg("30");
                ChildGuard::spawn(format!("incomplete-fault-test-{index}"), command).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(children[0].reap_fault(Duration::ZERO).is_err());
        assert!(children[0].exit_status().unwrap().is_none());
        children[3].signal_fault().unwrap();
        children[3].reap_fault(Duration::from_secs(5)).unwrap();
        let mut evidence = children
            .iter()
            .enumerate()
            .map(|(index, child)| WorkerStopEvidence {
                validator_index: index as u8,
                worker_ordinal: 0,
                pid: child.pid(),
                signal_at_millis: 0,
                signal_error: None,
                reaped_at_millis: None,
                exit_code: None,
                exit_signal: None,
                wait_error: None,
            })
            .collect::<Vec<_>>();
        assert!(
            terminate_cohort(&mut children.iter_mut().collect::<Vec<_>>(), &mut evidence).is_err()
        );
        assert!(evidence
            .iter()
            .all(|item| item.signal_at_millis == 0 && item.reaped_at_millis.is_none()));
        for child in &mut children[..3] {
            assert!(child.exit_status().unwrap().is_none());
            child.signal_fault().unwrap();
            child.reap_fault(Duration::from_secs(5)).unwrap();
        }
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn only_explicit_incomplete_publication_is_retryable() {
        use outbe_ocomp::export_receipt::ExportReceiptError as Error;
        assert!(observation::publication_pending(&Error::MissingPreparation));
        assert!(observation::publication_pending(&Error::MissingReceipt));
        assert!(observation::publication_pending(
            &Error::AmbiguousTemporary("receipt.ref.tmp".into())
        ));
        for error in [
            Error::InvalidEnvelope,
            Error::ConflictingReceipt,
            Error::Abstained,
            Error::AuthorityMismatch("job"),
        ] {
            assert!(!observation::publication_pending(&error));
        }
    }

    #[cfg(feature = "ocomp-integration")]
    #[test]
    fn readonly_publication_rejects_nonempty_corruption_and_foreign_job_directory() {
        use outbe_ocomp::{
            cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
            export_receipt::{ExportReceiptCandidate, ExportReceiptStore},
        };
        use outbe_ocomp_protocol::{
            common::BoundedBytes,
            input::{CheckpointIdentityV1, Compression, InputManifestV1},
            profile::poc_schema_limits,
            ObjectKind, SnapshotExportCommittedV1, SnapshotHandoffV1,
        };
        let dir = tempfile::tempdir().unwrap();
        let limits = poc_schema_limits();
        let job_id = B256::repeat_byte(1);
        assert!(observation::load_publication(dir.path(), job_id)
            .unwrap()
            .is_none());
        let cas_limits = CasLimits {
            max_object_bytes: limits.codec.max_allocation_bytes as u64,
            max_total_bytes: u64::MAX,
        };
        let cas = FilesystemCas::open(
            dir.path().join("cas-v1"),
            CasWriterRole::SnapshotExporter,
            cas_limits,
        )
        .unwrap();
        let reader = FilesystemCasReader::open(dir.path().join("cas-v1"), cas_limits).unwrap();
        let manifest = InputManifestV1 {
            protocol_bundle_hash: B256::repeat_byte(2),
            job_id,
            attempt: 0,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: 91,
                finalized_block_hash: B256::repeat_byte(3),
                finalized_state_root: B256::repeat_byte(4),
                finalized_ce_root: B256::repeat_byte(5),
                ce_schema_version: 1,
            },
            wwd: 20260905,
            sealed_tribute_collection_key: B256::repeat_byte(6),
            sealed_tribute_collection_root: B256::repeat_byte(7),
            tribute_count: 2,
            tribute_nominal_total: alloy_primitives::U256::from(46),
            input_chunk_count: 1,
            input_chunk_list_root: B256::repeat_byte(8),
            fidelity_opening_root: B256::repeat_byte(9),
            oracle_opening_root: B256::repeat_byte(10),
            exact_encoded_bytes: 128,
            exact_record_count: 2,
            body_codec_id: outbe_ocomp_protocol::registry::TRIBUTE_BODY_CODEC_ID,
            opening_codec_registry_hash: B256::repeat_byte(11),
            compression: Compression::None,
        };
        let manifest_hash = manifest.manifest_hash(&limits).unwrap();
        let mut manifest_ref = cas
            .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
            .unwrap();
        manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
        let handoff = SnapshotHandoffV1 {
            job_id,
            input_lease_id: B256::repeat_byte(12),
            pin_generation: 11,
            lease_generation: 17,
            checkpoint: manifest.checkpoint.clone(),
            canonical_lease_offer: BoundedBytes(vec![1, 2, 3]),
        };
        let committed = SnapshotExportCommittedV1 {
            job_id,
            pin_generation: 12,
            record_hash: B256::repeat_byte(13),
        };
        let receipts = dir.path().join("exporter-v1/receipts");
        let mut store = ExportReceiptStore::open(&receipts, job_id, limits).unwrap();
        store
            .record(
                &cas,
                &reader,
                ExportReceiptCandidate {
                    handoff: &handoff,
                    manifest_ref: &manifest_ref,
                    manifest_hash,
                    committed: &committed,
                },
            )
            .unwrap();
        drop(store);
        let loaded = observation::load_publication(dir.path(), job_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.manifest(), &manifest);
        let original = receipts.join(hex::encode(job_id));
        // Observe the real reader at both atomic-publication boundaries.
        // Persistent temporary state never counts as a verified receipt.
        for name in ["prepared.ref", "receipt.ref"] {
            let published = original.join(name);
            let temporary = original.join(format!("{name}.tmp"));
            std::fs::rename(&published, &temporary).unwrap();
            for _ in 0..2 {
                assert!(observation::load_publication(dir.path(), job_id)
                    .unwrap()
                    .is_none());
            }
            std::fs::rename(&temporary, &published).unwrap();
            assert_eq!(
                observation::load_publication(dir.path(), job_id)
                    .unwrap()
                    .unwrap()
                    .manifest(),
                &manifest
            );
        }
        let foreign_id = B256::repeat_byte(33);
        let foreign = receipts.join(hex::encode(foreign_id));
        std::fs::rename(&original, &foreign).unwrap();
        assert!(observation::load_publication(dir.path(), foreign_id).is_err());
        std::fs::rename(&foreign, &original).unwrap();
        std::fs::write(original.join("receipt.ref"), b"nonempty but invalid").unwrap();
        assert!(observation::load_publication(dir.path(), job_id).is_err());
    }
}
