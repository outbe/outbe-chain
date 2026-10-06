use super::*;

/// - `Open` while `Revealing`/`Clearing`.
/// - `Closed` past clearing, and `UnknownDay` for an unbriefed day. Both are acknowledged, since
///   nothing can make them applicable.
/// - `Err` before reveal, so the transport redelivers.
fn intake_state(stage: AuctionStage) -> Result<Intake> {
    match stage {
        AuctionStage::Revealing | AuctionStage::Clearing => Ok(Intake::Open),
        AuctionStage::Cleared | AuctionStage::Cancelled => Ok(Intake::Closed),
        AuctionStage::None => Ok(Intake::UnknownDay),
        _ => Err(DesisError::InvalidStageTransition.into()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intake {
    Open,
    Closed,
    UnknownDay,
}

/// The `InboundIgnored` reason for a message the intake no longer takes. `None` while it is open.
fn ignored_reason(stage: AuctionStage) -> Result<Option<u8>> {
    Ok(match intake_state(stage)? {
        Intake::Open => None,
        Intake::Closed => Some(IGNORED_OBSOLETE),
        Intake::UnknownDay => Some(IGNORED_NOT_FOUND),
    })
}

/// Where an inbound bid message comes from: the relaying caller, the day and the source chain.
#[derive(Clone, Copy)]
pub struct Inbound {
    pub caller: Address,
    pub worldwide_day: WorldwideDay,
    pub src_chain_id: u32,
}

fn emit_inbound_ignored(
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    src_chain_id: u32,
    reason: u8,
) -> Result<()> {
    contract.emit(IDesis::InboundIgnored {
        worldwideDay: worldwide_day.into(),
        srcChainId: src_chain_id,
        reason,
    })
}

/// Accept a relayed bid batch. Bids accumulate per source chain while the stage is `Revealing`.
/// Batches may arrive in any order over the unordered bridge, so a per-chain bitmap of
/// `batch_index` tracks completeness. The chain finalizes once its BIDS_DONE marker and every batch
/// have arrived (see `try_finalize_chain`). The first batch fixes the chain's `total_batches` and
/// every later one must agree. A redelivered batch (its bit already set) is an idempotent no-op, so
/// the transport may safely re-deliver. A batch past clearing, or for a day this chain never
/// briefed, is acknowledged with `InboundIgnored`: no later state could make it applicable.
pub fn process_bids_batch(
    storage: StorageHandle<'_>,
    inbound: Inbound,
    batch_index: u16,
    total_batches: u16,
    bids: Vec<BidData>,
) -> Result<()> {
    let Inbound {
        caller,
        worldwide_day,
        src_chain_id,
    } = inbound;
    require_origin_router(caller)?;
    require_nonzero_worldwide_day(worldwide_day)?;
    check_batch_shape(batch_index, total_batches, &bids)?;
    let mut contract = storage.contract::<DesisContract>();

    if let Some(reason) = ignored_reason(contract.read_stage(worldwide_day)?)? {
        return emit_inbound_ignored(&mut contract, worldwide_day, src_chain_id, reason);
    }

    let chain_key = DesisContract::chain_key(worldwide_day, src_chain_id);
    // All of a chain's batches must agree on total_batches, else a bad peer could set an
    // out-of-range bit and false-complete the set with a real batch missing.
    let stored_total = contract.chain_total_batches.read(&chain_key)?;
    if stored_total == 0 {
        contract
            .chain_total_batches
            .write(&chain_key, u32::from(total_batches))?;
    } else if u32::from(total_batches) != stored_total {
        return Err(PrecompileError::Revert(
            "processBidsBatch: batch total mismatch".into(),
        ));
    }

    let bit = U256::from(1u8) << (batch_index as usize);
    let mask = contract.chain_arrived_mask.read(&chain_key)?;
    if !(mask & bit).is_zero() {
        // This batch was already applied. Redelivery is idempotent.
        return Ok(());
    }

    for bid in &bids {
        contract.append_bid(worldwide_day, src_chain_id, bid)?;
    }
    contract.chain_arrived_mask.write(&chain_key, mask | bit)?;

    try_finalize_chain(&mut contract, worldwide_day, src_chain_id)
}

/// Accept a chain's BIDS_DONE completeness marker: the source relayed `total_batches` batches with
/// `total_bids` bids for this day. Stage semantics mirror `process_bids_batch`. A marker may land
/// before the batches it counts, which finalize the chain as they arrive. A marker the chain already
/// recorded is a no-op when it agrees and is acknowledged with `InboundIgnored` when it does not: the
/// first marker stands.
pub fn process_bids_done(
    storage: StorageHandle<'_>,
    inbound: Inbound,
    total_batches: u16,
    total_bids: u32,
) -> Result<()> {
    let Inbound {
        caller,
        worldwide_day,
        src_chain_id,
    } = inbound;
    require_origin_router(caller)?;
    require_nonzero_worldwide_day(worldwide_day)?;
    if total_batches == 0 || total_batches > 256 {
        return Err(PrecompileError::Revert(
            "processBidsDone: invalid total batches".into(),
        ));
    }
    let mut contract = storage.contract::<DesisContract>();

    if let Some(reason) = ignored_reason(contract.read_stage(worldwide_day)?)? {
        return emit_inbound_ignored(&mut contract, worldwide_day, src_chain_id, reason);
    }

    let chain_key = DesisContract::chain_key(worldwide_day, src_chain_id);
    let recorded_batches = contract.chain_done_batches.read(&chain_key)?;
    if recorded_batches != 0 {
        let same = recorded_batches == u32::from(total_batches)
            && contract.chain_done_bids.read(&chain_key)? == total_bids;
        if same {
            return Ok(());
        }
        return emit_inbound_ignored(&mut contract, worldwide_day, src_chain_id, IGNORED_CONFLICT);
    }

    contract
        .chain_done_batches
        .write(&chain_key, u32::from(total_batches))?;
    contract.chain_done_bids.write(&chain_key, total_bids)?;

    try_finalize_chain(&mut contract, worldwide_day, src_chain_id)
}

fn check_batch_shape(batch_index: u16, total_batches: u16, bids: &[BidData]) -> Result<()> {
    // The arrival bitmap is a U256, so at most 256 batches (batch_index 0..=255) are trackable.
    if total_batches == 0 || total_batches > MAX_BID_BATCHES || batch_index >= total_batches {
        return Err(PrecompileError::Revert(
            "processBidsBatch: invalid batch index/total".into(),
        ));
    }
    // Same reason as the currency check below: an over-wide batch is admissible
    // here but not at clearing, where the refund fan-out would reject the day.
    if bids.len() > MAX_BIDS_PER_BATCH {
        return Err(DesisError::BidBatchTooLarge(bids.len(), MAX_BIDS_PER_BATCH).into());
    }
    // Checked here because clearing cannot recover from it: an unspellable code would
    // otherwise surface as a day whose clearing reverts every block.
    if let Some(bad) = bids.iter().find(|bid| {
        SeriesId::currency_code(bid.issuance_currency).is_err()
            || SeriesId::currency_code(bid.reference_currency).is_err()
    }) {
        return Err(DesisError::UnspellableBidCurrency(
            bad.issuance_currency,
            bad.reference_currency,
        )
        .into());
    }
    Ok(())
}

/// Mark the chain done once its BIDS_DONE marker and every batch have arrived with matching totals.
/// Invoked from both arrival paths. Either side may land last over the unordered bridge. An
/// integrity mismatch (batch totals vs marker claims) keeps the chain not-done, so the deadline
/// skip excludes it.
fn try_finalize_chain(
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    chain_id: u32,
) -> Result<()> {
    let key = DesisContract::chain_key(worldwide_day, chain_id);
    if contract.chain_done.read(&key)? != 0 {
        return Ok(());
    }
    let claimed_batches = contract.chain_done_batches.read(&key)?;
    if claimed_batches == 0 {
        return Ok(()); // no marker yet
    }
    let total = contract.chain_total_batches.read(&key)?;
    let mask = contract.chain_arrived_mask.read(&key)?;
    let bid_count = contract.chain_bid_count.read(&key)?;
    if total != claimed_batches
        || mask.count_ones() as u32 != total
        || bid_count != contract.chain_done_bids.read(&key)?
    {
        return Ok(());
    }
    contract.chain_done.write(&key, 1u8)?;
    contract.emit(IDesis::ChainBidsDone {
        worldwideDay: worldwide_day.into(),
        srcChainId: chain_id,
        bidsCount: bid_count,
    })
}

fn require_origin_router(caller: Address) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(DesisError::UnauthorizedOrigin(caller).into());
    }
    Ok(())
}

fn require_nonzero_worldwide_day(worldwide_day: WorldwideDay) -> Result<()> {
    if worldwide_day.value() == 0 {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }
    Ok(())
}
