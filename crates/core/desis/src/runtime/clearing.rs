use super::algorithm::{compute_clearing, sort_bids};
use super::*;

/// Tick entry for the fan-in gate: clear once every snapshot chain has finalized,
/// or once the deadline passes (missing chains are excluded and reported via
/// `BidsExpired`). Returns `None` while the gate is not ready, and past the day's
/// last clearing time, which leaves the day to fail.
pub fn force_clear(
    storage: StorageHandle<'_>,
    emission_day: EmissionDate,
    now: u64,
) -> Result<Option<ClearingResult>> {
    if now >= super::schedule::last_clearing(&storage.contract::<DesisContract>(), emission_day)? {
        return Ok(None);
    }
    let snapshot = fetch_targets(&storage, emission_day)?;
    let (included, skipped) = {
        let contract = storage.contract::<DesisContract>();
        let parts = partition_chains(&contract, emission_day, &snapshot)?;
        if !parts.1.is_empty() && now < contract.clearing_deadline.read(&emission_day)? {
            return Ok(None);
        }
        parts
    };
    clear_inner(storage, emission_day, &snapshot, &included, &skipped).map(Some)
}

/// Poll one fan-in gate per invocation, rotating through the active set. A day
/// runs in its own checkpoint; ordinary reverts do not block other days.
pub fn complete_auctions(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.with_checkpoint(|| poll_gate(ctx))
}

fn poll_gate(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let count = {
        let contract = storage.contract::<DesisContract>();
        contract.gate_active_count.read()?
    };
    if count == 0 {
        return Ok(());
    }
    let contract = storage.contract::<DesisContract>();
    let index = contract.gate_poll_cursor.read()? % count;
    let day = contract.gate_active_at.read(&index)?.into();
    let res = storage.with_checkpoint(|| force_clear(storage.clone(), day, ctx.block.timestamp));
    match res {
        Ok(_) => {}
        Err(
            error @ (outbe_primitives::error::PrecompileError::Revert(_)
            | outbe_primitives::error::PrecompileError::RevertBytes(_)),
        ) => {
            tracing::warn!(target: "outbe::desis", %day, error = ?error, "clearing gate: skipping day");
        }
        Err(error) => return Err(error),
    }
    let remaining = contract.gate_active_count.read()?;
    // Removal swap-pops a new day into this slot. Otherwise move past the day
    // just polled, including one whose fan-in is not ready yet.
    let next = if remaining == 0 {
        0
    } else if contract.gate_active_slot.read(&day)? == 0 {
        index % remaining
    } else {
        (index + 1) % remaining
    };
    contract.gate_poll_cursor.write(next)?;
    Ok(())
}

/// Gas one COLLECTING_BIDS round asks of `chain_id`, from the busiest of its last
/// [`CLEARING_HISTORY_DAYS`] days and never below [`CLEARING_MIN_BIDS`] bids' worth.
pub(crate) fn clearing_run_gas(
    storage: &StorageHandle<'_>,
    emission_day: EmissionDate,
    chain_id: u32,
) -> Result<u64> {
    let contract = storage.contract::<DesisContract>();
    let mut bids = CLEARING_MIN_BIDS;
    let mut day = emission_day.value();
    for _ in 0..CLEARING_HISTORY_DAYS {
        day = match outbe_system_common::time::previous_date_key(day) {
            Ok(previous) => previous,
            Err(outbe_system_common::time::TimeError::DateBeforeMinimum { .. }) => break,
            Err(err) => return Err(crate::errors::date_step_error(err)),
        };
        let history_day = EmissionDate::new(day);
        // Terminal intake is logically empty even while its physical rows await
        // collection. Cleanup progress must not change the next auction's quote.
        if matches!(
            contract.read_stage(history_day)?,
            AuctionStage::Completed | AuctionStage::Canceled | AuctionStage::Failed
        ) {
            continue;
        }
        let key = DesisContract::chain_key(history_day, chain_id);
        bids = bids.max(u64::from(contract.chain_bid_count.read(&key)?));
    }
    let pages = bids.div_ceil(MAX_PAYLOAD_ARRAY_LEN as u64).max(1);
    let cost = CLEARING_FIXED_GAS
        .saturating_add(pages.saturating_mul(CLEARING_PAGE_GAS))
        .saturating_add(bids.saturating_mul(CLEARING_BID_GAS));
    Ok(cost.saturating_mul(3) / 2)
}

/// The day's frozen target snapshot, read from the OriginRouter registry
/// (deterministic: frozen at STAGE_START).
pub(super) fn fetch_targets(
    storage: &StorageHandle<'_>,
    emission_day: EmissionDate,
) -> Result<Vec<u32>> {
    let ret = storage.staticcall(
        ORIGIN_ROUTER_ADDRESS,
        IOriginRouter::targetsOfCall {
            emissionDay: emission_day.into(),
        }
        .abi_encode()
        .into(),
    )?;
    IOriginRouter::targetsOfCall::abi_decode_returns(&ret)
        .map_err(|_| DesisError::TargetsOfUndecodable.into())
}

/// Split the snapshot into chains whose intake finalized and chains still missing.
fn partition_chains(
    contract: &DesisContract<'_>,
    emission_day: EmissionDate,
    snapshot: &[u32],
) -> Result<(Vec<u32>, Vec<u32>)> {
    let mut included = Vec::with_capacity(snapshot.len());
    let mut skipped = Vec::new();
    for &chain_id in snapshot {
        if contract
            .bids_completed
            .read(&DesisContract::chain_key(emission_day, chain_id))?
            != 0
        {
            included.push(chain_id);
        } else {
            skipped.push(chain_id);
        }
    }
    Ok((included, skipped))
}

/// Clear the day:
/// - Run the clearing algorithm over the included chains' bids.
/// - Transition to `Completed`.
/// - Hand issuance to IntexFactory.
/// - Return the unused limit to PromisLimit.
/// - Send the per-chain AUCTION_RESULT / REFUND_INSTRUCTIONS messages.
fn clear_inner(
    storage: StorageHandle<'_>,
    emission_day: EmissionDate,
    snapshot: &[u32],
    included: &[u32],
    skipped: &[u32],
) -> Result<ClearingResult> {
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, emission_day, AuctionStage::CollectingBids)?;

    let desis_limit_units = contract.pending_desis_limit_units.read(&emission_day)?;
    if contract.clearing_initiated.read(&emission_day)? == 0 {
        return Err(DesisError::PendingClearingDataMissing(emission_day).into());
    }

    let config = contract.read_auction_config(emission_day)?;
    let min_bid_qty = contract.config_min_bid_quantity.read(&emission_day)? as u16;
    // Zero bids are valid here. `compute_clearing` yields 0 issued, and the full limit returns
    // to PromisLimit. A no-sale AuctionResult(0,0,0) is reported to every snapshot chain.
    let bids = contract.read_chains_bids(emission_day, included)?;

    let total_demand: u64 = bids.iter().map(|(_, b)| u64::from(b.intex_quantity)).sum();
    let mut sorted = bids;
    sort_bids(&mut sorted);

    let result = compute_clearing(&sorted, &config, desis_limit_units, min_bid_qty);

    // Persist clearing outcome and transition.
    contract.write_stage(emission_day, AuctionStage::Completed)?;
    contract.write_last_cleared_emission_day(emission_day)?;
    contract.write_last_clearing_issued_count(result.issued_units)?;

    // Retire the economic state before external calls. Bid deletion is a
    // separate bounded job: valid intake must not make completion unaffordable.
    let desis_limit_minor = contract.pending_desis_limit_minor.read(&emission_day)?;
    contract.day_bid_count.write(&emission_day, 0)?;
    contract.pending_desis_limit_units.write(&emission_day, 0)?;
    contract
        .pending_desis_limit_minor
        .write(&emission_day, U256::ZERO)?;
    contract.clearing_initiated.write(&emission_day, 0u8)?;
    contract.clearing_deadline.clear(&emission_day)?;
    contract.remove_gate_active(emission_day)?;
    contract.queue_intake_cleanup(emission_day)?;

    for &chain_id in skipped {
        contract.emit(IDesis::IntexAuctionBidsExpired {
            emissionDay: emission_day.into(),
            srcChainId: chain_id,
        })?;
    }

    contract.emit(IDesis::DesisCompleted {
        emissionDay: emission_day.into(),
        quantity: result.issued_units,
        clearingRate: u64::from(result.clearing_rate),
        totalDemand: total_demand,
    })?;

    // Return the Unused Desis Limit (unsold whole units + conversion dust) to PromisLimit.
    let desis_allocation_minor =
        U256::from(result.issued_units as u128) * U256::from(config.promis_load_minor);
    let unused_desis_limit_minor = desis_limit_minor
        .checked_sub(desis_allocation_minor)
        .ok_or(DesisError::DesisAllocationExceedsLimit {
            day: emission_day,
            allocation: desis_allocation_minor,
            limit: desis_limit_minor,
        })?;
    contract.emit(IDesis::DesisLimitAllocated {
        emissionDay: emission_day.into(),
        amountMinor: desis_allocation_minor,
    })?;
    if !unused_desis_limit_minor.is_zero() {
        PromisLimitContract::new(storage.clone()).add(
            outbe_promislimit::LimitSource::DesisLimit,
            unused_desis_limit_minor,
        )?;
    }

    if result.issued_units != 0 {
        let mut legs = Vec::new();
        for group in issuance_groups(&result, &config, emission_day, snapshot)? {
            legs.extend(outbe_intexfactory::api::issue_intex(&storage, group)?);
        }

        outbe_intexfactory::api::send_issuance(&storage, legs)?;
    }

    // Send AUCTION_RESULT to every snapshot chain; skipped/zero-winner chains get
    // wonBidsCount 0 so their local auction still completes.
    for &chain_id in snapshot {
        let (won_bids_count, won_quantity) = chain_winnings(&result, chain_id);
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendIntexAuctionResultCall {
                dstChainId: chain_id,
                emissionDay: emission_day.into(),
                wonQuantity: won_quantity,
                wonBidsCount: won_bids_count,
                quantity: result.issued_units,
                clearingRate: u64::from(result.clearing_rate),
                totalDemand: total_demand,
            }
            .abi_encode()
            .into(),
        )?;
    }

    // A skipped chain's bidders reclaim through the escrow timeout path instead.
    for &chain_id in included {
        if !result.bidder_chains.contains(&chain_id) {
            continue;
        }
        let pages = intex_auction_winners_pages(&result, chain_id)?;
        let total_pages = pages.len() as u16;
        for (page_index, page) in pages.into_iter().enumerate() {
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendIntexAuctionWinnersCall {
                    dstChainId: chain_id,
                    emissionDay: emission_day.into(),
                    pageIndex: page_index as u16,
                    totalPages: total_pages,
                    clearingRate: u64::from(result.clearing_rate),
                    basis: config.escrow_basis_minor(),
                    winners: page.winners,
                    partialIndex: page.partial_index,
                    partialWon: page.partial_won,
                }
                .abi_encode()
                .into(),
            )?;
        }
    }

    Ok(result)
}

/// How many REFUND_INSTRUCTIONS messages one chain's winners take: at least one, which
/// closes the day on a chain whose bids all lost. Bounded by the codec's arrival set,
/// which is one 256-bit word wide.
pub(crate) fn intex_auction_winners_page_count(winners: usize) -> Result<usize> {
    let pages = winners.div_ceil(MAX_PAYLOAD_ARRAY_LEN).max(1);
    if pages > MAX_INTEX_AUCTION_WINNERS_PAGES {
        return Err(DesisError::ReturnFanOutTooLarge(winners).into());
    }
    Ok(pages)
}

/// One REFUND_INSTRUCTIONS message: a run of a chain's winners and the partial fill among them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct IntexAuctionWinnersPage {
    pub(crate) winners: Vec<Address>,
    pub(crate) partial_index: u16,
    /// Units the partially filled winner received; 0 when the page has none.
    pub(crate) partial_won: u16,
}

/// A chain's winners in ranking order, cut into REFUND_INSTRUCTIONS messages.
pub(crate) fn intex_auction_winners_pages(
    result: &ClearingResult,
    chain_id: u32,
) -> Result<Vec<IntexAuctionWinnersPage>> {
    let mut winners = Vec::new();
    let mut partial = None;
    for (j, &winner_chain) in result.winner_chains.iter().enumerate() {
        if winner_chain != chain_id {
            continue;
        }
        if result.partial_winner == Some(j) {
            partial = Some((
                winners.len(),
                result.winner_quantities[j].saturating_to::<u16>(),
            ));
        }
        winners.push(result.winners[j]);
    }

    let total_pages = intex_auction_winners_page_count(winners.len())?;
    Ok((0..total_pages)
        .map(|page_index| {
            let start = page_index * MAX_PAYLOAD_ARRAY_LEN;
            let end = (start + MAX_PAYLOAD_ARRAY_LEN).min(winners.len());
            let (partial_index, partial_won) = match partial {
                Some((at, won)) if (start..end).contains(&at) => ((at - start) as u16, won),
                _ => (0, 0),
            };
            IntexAuctionWinnersPage {
                winners: winners[start..end].to_vec(),
                partial_index,
                partial_won,
            }
        })
        .collect())
}

/// One issuance per distinct winning `(issuance, reference)` pair, in the order
/// the pairs first appear in the ranking. Each group carries every condition the
/// day fixed at its auction start: the Promis load, the call trigger and its own
/// reference currency's entry and call price.
pub(crate) fn issuance_groups(
    result: &ClearingResult,
    config: &AuctionConfig,
    emission_day: EmissionDate,
    snapshot: &[u32],
) -> Result<Vec<IssuanceParams>> {
    let mut groups: Vec<IssuanceParams> = Vec::new();
    for (i, &(issuance_currency, reference_currency)) in result.winner_currencies.iter().enumerate()
    {
        let at = match groups.iter().position(|g| {
            (g.issuance_currency, g.reference_currency) == (issuance_currency, reference_currency)
        }) {
            Some(at) => at,
            None => {
                // Reveal only accepts a reference the day priced, so a winner
                // without a row means the day's table and its bids disagree.
                let row = config
                    .price_for(reference_currency)
                    .ok_or(DesisError::UnpricedReferenceCurrency(reference_currency))?;
                groups.push(IssuanceParams {
                    series_id: SeriesId::for_pair(
                        emission_day.value(),
                        issuance_currency,
                        reference_currency,
                    )
                    .map_err(outbe_intex::IntexError::from)?,
                    emission_day,
                    issued_units: 0,
                    promis_load_minor: config.promis_load_minor,
                    entry_price_minor: row.entry_price_minor,
                    call_price_minor: row.call_price_minor,
                    call_trigger: config.call_trigger,
                    issuance_currency,
                    reference_currency,
                    snapshot_chains: snapshot.to_vec(),
                });
                groups.len() - 1
            }
        };

        groups[at].issued_units += result.winner_quantities[i].saturating_to::<u32>();
    }
    Ok(groups)
}

fn require_stage(
    contract: &DesisContract<'_>,
    emission_day: EmissionDate,
    expected: AuctionStage,
) -> Result<()> {
    let actual = contract.read_stage(emission_day)?;
    if actual != expected {
        return Err(DesisError::InvalidStageTransition.into());
    }
    Ok(())
}

/// The winning bids of `chain_id` and the Intex quantity they won.
fn chain_winnings(result: &ClearingResult, chain_id: u32) -> (u32, u32) {
    result
        .winner_chains
        .iter()
        .zip(&result.winner_quantities)
        .filter(|(chain, _)| **chain == chain_id)
        .fold((0u32, 0u32), |(bids, quantity), (_, won)| {
            let won = u32::try_from(*won).unwrap_or(u32::MAX);
            (bids.saturating_add(1), quantity.saturating_add(won))
        })
}
