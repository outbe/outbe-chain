//! frames obligations for the offline OCOMP audit.
use super::*;

#[derive(Debug, Default)]
pub(crate) struct FrameAvailability {
    pub blocks: u64,
    pub transactions: u64,
}

#[derive(Debug)]
pub(crate) struct ClosureAudit {
    pub checkpoint: outbe_ocomp::discovery_spool::ClosureCheckpointInspectionV1,
    pub replay: FrameAvailability,
}

/// Observe the native closure positions without opening its writable store.
/// Historical previous may be sparse. Only the saved identities are required.
pub(crate) fn verify_closure(
    view: &crate::snapshot::native::RethReadOnlyView,
    root: &Path,
    projection: Option<outbe_primitives::projection::ProjectionCheckpoint>,
    maximum_transactions: Option<u64>,
) -> eyre::Result<ClosureAudit> {
    use alloy_consensus::Sealable;
    use outbe_ocomp::discovery_spool::inspect_closure_checkpoint;
    use outbe_primitives::{projection::ProjectionCheckpoint, OutbeHeader};
    use reth_ethereum::provider::db::tables;
    use reth_provider::{BlockHashReader, HeaderProvider};

    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: view.chain.genesis_hash(),
    };
    let checkpoint = inspect_closure_checkpoint(root, baseline).map_err(|error| {
        if missing_native_input(&error) {
            eyre::Report::new(error).wrap_err(
                SnapshotIncompleteError::MissingNativeClosureCheckpoint {
                    root: root.display().to_string(),
                }
                .into_incomplete(),
            )
        } else {
            error.into()
        }
    })?;
    let tx = view.read_transaction()?;
    for (name, point) in [
        ("baseline", checkpoint.baseline),
        ("previous", checkpoint.previous),
        ("current", checkpoint.current),
    ] {
        let number = point.block_number;
        let header = match tx.get::<tables::Headers<OutbeHeader>>(number)? {
            Some(header) => Some(header),
            None => view.static_files.header_by_number(number)?,
        }
        .ok_or_else(|| {
            SnapshotIncompleteError::MissingClosureHeader { name, number }.into_incomplete()
        })?;
        let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(number)?,
        }
        .ok_or_else(|| {
            SnapshotIncompleteError::MissingClosureCanonicalHash { name, number }.into_incomplete()
        })?;
        require(
            header.inner.number == number && header.hash_slow() == hash && point.block_hash == hash,
            SnapshotOcompError::ClosureHeaderMismatch {
                name: name.to_string(),
                number,
            },
        )?;
    }
    match projection {
        Some(projected) => {
            require(
                projected.block_number > checkpoint.current.block_number
                    || (projected.block_number == checkpoint.current.block_number
                        && projected.block_hash == checkpoint.current.block_hash),
                SnapshotOcompError::ClosureAheadOrConflictsWithProjection,
            )?;
        }
        None => {
            require(
                checkpoint.current.block_number == 0,
                SnapshotOcompError::NonzeroClosureWithoutProjection,
            )?;
        }
    }
    let replay = if checkpoint.current.block_number < view.progress.finalized.number {
        // The strict comparison proves addition cannot overflow, even at MAX.
        verify_retained_frames(
            view,
            checkpoint.current.block_number + 1,
            view.progress.finalized.clone(),
            maximum_transactions,
        )?
    } else {
        FrameAvailability::default()
    };
    Ok(ClosureAudit { checkpoint, replay })
}

/// Check the retained inputs used by ordinary OCOMP replay, one transaction and
/// receipt at a time. This is availability/identity validation, not EVM replay.
pub(crate) fn verify_retained_frames(
    view: &crate::snapshot::native::RethReadOnlyView,
    start: u64,
    end: outbe_snapshot::manifest::BlockIdentity,
    maximum_transactions: Option<u64>,
) -> eyre::Result<FrameAvailability> {
    visit_retained_frames(view, start, end, maximum_transactions, &mut |_| Ok(()))
}

pub(super) fn visit_retained_frames(
    view: &crate::snapshot::native::RethReadOnlyView,
    start: u64,
    end: outbe_snapshot::manifest::BlockIdentity,
    maximum_transactions: Option<u64>,
    visitor: &mut impl FnMut(&outbe_primitives::OutbeReceipt) -> eyre::Result<()>,
) -> eyre::Result<FrameAvailability> {
    use alloy_consensus::Sealable;
    use outbe_primitives::{OutbeHeader, OutbeReceipt};
    use reth_ethereum::provider::db::tables;
    use reth_provider::{
        BlockHashReader, HeaderProvider, ReceiptProvider, StaticFileSegment, TransactionsProvider,
    };

    let mut result = FrameAvailability::default();
    if start > end.number {
        return Ok(result);
    }
    let expected_end = B256::try_from(hex::decode(&end.hash)?.as_slice())?;
    let tx = view.read_transaction()?;
    let mut previous = None;
    for height in start..=end.number {
        let header = match tx.get::<tables::Headers<OutbeHeader>>(height)? {
            Some(header) => Some(header),
            None => view.static_files.header_by_number(height)?,
        }
        .ok_or_else(|| SnapshotIncompleteError::MissingReplayHeader { height }.into_incomplete())?;
        let hash = match tx.get::<tables::CanonicalHeaders>(height)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(height)?,
        }
        .ok_or_else(|| {
            SnapshotIncompleteError::MissingReplayCanonicalHash { height }.into_incomplete()
        })?;
        require(
            header.inner.number == height && header.hash_slow() == hash,
            SnapshotOcompError::ReplayHeaderIdentityMismatch { height },
        )?;
        if let Some(previous) = previous {
            require(
                header.inner.parent_hash == previous,
                SnapshotOcompError::ReplayHeaderParentMismatch { height },
            )?;
        }
        previous = Some(hash);
        require(
            height != end.number || hash == expected_end,
            SnapshotOcompError::ReplayTargetHashMismatch { height },
        )?;
        let indices = tx.get::<tables::BlockBodyIndices>(height)?.ok_or_else(|| {
            SnapshotIncompleteError::MissingReplayBodyIndices { height }.into_incomplete()
        })?;
        let tx_end = indices
            .first_tx_num
            .checked_add(indices.tx_count)
            .ok_or(SnapshotOcompError::ReplayTxRangeOverflow { height })?;
        for number in indices.first_tx_num..tx_end {
            require(
                maximum_transactions.is_none_or(|maximum| result.transactions < maximum),
                SnapshotIncompleteError::ReplayFrameScanStopped {
                    height,
                    end_number: end.number,
                    number,
                    tx_end,
                    blocks: result.blocks,
                    transactions: result.transactions,
                }
                .into_incomplete(),
            )?;
            // Match the pinned Reth provider. Transactions are static-only.
            // Receipts choose one native backend by its high-water mark. A hole
            // below that mark must not be disguised by a different backend.
            view.static_files
                .transaction_by_id(number)?
                .ok_or_else(|| {
                    SnapshotIncompleteError::MissingReplayTransaction { number, height }
                        .into_incomplete()
                })?;
            let receipt = view
                .static_files
                .get_with_static_file_or_database(
                    StaticFileSegment::Receipts,
                    number,
                    |files| files.receipt(number),
                    || Ok(tx.get::<tables::Receipts<OutbeReceipt>>(number)?),
                )?
                .ok_or_else(|| {
                    SnapshotIncompleteError::MissingReplayReceipt { number, height }
                        .into_incomplete()
                })?;
            visitor(&receipt)?;
            result.transactions = result
                .transactions
                .checked_add(1)
                .ok_or(SnapshotOcompError::ReplayTxCountOverflow)?;
        }
        result.blocks = result
            .blocks
            .checked_add(1)
            .ok_or(SnapshotOcompError::ReplayBlockCountOverflow)?;
    }
    Ok(result)
}

/// Use one retained request frame as a locator, then authenticate its intent
/// against verified current E. No historical state or retired spool is needed.
pub(crate) fn locate_request_job(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    (request_height, expected_job, expected_day): (u64, B256, WorldwideDay),
    maximum_transactions: Option<u64>,
) -> eyre::Result<OcompJobRecordV1> {
    use alloy_consensus::{Sealable, TxReceipt};
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;
    use outbe_primitives::addresses::METADOSIS_ADDRESS;

    let header = view.header(request_height)?.ok_or_else(|| {
        SnapshotIncompleteError::MissingOcompRequestHeader { request_height }.into_incomplete()
    })?;
    let hash = header.hash_slow();
    let mut request = None;
    visit_retained_frames(
        view,
        request_height,
        outbe_snapshot::manifest::BlockIdentity {
            number: request_height,
            hash: hex::encode(hash),
        },
        maximum_transactions,
        &mut |receipt| {
            if !receipt.status() {
                return Ok(());
            }
            for log in receipt.logs() {
                if log.address != METADOSIS_ADDRESS
                    || log.data.topics().first()
                        != Some(&IMetadosis::OffchainJobRequested::SIGNATURE_HASH)
                {
                    continue;
                }
                let event = IMetadosis::OffchainJobRequested::decode_log(log)?;
                require(
                    request.replace(event.data).is_none(),
                    SnapshotOcompError::MultipleRequestsInFrame { request_height },
                )?;
            }
            Ok(())
        },
    )?;
    let event = request.ok_or(SnapshotOcompError::NoRequestInFrame { request_height })?;
    require(
        event.wwd == expected_day.value(),
        SnapshotOcompError::RequestEventDayMismatch,
    )?;
    let job = state.metadosis_job(event.intentId, expected_day, Some(expected_job))?;
    let limits = poc_schema_limits();
    require(
        (
            job.intent_height,
            job.intent.logical_evaluation_height,
            job.intent.logical_evaluation_time,
            job.intent.pending_nonce,
            job.intent.attempt,
            job.intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)?,
        ) == (
            request_height,
            request_height,
            header.inner.timestamp,
            event.pendingNonce,
            event.attempt,
            event.activationPreconditionsHash,
        ),
        SnapshotOcompError::RequestEventContextMismatch,
    )?;
    require(
        job.intent.job_id(hash, header.inner.state_root, &limits)? == expected_job,
        SnapshotOcompError::ArtifactJobIdMismatch,
    )?;
    Ok(job)
}
