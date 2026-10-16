//! pins obligations for the offline OCOMP audit.
use super::*;

pub(crate) struct VerifiedPin {
    pub candidate: outbe_node::ocomp::retention::CandidatePinV1,
    pub job: OcompJobRecordV1,
    /// Retention required by this local pin only. A false value cannot waive
    /// independent canonical active-job or shared-lease obligations.
    pub requires_source: bool,
    pub export: Option<outbe_node::ocomp::retention::ExportAuthorityV1>,
}

/// Bind a natively decoded journal record to immutable current-E authority.
/// Local lifecycle progress may lag canonical completion. It is not a claim
/// that the donor executed, exported or materialized the completed job.
pub(crate) fn verify_pin_authority(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    key: B256,
    record: &outbe_node::ocomp::retention::PinRecordV1,
) -> eyre::Result<VerifiedPin> {
    use alloy_consensus::Sealable;
    use outbe_node::ocomp::retention::PinStateV1;
    let candidate = match record.state {
        PinStateV1::AwaitingJobFinalization { candidate }
        | PinStateV1::Finalized { candidate, .. }
        | PinStateV1::Exported { candidate, .. }
        | PinStateV1::Terminal { candidate, .. }
        | PinStateV1::GcPending { candidate, .. }
        | PinStateV1::Released { candidate, .. } => candidate,
    };
    if key != candidate.block_hash {
        return Err(SnapshotOcompError::RetentionKeyBlockHashMismatch.into());
    }
    let expected_job = match record.state {
        PinStateV1::AwaitingJobFinalization { .. } => None,
        PinStateV1::Finalized { job_id, .. }
        | PinStateV1::Exported { job_id, .. }
        | PinStateV1::Terminal { job_id, .. }
        | PinStateV1::GcPending { job_id, .. }
        | PinStateV1::Released { job_id, .. } => Some(job_id),
    };
    let job = state.metadosis_job(
        candidate.intent_id,
        WorldwideDay::new(candidate.wwd),
        expected_job,
    )?;
    if job.intent_height != candidate.block_number
        || job.intent.protocol_bundle_hash != candidate.protocol_bundle_hash
        || job.intent.input_lease_id()? != candidate.input_lease_id
    {
        return Err(SnapshotOcompError::RetainedCandidateRequestMismatch.into());
    }
    // The canonical getter validates finalized B bindings. Awaiting pins also
    // need the retained request header even before canonical finality exists.
    let number = candidate.block_number;
    let header = view.header(number)?.ok_or_else(|| {
        SnapshotIncompleteError::MissingRetainedPinRequestHeader { number }.into_incomplete()
    })?;
    let canonical_hash = view.canonical_hash(number)?.ok_or_else(|| {
        SnapshotIncompleteError::MissingCanonicalPinRequestHash { number }.into_incomplete()
    })?;
    if header.inner.number != number
        || header.hash_slow() != canonical_hash
        || canonical_hash != candidate.block_hash
        || header.inner.state_root != candidate.state_root
    {
        return Err(SnapshotOcompError::RetainedCandidateHeaderMismatch { number }.into());
    }
    match record.state {
        PinStateV1::Finalized {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::Exported {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::Terminal {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        }
        | PinStateV1::GcPending {
            finality_recorded_height,
            open_height,
            deadline_height,
            ..
        } => {
            let finalized = job
                .finalized
                .as_ref()
                .ok_or(SnapshotOcompError::RetainedFinalizedPinLacksFinality)?;
            if finalized.finality_recorded_height != finality_recorded_height
                || finalized.open_height != open_height
                || finalized.deadline_height != deadline_height
            {
                return Err(SnapshotOcompError::RetainedPinFinalityWindowMismatch.into());
            }
        }
        PinStateV1::AwaitingJobFinalization { .. } | PinStateV1::Released { .. } => {}
    }
    let export = match record.state {
        PinStateV1::Exported { export, .. } => Some(export),
        PinStateV1::Terminal { export, .. }
        | PinStateV1::GcPending { export, .. }
        | PinStateV1::Released { export, .. } => export,
        _ => None,
    };
    Ok(VerifiedPin {
        candidate,
        job,
        requires_source: !matches!(
            record.state,
            PinStateV1::GcPending { .. } | PinStateV1::Released { .. }
        ),
        export,
    })
}

/// Verify complete Tribute coverage at the job's exact native request checkpoint.
/// This also supports awaiting-finality pins without asserting their finality.
pub(crate) fn verify_lease_inputs(
    history: &crate::snapshot::native::HistoricalStateReader,
    view: &crate::snapshot::native::RethReadOnlyView,
    job: &OcompJobRecordV1,
    maximum_records: Option<u64>,
) -> eyre::Result<()> {
    use alloy_consensus::Sealable;
    use outbe_primitives::{
        addresses::TRIBUTE_ADDRESS,
        error::{PrecompileError, Result as StorageResult},
        storage::{
            proof::AccountStorageProof,
            readonly::{ReadOnlyStorageProvider, StorageReader},
            StorageHandle,
        },
    };
    use outbe_tee::tribute_source::{TributeSourceCheckpoint, TributeSourceSubject};
    use outbe_tribute::state_proof::{
        expected_record_words, TributeDayProofVerifier, TributeRecordStateProof,
    };
    use reth_ethereum::trie::TrieInput;
    use reth_provider::StateProvider;

    struct NativeReader<'a>(&'a dyn StateProvider);
    impl StorageReader for NativeReader<'_> {
        fn read_storage(
            &self,
            address: alloy_primitives::Address,
            key: B256,
        ) -> StorageResult<U256> {
            self.0
                .storage(address, key)
                .map(|v| v.unwrap_or_default())
                .map_err(|error| PrecompileError::storage(error.to_string()))
        }
    }
    let number = job.intent_height;
    let header = view
        .header(number)?
        .ok_or_else(|| Incomplete(format!("missing Tribute request header B={number}")))?;
    let hash = view
        .canonical_hash(number)?
        .ok_or_else(|| Incomplete(format!("missing Tribute request hash B={number}")))?;
    if header.inner.number != number || header.hash_slow() != hash {
        return Err(SnapshotOcompError::SourceConflictingRequestIdentity.into());
    }
    if job.finalized.as_ref().is_some_and(|f| {
        f.finalized_request_block_hash != hash
            || f.finalized_request_state_root != header.inner.state_root
    }) {
        return Err(SnapshotOcompError::SourceConflictingRequestIdentity.into());
    }
    let subject = TributeSourceSubject {
        checkpoint: TributeSourceCheckpoint {
            block_number: number,
            block_hash: hash,
            state_root: header.inner.state_root,
        },
        chain_id: view.chain.chain().id(),
        worldwide_day: WorldwideDay::new(job.intent.wwd),
        tribute_count: job.intent.authenticated_day_count,
    };
    let node_subject = subject.into();
    let limits = outbe_tee::tribute_source::proof_limits();
    history.with_state(hash, |state| {
        let mut readonly = ReadOnlyStorageProvider::new_with_chain_identity(
            NativeReader(state),
            subject.chain_id,
            view.chain.genesis_hash(),
        );
        let length_slot = StorageHandle::enter(&mut readonly, |storage| -> StorageResult<_> {
            let contract = outbe_tribute::TributeContract::new(storage);
            let ids = contract.day_ids.list(&subject.worldwide_day);
            if ids.len()? != subject.tribute_count {
                return Err(PrecompileError::storage(
                    "native Tribute day count mismatch",
                ));
            }
            Ok(B256::from(ids.base_slot().to_be_bytes::<32>()))
        })
        .map_err(eyre::Report::new)?;
        let length_proof = state.proof(TrieInput::default(), TRIBUTE_ADDRESS, &[length_slot])?;
        let length_proof = AccountStorageProof::try_from(&length_proof)?;
        let mut verifier = TributeDayProofVerifier::new(node_subject, &length_proof, limits)?;
        for index in 0..subject.tribute_count {
            if maximum_records.is_some_and(|limit| u64::from(index) >= limit) {
                return Err(Incomplete(format!(
                    "Tribute source scan stopped at {index}/{} records",
                    subject.tribute_count
                ))
                .into());
            }
            let (tribute, words) =
                StorageHandle::enter(&mut readonly, |storage| -> StorageResult<_> {
                    let contract = outbe_tribute::TributeContract::new(storage.clone());
                    let id = contract
                        .day_ids
                        .list(&subject.worldwide_day)
                        .get(index)?
                        .ok_or_else(|| {
                            PrecompileError::storage("native Tribute day index is missing")
                        })?;
                    let record = outbe_tribute::state_api::get_retained_record(&storage, id)?
                        .ok_or_else(|| {
                            PrecompileError::storage("native Tribute record is missing")
                        })?;
                    let tribute = record
                        .encrypted()
                        .ok_or_else(|| PrecompileError::storage("native Tribute is not encrypted"))?
                        .clone();
                    let words = expected_record_words(&contract, &node_subject, index, &tribute)?;
                    Ok((tribute, words))
                })
                .map_err(eyre::Report::new)?;
            let slots = words.iter().map(|(slot, _)| *slot).collect::<Vec<_>>();
            let proof = state.proof(TrieInput::default(), TRIBUTE_ADDRESS, &slots)?;
            verifier.verify_next(
                &TributeRecordStateProof {
                    day_index: index,
                    tribute,
                    account: AccountStorageProof::try_from(&proof)?,
                },
                limits,
            )?;
        }
        verifier.finish()?;
        Ok(())
    })
}
