use super::*;

/// - `Open` while `RevealingBids`/`CollectingBids`.
/// - `Closed` once the day ended, and `UnknownDay` for a day that has not started. Both are acknowledged,
///   since nothing can make them applicable.
/// - `Err` before reveal, so the transport redelivers.
fn intake_state(stage: AuctionStage) -> Result<Intake> {
    match stage {
        AuctionStage::RevealingBids | AuctionStage::CollectingBids => Ok(Intake::Open),
        AuctionStage::Completed | AuctionStage::Canceled | AuctionStage::Failed => {
            Ok(Intake::Closed)
        }
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

/// The `IntexAuctionBidsIgnored` reason for a message the intake no longer takes. `None` while it is open.
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
    pub receive_id: B256,
    pub emission_day: EmissionDate,
    pub src_chain_id: u32,
}

fn emit_intex_auction_bids_ignored(
    contract: &mut DesisContract<'_>,
    inbound: Inbound,
    reason: u8,
) -> Result<()> {
    contract.emit(IDesis::IntexAuctionBidsIgnored {
        receiveId: inbound.receive_id,
        emissionDay: inbound.emission_day.into(),
        srcChainId: inbound.src_chain_id,
        reason,
    })
}

/// Accept a relayed bid batch. Bids accumulate per source chain while the stage is `RevealingBids`.
/// Batches may arrive in any order over the unordered bridge, so a per-chain bitmap of
/// `page_index` tracks completeness. The chain finalizes once its BIDS_DONE marker and every batch
/// have arrived (see `try_complete_intex_auction_bids`). The first batch fixes the chain's `total_pages` and
/// every later one must agree. A redelivered batch (its bit already set) is an idempotent no-op, so
/// the transport may safely re-deliver. A batch past clearing, or for a day that has not
/// started, is acknowledged with `IntexAuctionBidsIgnored`: no later state could make it applicable.
pub fn collect_intex_auction_bids(
    storage: StorageHandle<'_>,
    inbound: Inbound,
    page_index: u16,
    total_pages: u16,
    bids: Vec<IntexAuctionBidData>,
) -> Result<()> {
    let Inbound {
        caller,
        emission_day,
        src_chain_id,
        ..
    } = inbound;
    require_origin_router(caller)?;
    require_nonzero_emission_day(emission_day)?;
    check_page_shape(page_index, total_pages, &bids)?;
    let mut contract = storage.contract::<DesisContract>();

    if let Some(reason) = ignored_reason(contract.read_stage(emission_day)?)? {
        return emit_intex_auction_bids_ignored(&mut contract, inbound, reason);
    }

    let chain_key = DesisContract::chain_key(emission_day, src_chain_id);
    contract.track_intake_chain(emission_day, src_chain_id)?;
    // All of a chain's batches must agree on total_pages, else a bad peer could set an
    // out-of-range bit and false-complete the set with a real batch missing.
    let stored_total = contract.chain_total_pages.read(&chain_key)?;
    if stored_total == 0 {
        contract
            .chain_total_pages
            .write(&chain_key, u32::from(total_pages))?;
    } else if u32::from(total_pages) != stored_total {
        return Err(DesisError::PageTotalMismatch.into());
    }

    let bit = U256::from(1u8) << (page_index as usize);
    let mask = contract.chain_arrived_mask.read(&chain_key)?;
    if !(mask & bit).is_zero() {
        // This batch was already applied. Redelivery is idempotent.
        return Ok(());
    }

    for bid in &bids {
        contract.append_bid(emission_day, src_chain_id, bid)?;
    }
    contract.chain_arrived_mask.write(&chain_key, mask | bit)?;
    contract.emit(IDesis::IntexAuctionBidsCollected {
        emissionDay: emission_day.into(),
        srcChainId: src_chain_id,
        pageIndex: u32::from(page_index),
        totalPages: u32::from(total_pages),
        bidsCount: u32::try_from(bids.len()).unwrap_or(u32::MAX),
    })?;

    try_complete_intex_auction_bids(&mut contract, emission_day, src_chain_id)
}

/// Accept a chain's BIDS_DONE completeness marker: the source relayed `total_pages` batches with
/// `total_bids` bids for this day. Stage semantics mirror `collect_intex_auction_bids`. A marker may land
/// before the batches it counts, which finalize the chain as they arrive. A marker the chain already
/// recorded is a no-op when it agrees and is acknowledged with `IntexAuctionBidsIgnored` when it does not: the
/// first marker stands.
pub fn complete_intex_auction_bids(
    storage: StorageHandle<'_>,
    inbound: Inbound,
    total_pages: u16,
    total_bids: u32,
) -> Result<()> {
    let Inbound {
        caller,
        emission_day,
        src_chain_id,
        ..
    } = inbound;
    require_origin_router(caller)?;
    require_nonzero_emission_day(emission_day)?;
    if total_pages == 0 || total_pages > MAX_BID_PAGES {
        return Err(DesisError::InvalidTotalPages.into());
    }
    let mut contract = storage.contract::<DesisContract>();

    if let Some(reason) = ignored_reason(contract.read_stage(emission_day)?)? {
        return emit_intex_auction_bids_ignored(&mut contract, inbound, reason);
    }

    let chain_key = DesisContract::chain_key(emission_day, src_chain_id);
    let recorded_batches = contract.completed_pages.read(&chain_key)?;
    contract.track_intake_chain(emission_day, src_chain_id)?;
    if recorded_batches != 0 {
        let same = recorded_batches == u32::from(total_pages)
            && contract.completed_bids.read(&chain_key)? == total_bids;
        if same {
            return Ok(());
        }
        return emit_intex_auction_bids_ignored(&mut contract, inbound, IGNORED_CONFLICT);
    }

    contract
        .completed_pages
        .write(&chain_key, u32::from(total_pages))?;
    contract.completed_bids.write(&chain_key, total_bids)?;

    try_complete_intex_auction_bids(&mut contract, emission_day, src_chain_id)
}

fn check_page_shape(page_index: u16, total_pages: u16, bids: &[IntexAuctionBidData]) -> Result<()> {
    // The arrival bitmap is a U256, so at most 256 batches (page_index 0..=255) are trackable.
    if total_pages == 0 || total_pages > MAX_BID_PAGES || page_index >= total_pages {
        return Err(DesisError::InvalidPageIndexOrTotal.into());
    }
    // Same reason as the currency check below: an over-wide batch is admissible
    // here but not at clearing, where the refund fan-out would reject the day.
    if bids.len() > MAX_PAYLOAD_ARRAY_LEN {
        return Err(
            DesisError::IntexAuctionBidsPageTooLarge(bids.len(), MAX_PAYLOAD_ARRAY_LEN).into(),
        );
    }
    // Checked here because clearing cannot recover from it: an unspellable code would
    // otherwise surface as a day whose clearing reverts every block.
    if let Some(bad) = bids.iter().find(|bid| {
        SeriesId::currency_code(bid.issuance_currency).is_err()
            || SeriesId::currency_code(bid.reference_currency).is_err()
    }) {
        return Err(DesisError::UnspellableIntexAuctionBidCurrency(
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
fn try_complete_intex_auction_bids(
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    chain_id: u32,
) -> Result<()> {
    let key = DesisContract::chain_key(emission_day, chain_id);
    if contract.bids_completed.read(&key)? != 0 {
        return Ok(());
    }
    let claimed_batches = contract.completed_pages.read(&key)?;
    if claimed_batches == 0 {
        return Ok(()); // no marker yet
    }
    let total = contract.chain_total_pages.read(&key)?;
    let mask = contract.chain_arrived_mask.read(&key)?;
    let bid_count = contract.chain_bid_count.read(&key)?;
    if total != claimed_batches
        || mask.count_ones() as u32 != total
        || bid_count != contract.completed_bids.read(&key)?
    {
        return Ok(());
    }
    contract.bids_completed.write(&key, 1u8)?;
    contract.emit(IDesis::IntexAuctionBidsCompleted {
        emissionDay: emission_day.into(),
        srcChainId: chain_id,
        totalPages: total,
        totalBids: bid_count,
    })
}

fn require_origin_router(caller: Address) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(DesisError::UnauthorizedOrigin(caller).into());
    }
    Ok(())
}

fn require_nonzero_emission_day(emission_day: EmissionDate) -> Result<()> {
    if emission_day.value() == 0 {
        return Err(DesisError::InvalidEmissionDay(emission_day).into());
    }
    Ok(())
}
