use super::algorithm::{calculate_clearing, sort_bids};
use super::*;

/// Tick entry for the fan-in gate: clear once every snapshot chain has finalized,
/// or once the deadline passes (missing chains are excluded and reported via
/// `ChainSkipped`). Returns `None` while the gate is not ready.
pub fn force_clear(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    now: u64,
) -> Result<Option<ClearingResult>> {
    let snapshot = fetch_targets(&storage, worldwide_day)?;
    let (included, skipped) = {
        let contract = storage.contract::<DesisContract>();
        let parts = partition_chains(&contract, worldwide_day, &snapshot)?;
        if !parts.1.is_empty() && now < contract.clearing_deadline.read(&worldwide_day)? {
            return Ok(None);
        }
        parts
    };
    clear_inner(storage, worldwide_day, &snapshot, &included, &skipped).map(Some)
}

/// Cycle `auction_clearing` trigger: attempt to clear every day awaiting the
/// fan-in gate. Each day runs in its own checkpoint. An Err reverts that day
/// (retried next slot) and never escapes into the trigger chain.
pub fn tick_gate(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let count = {
        let contract = storage.contract::<DesisContract>();
        contract.gate_active_count.read()?
    };
    if count == 0 {
        return Ok(());
    }
    let now = ctx.block.timestamp;
    // Snapshot the set before iterating: a successful clear swap-pops it.
    let mut days = Vec::with_capacity(count as usize);
    {
        let contract = storage.contract::<DesisContract>();
        for i in 0..count {
            days.push(contract.gate_active_at.read(&i)?.into());
        }
    }
    for day in days {
        let res = storage.with_checkpoint(|| force_clear(storage.clone(), day, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::desis", %day, error = ?e, "clearing gate: skipping day");
        }
    }
    Ok(())
}

/// Gas one CLEARING round asks of `chain_id`, from the busiest of its last
/// [`CLEARING_HISTORY_DAYS`] days and never below [`CLEARING_MIN_BIDS`] bids' worth.
pub(crate) fn clearing_round_gas(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    chain_id: u32,
) -> Result<u64> {
    let contract = storage.contract::<DesisContract>();
    let mut bids = CLEARING_MIN_BIDS;
    let mut day = worldwide_day.value();
    for _ in 0..CLEARING_HISTORY_DAYS {
        day = outbe_primitives::time::previous_date_key(day);
        let key = DesisContract::chain_key(WorldwideDay::new(day), chain_id);
        bids = bids.max(u64::from(contract.chain_bid_count.read(&key)?));
    }
    let chunks = bids.div_ceil(CLEARING_BIDS_PER_CHUNK).max(1);
    let cost = CLEARING_FIXED_GAS
        .saturating_add(chunks.saturating_mul(CLEARING_CHUNK_GAS))
        .saturating_add(bids.saturating_mul(CLEARING_BID_GAS));
    Ok(cost.saturating_mul(3) / 2)
}

/// The day's frozen target snapshot, read from the OriginRouter registry
/// (deterministic: frozen at STAGE_START).
pub(super) fn fetch_targets(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
) -> Result<Vec<u32>> {
    let ret = storage.staticcall(
        ORIGIN_ROUTER_ADDRESS,
        IOriginRouter::targetsOfCall {
            worldwideDay: worldwide_day.into(),
        }
        .abi_encode()
        .into(),
    )?;
    IOriginRouter::targetsOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("targetsOf undecodable".into()))
}

/// Split the snapshot into chains whose intake finalized and chains still missing.
fn partition_chains(
    contract: &DesisContract<'_>,
    worldwide_day: WorldwideDay,
    snapshot: &[u32],
) -> Result<(Vec<u32>, Vec<u32>)> {
    let mut included = Vec::with_capacity(snapshot.len());
    let mut skipped = Vec::new();
    for &chain_id in snapshot {
        if contract
            .chain_done
            .read(&DesisContract::chain_key(worldwide_day, chain_id))?
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
/// - Transition to `Cleared`.
/// - Hand issuance to IntexFactory.
/// - Return the unused limit to PromisLimit.
/// - Send the per-chain AUCTION_RESULT / REFUND_INSTRUCTIONS messages.
fn clear_inner(
    storage: StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    snapshot: &[u32],
    included: &[u32],
    skipped: &[u32],
) -> Result<ClearingResult> {
    let mut contract = storage.contract::<DesisContract>();
    require_stage(&contract, worldwide_day, AuctionStage::Clearing)?;

    let desis_limit_units = contract.pending_desis_limit_units.read(&worldwide_day)?;
    if contract.clearing_initiated.read(&worldwide_day)? == 0 {
        return Err(DesisError::PendingClearingDataMissing(worldwide_day).into());
    }

    let config = contract.read_auction_config(worldwide_day)?;
    let min_bid_qty = contract.config_min_bid_quantity.read(&worldwide_day)? as u16;
    // Zero bids are valid here. `calculate_clearing` yields 0 issued, and the full limit returns
    // to PromisLimit. A no-sale AuctionResult(0,0,0) is reported to every snapshot chain.
    let bids = contract.read_chains_bids(worldwide_day, included)?;

    let total_demand: u64 = bids.iter().map(|(_, b)| u64::from(b.intex_quantity)).sum();
    let mut sorted = bids;
    sort_bids(&mut sorted);

    let result = calculate_clearing(&sorted, &config, desis_limit_units, min_bid_qty);

    // Persist clearing outcome and transition.
    contract.write_stage(worldwide_day, AuctionStage::Cleared)?;
    contract.write_last_cleared_worldwide_day(worldwide_day)?;
    contract.write_last_clearing_issued_count(result.issued_units)?;

    // Clear the bid working-set, pending inputs and the gate (CEI: state writes before external calls).
    let desis_limit_minor = contract.pending_desis_limit_minor.read(&worldwide_day)?;
    for &chain_id in snapshot {
        contract.reset_chain_intake(worldwide_day, chain_id)?;
    }
    contract.day_bid_count.write(&worldwide_day, 0)?;
    contract
        .pending_desis_limit_units
        .write(&worldwide_day, 0)?;
    contract
        .pending_desis_limit_minor
        .write(&worldwide_day, U256::ZERO)?;
    contract.clearing_initiated.write(&worldwide_day, 0u8)?;
    contract.clearing_deadline.clear(&worldwide_day)?;
    contract.remove_gate_active(worldwide_day)?;

    for &chain_id in skipped {
        contract.emit(IDesis::ChainSkipped {
            worldwideDay: worldwide_day.into(),
            srcChainId: chain_id,
        })?;
    }

    if result.issued_units == 0 {
        contract.emit(IDesis::AuctionClearedEmpty {
            worldwideDay: worldwide_day.into(),
            totalDemand: total_demand,
        })?;
    } else {
        contract.emit(IDesis::AuctionCleared {
            worldwideDay: worldwide_day.into(),
            issuedUnits: result.issued_units,
            clearingRate: result.clearing_rate,
            totalDemand: total_demand,
        })?;
    }

    // Return the Unused Desis Limit (unsold whole units + conversion dust) to PromisLimit.
    let desis_allocation_minor =
        U256::from(result.issued_units as u128) * U256::from(config.promis_load_minor);
    let unused_desis_limit_minor = desis_limit_minor
        .checked_sub(desis_allocation_minor)
        .ok_or(DesisError::DesisAllocationExceedsLimit {
            wwd: worldwide_day,
            allocation: desis_allocation_minor,
            limit: desis_limit_minor,
        })?;
    contract.emit(IDesis::DesisAllocationRecorded {
        worldwideDay: worldwide_day.into(),
        desisLimitMinor: desis_limit_minor,
        desisAllocationMinor: desis_allocation_minor,
    })?;
    if !unused_desis_limit_minor.is_zero() {
        contract.emit(IDesis::UnusedDesisLimitReported {
            worldwideDay: worldwide_day.into(),
            unusedDesisLimitMinor: unused_desis_limit_minor,
        })?;
        PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(unused_desis_limit_minor)?;
    }

    if result.issued_units == 0 {
        // No series anywhere, so no proceeds can arrive for the day.
        outbe_intexfactory::api::discard_day_contributors(&storage, worldwide_day)?;
    } else {
        let mut legs = Vec::new();
        for group in issuance_groups(&result, &config, worldwide_day, snapshot)? {
            legs.extend(outbe_intexfactory::api::issue(&storage, group)?);
        }

        outbe_intexfactory::api::send_issuance(&storage, legs)?;
    }

    // Send AUCTION_RESULT to every snapshot chain; skipped/zero-winner chains get
    // wonBidsCount 0 so their local auction still completes.
    for &chain_id in snapshot {
        let won_bids_count = result
            .winner_chains
            .iter()
            .filter(|&&c| c == chain_id)
            .count() as u32;
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendAuctionResultCall {
                dstChainId: chain_id,
                worldwideDay: worldwide_day.into(),
                issuedUnits: result.issued_units,
                auctionClearingRate: u64::from(result.clearing_rate),
                wonBidsCount: won_bids_count,
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
        let chunks = refund_chunks(&result, chain_id)?;
        let total_chunks = chunks.len() as u16;
        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendRefundInstructionsCall {
                    dstChainId: chain_id,
                    worldwideDay: worldwide_day.into(),
                    chunkIndex: chunk_index as u16,
                    totalChunks: total_chunks,
                    clearingRate: u64::from(result.clearing_rate),
                    basis: config.escrow_basis_minor(),
                    winners: chunk.winners,
                    partialIndex: chunk.partial_index,
                    partialWon: chunk.partial_won,
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
pub(crate) fn refund_chunk_count(winners: usize) -> Result<usize> {
    let chunks = winners.div_ceil(REFUND_CHUNK_LEN).max(1);
    if chunks > MAX_REFUND_CHUNKS {
        return Err(DesisError::RefundFanOutTooLarge(winners).into());
    }
    Ok(chunks)
}

/// One REFUND_INSTRUCTIONS message: a run of a chain's winners and the partial fill among them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RefundChunk {
    pub(crate) winners: Vec<Address>,
    pub(crate) partial_index: u16,
    /// Units the partially filled winner received; 0 when the chunk has none.
    pub(crate) partial_won: u16,
}

/// A chain's winners in ranking order, cut into REFUND_INSTRUCTIONS messages.
pub(crate) fn refund_chunks(result: &ClearingResult, chain_id: u32) -> Result<Vec<RefundChunk>> {
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

    let total_chunks = refund_chunk_count(winners.len())?;
    Ok((0..total_chunks)
        .map(|chunk_index| {
            let start = chunk_index * REFUND_CHUNK_LEN;
            let end = (start + REFUND_CHUNK_LEN).min(winners.len());
            let (partial_index, partial_won) = match partial {
                Some((at, won)) if (start..end).contains(&at) => ((at - start) as u16, won),
                _ => (0, 0),
            };
            RefundChunk {
                winners: winners[start..end].to_vec(),
                partial_index,
                partial_won,
            }
        })
        .collect())
}

/// One issuance per distinct winning `(issuance, reference)` pair, in the order
/// the pairs first appear in the ranking. Each group carries its own reference
/// currency's entry price, which is what floor and call derive from.
fn issuance_groups(
    result: &ClearingResult,
    config: &AuctionConfig,
    worldwide_day: WorldwideDay,
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
                let entry_price_minor = config
                    .entry_price_for(reference_currency)
                    .ok_or(DesisError::UnpricedReferenceCurrency(reference_currency))?;
                groups.push(IssuanceParams {
                    series_id: SeriesId::for_pair(
                        worldwide_day,
                        issuance_currency,
                        reference_currency,
                    )?,
                    worldwide_day,
                    issued_units: 0,
                    promis_load_minor: config.promis_load_minor,
                    entry_price_minor,
                    issuance_currency,
                    reference_currency,
                    recipients: Vec::new(),
                    units: Vec::new(),
                    recipient_chains: Vec::new(),
                    snapshot_chains: snapshot.to_vec(),
                });
                groups.len() - 1
            }
        };

        let units = result.winner_quantities[i];
        let group = &mut groups[at];
        group.issued_units += units.saturating_to::<u32>();
        group.recipients.push(result.winners[i]);
        group.units.push(units);
        group.recipient_chains.push(result.winner_chains[i]);
    }
    Ok(groups)
}

fn require_stage(
    contract: &DesisContract<'_>,
    worldwide_day: WorldwideDay,
    expected: AuctionStage,
) -> Result<()> {
    let actual = contract.read_stage(worldwide_day)?;
    if actual != expected {
        return Err(DesisError::InvalidStageTransition.into());
    }
    Ok(())
}
