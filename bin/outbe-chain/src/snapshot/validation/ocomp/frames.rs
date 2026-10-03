//! frames obligations for the offline OCOMP audit.
use super::*;

/// Counts from a complete native payout bitmap, including non-prefix payments.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BitmapAudit {
    pub words: u64,
    pub paid: u64,
    pub unpaid: u64,
}

pub(crate) fn verify_paid_bitmap(
    contributor_count: u32,
    paid_leaf_count: u32,
    maximum_words: Option<u64>,
    mut read_word: impl FnMut(u32) -> eyre::Result<U256>,
) -> eyre::Result<BitmapAudit> {
    ensure!(
        paid_leaf_count <= contributor_count,
        "paid leaf count exceeds certified count"
    );
    let words = u64::from(contributor_count).div_ceil(256);
    let mut paid = 0_u64;
    for index in 0..words {
        if maximum_words.is_some_and(|maximum| index >= maximum) {
            return Err(Incomplete(format!(
                "payout bitmap scan stopped at {index}/{words} words"
            ))
            .into());
        }
        let word = read_word(u32::try_from(index)?)?;
        let bits = (u64::from(contributor_count) - index * 256).min(256);
        if bits < 256 {
            ensure!(
                (word >> bits).is_zero(),
                "paid bitmap contains bits above certified contributor count"
            );
        }
        paid = paid
            .checked_add(word.count_ones() as u64)
            .ok_or_else(|| eyre::eyre!("paid bitmap count overflow"))?;
    }
    ensure!(
        paid == u64::from(paid_leaf_count),
        "paid bitmap popcount differs from paid leaf count"
    );
    Ok(BitmapAudit {
        words,
        paid,
        unpaid: u64::from(contributor_count) - paid,
    })
}

/// Native SeriesId::worldwide_day is intentionally permissive; validate its
/// stored spelling and date before using it to discover canonical obligations.
pub(crate) fn verify_series_day(id: SeriesId) -> eyre::Result<WorldwideDay> {
    let bytes = id.as_bytes();
    ensure!(
        bytes[..8].iter().all(u8::is_ascii_digit) && bytes[8] == b'-' && bytes[12] == b'-',
        "noncanonical series identity"
    );
    let day = id.worldwide_day();
    ensure!(
        day.value() != 0 && day.is_valid(),
        "invalid series worldwide day"
    );
    let issuance = [bytes[9], bytes[10], bytes[11]];
    ensure!(
        SeriesId::pack(day, issuance, bytes[13])? == id,
        "noncanonical series codes"
    );
    Ok(day)
}
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
/// Historical previous may be sparse; only the saved identities are required.
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
            eyre::Report::new(error).wrap_err(Incomplete(format!(
                "missing native closure checkpoint at {}",
                root.display()
            )))
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
        .ok_or_else(|| Incomplete(format!("missing closure {name} header at {number}")))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(number)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(number)?,
        }
        .ok_or_else(|| Incomplete(format!("missing closure {name} canonical hash at {number}")))?;
        ensure!(
            header.inner.number == number && header.hash_slow() == hash && point.block_hash == hash,
            "closure {name} differs from canonical header at {number}"
        );
    }
    match projection {
        Some(projected) => ensure!(
            projected.block_number >= checkpoint.current.block_number
                && (projected.block_number != checkpoint.current.block_number
                    || projected.block_hash == checkpoint.current.block_hash),
            "closure checkpoint is ahead of or conflicts with durable projection"
        ),
        None => ensure!(
            checkpoint.current.block_number == 0,
            "nonzero closure checkpoint exists without durable projection"
        ),
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
        .ok_or_else(|| Incomplete(format!("missing replay header at {height}")))?;
        let hash = match tx.get::<tables::CanonicalHeaders>(height)? {
            Some(hash) => Some(hash),
            None => view.static_files.block_hash(height)?,
        }
        .ok_or_else(|| Incomplete(format!("missing replay canonical hash at {height}")))?;
        ensure!(
            header.inner.number == height && header.hash_slow() == hash,
            "replay header differs from canonical identity at {height}"
        );
        if let Some(previous) = previous {
            ensure!(
                header.inner.parent_hash == previous,
                "replay header parent differs at {height}"
            );
        }
        previous = Some(hash);
        if height == end.number {
            ensure!(
                hash == expected_end,
                "replay target hash differs at {height}"
            );
        }
        let indices = tx
            .get::<tables::BlockBodyIndices>(height)?
            .ok_or_else(|| Incomplete(format!("missing replay body indices at {height}")))?;
        let tx_end = indices
            .first_tx_num
            .checked_add(indices.tx_count)
            .ok_or_else(|| eyre::eyre!("replay transaction range overflows at {height}"))?;
        for number in indices.first_tx_num..tx_end {
            if maximum_transactions.is_some_and(|maximum| result.transactions >= maximum) {
                return Err(Incomplete(format!(
                    "replay frame scan stopped at block {height}/{}, transaction {number}/{tx_end}; {} blocks, {} transactions visited",
                    end.number, result.blocks, result.transactions
                )).into());
            }
            // Match the pinned Reth provider: transactions are static-only;
            // receipts choose one native backend by its high-water mark. A hole
            // below that mark must not be disguised by a different backend.
            view.static_files
                .transaction_by_id(number)?
                .ok_or_else(|| {
                    Incomplete(format!(
                        "missing replay transaction {number} at block {height}"
                    ))
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
                    Incomplete(format!("missing replay receipt {number} at block {height}"))
                })?;
            visitor(&receipt)?;
            result.transactions = result
                .transactions
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("replay transaction count overflow"))?;
        }
        result.blocks = result
            .blocks
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("replay block count overflow"))?;
    }
    Ok(result)
}

/// Use one retained request frame as a locator, then authenticate its intent
/// against verified current E. No historical state or retired spool is needed.
pub(crate) fn locate_request_job(
    state: &CanonicalState<'_>,
    view: &crate::snapshot::native::RethReadOnlyView,
    request_height: u64,
    expected_job: B256,
    expected_day: WorldwideDay,
    maximum_transactions: Option<u64>,
) -> eyre::Result<OcompJobRecordV1> {
    use alloy_consensus::{Sealable, TxReceipt};
    use alloy_sol_types::SolEvent;
    use outbe_metadosis::precompile::IMetadosis;
    use outbe_primitives::addresses::METADOSIS_ADDRESS;

    let header = view
        .header(request_height)?
        .ok_or_else(|| Incomplete(format!("missing OCOMP request header B={request_height}")))?;
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
                ensure!(
                    request.replace(event.data).is_none(),
                    "request frame B={request_height} contains multiple OCOMP requests"
                );
            }
            Ok(())
        },
    )?;
    let event = request.ok_or_else(|| {
        eyre::eyre!("complete request frame B={request_height} contains no OCOMP request")
    })?;
    ensure!(
        event.wwd == expected_day.value(),
        "request event day differs from artifact"
    );
    let job = state.metadosis_job(event.intentId, expected_day, Some(expected_job))?;
    let limits = poc_schema_limits();
    ensure!(
        job.intent_height == request_height
            && job.intent.logical_evaluation_height == request_height
            && job.intent.logical_evaluation_time == header.inner.timestamp
            && job.intent.pending_nonce == event.pendingNonce
            && job.intent.attempt == event.attempt
            && job
                .intent
                .activation_preconditions
                .activation_preconditions_hash(&limits)?
                == event.activationPreconditionsHash,
        "request event or frozen block context differs from canonical job"
    );
    ensure!(
        job.intent.job_id(hash, header.inner.state_root, &limits)? == expected_job,
        "artifact JobId differs from canonical request identity"
    );
    Ok(job)
}
