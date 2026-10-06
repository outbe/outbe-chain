use super::*;
use outbe_intex::CertifiedContributorGenerationProjection;

/// Credit auction proceeds (native COEN, arriving as `amount` = msg.value) from
/// one target chain into the day's pot. Gated to the OriginRouter. The day's
/// payout round opens once every winning chain has routed its proceeds (or the
/// fan-in deadline passes). `payContributorBatch` pays it out. Proceeds arrive
/// once per winning chain (loopback same-block, remote minutes later). So the
/// credit only accumulates. It never reverts on a repeat or ownerless day: a
/// revert there would strand that chain's delivery.
pub fn distribute(
    storage: &StorageHandle<'_>,
    caller: Address,
    worldwide_day: WorldwideDay,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    #[cfg(not(feature = "e2e-test"))]
    let from_router = caller == ORIGIN_ROUTER_ADDRESS;
    #[cfg(feature = "e2e-test")]
    let from_router =
        caller == ORIGIN_ROUTER_ADDRESS || caller == crate::constants::PROCEEDS_TEST_SENDER;
    if !from_router {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    outbe_intex::api::credit_proceeds(storage, worldwide_day, src_chain_id, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsCredited {
            worldwideDay: worldwide_day.value(),
            srcChainId: src_chain_id,
            amount,
        },
    )?;
    let now = storage.timestamp()?.to::<u64>();
    try_settle_proceeds(storage, worldwide_day, now)
}

/// Open the payout round for a series if its proceeds fan-in is satisfied
/// (all winning chains in) or its deadline has passed. Idempotent, so repeated
/// arrivals and the begin-block sweep can both call it safely.
pub(crate) fn try_settle_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    now: u64,
) -> Result<()> {
    // Batches drain a certified round, not this sweep, and a day gets exactly
    // one. Anything that arrives after it opened missed the window.
    if outbe_intex::api::certified_payout_round(storage, worldwide_day.value())?.is_some() {
        return burn_late_pot(storage, worldwide_day);
    }
    let deadline = outbe_intex::api::proceeds_deadline(storage, worldwide_day)?;
    if deadline == 0 {
        // Never armed - or already finalized. A certified day past finalization
        // (an ownerless one never opens a round) treats any re-delivery as late.
        if outbe_intex::api::certified_contributor_generation(storage, worldwide_day)?.is_some() {
            burn_late_pot(storage, worldwide_day)?;
        }
        return Ok(());
    }
    let complete = outbe_intex::api::proceeds_ready(storage, worldwide_day)?;
    if !complete && now < deadline {
        return Ok(()); // keep waiting for the remaining chains
    }

    let certified = outbe_intex::api::certified_contributor_generation(storage, worldwide_day)?;
    // Proceeds can complete before quorum installs the root, so hold the pot
    // until the window closes rather than burn a day about to become payable.
    if certified.is_none() && now < deadline {
        return Ok(());
    }
    settle_pot(storage, worldwide_day, certified, complete)
}

fn burn_late_pot(storage: &StorageHandle<'_>, worldwide_day: WorldwideDay) -> Result<()> {
    let late = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if !late.is_zero() {
        burn_late_proceeds(storage, worldwide_day.value(), late)?;
    }
    Ok(())
}

fn settle_pot(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    certified: Option<CertifiedContributorGenerationProjection>,
    complete: bool,
) -> Result<()> {
    let pot = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if pot.is_zero() {
        // Nothing to pay, and nothing left to wait for: an incomplete fan-in only
        // reaches here past its deadline. Finalize either way, so a day no chain
        // ever paid into leaves the awaiting set instead of being re-swept forever.
        // The branches above still catch and burn a later arrival.
        return outbe_intex::api::finalize_proceeds(storage, worldwide_day);
    }

    if let Some(generation) = certified {
        if generation.contributor_count == 0 {
            burn_ownerless_proceeds(storage, worldwide_day, pot)?;
        } else {
            outbe_intex::api::open_certified_payout_round(storage, worldwide_day.value(), pot)?;
            emit_event(
                storage,
                crate::precompile::IIntexFactory::ContributorPayoutOpened {
                    worldwideDay: worldwide_day.value(),
                    amount: pot,
                    contributorCount: generation.contributor_count,
                },
            )?;
        }
        // Unconditional: batches drain the round, so staying in the awaiting set
        // would re-enter this sweep every block.
        return outbe_intex::api::finalize_proceeds(storage, worldwide_day);
    }

    // Ownerless proceeds: burn instead of stranding them.
    burn_ownerless_proceeds(storage, worldwide_day, pot)?;
    if complete {
        outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
    }
    Ok(())
}

/// Pay one chunk-aligned range of certified contributors.
///
/// Permissionless: a batch is accepted only if its records rebuild the day's
/// certified contributor root. Shares divide the amount frozen at round open,
/// never the live balance.
pub(crate) fn pay_contributor_batch(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    start_index: u32,
    leaves: &[crate::precompile::IIntexFactory::ContributorLeaf],
    proof: &[B256],
) -> Result<()> {
    let round = outbe_intex::api::certified_payout_round(storage, worldwide_day)?
        .ok_or(IntexFactoryError::NoCertifiedRound(worldwide_day))?;
    let leaf_count =
        u32::try_from(leaves.len()).map_err(|_| IntexFactoryError::BadContributorBatch)?;

    // Cheapest gate first: a batch another sender already paid costs one
    // storage read instead of a full proof verification.
    outbe_intex::api::require_certified_leaves_unpaid(
        storage,
        worldwide_day,
        start_index,
        leaf_count,
    )?;

    let decoded: Vec<ContributorLeafData> = leaves.iter().map(decode_contributor_leaf).collect();
    let generation = outbe_intex::api::verify_certified_contributor_batch(
        storage,
        worldwide_day,
        start_index,
        &decoded,
        proof,
    )?;
    storage.with_checkpoint(|| {
        let mut shares = Vec::with_capacity(decoded.len());
        let mut paid = U256::ZERO;
        for leaf in &decoded {
            // Floor for every leaf: batches arrive in any order, so none can be
            // the one that absorbs the remainder.
            let share = round
                .amount
                .checked_mul(leaf.nominal)
                .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?
                / generation.eligible_nominal_total;
            paid = paid
                .checked_add(share)
                .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?;
            shares.push(share);
        }
        // One balance serves every day. Bounding before any transfer keeps a
        // bad denominator from spending another day's proceeds and from
        // draining into an insufficient-balance Fatal mid-batch.
        let total_paid = round
            .paid_so_far
            .checked_add(paid)
            .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?;
        if total_paid > round.amount {
            return Err(IntexFactoryError::PayoutExceedsRound(worldwide_day).into());
        }
        for (leaf, share) in decoded.iter().zip(&shares) {
            storage.transfer_balance(INTEX_FACTORY_ADDRESS, leaf.owner, *share)?;
        }
        outbe_intex::api::mark_certified_leaves_paid(
            storage,
            worldwide_day,
            start_index,
            leaf_count,
            paid,
        )?;
        emit_event(
            storage,
            crate::precompile::IIntexFactory::ContributorBatchPaid {
                worldwideDay: worldwide_day,
                startIndex: start_index,
                leafCount: leaf_count,
                paidAmount: paid,
            },
        )?;
        close_round_if_complete(storage, worldwide_day, generation.contributor_count)
    })
}

/// Burns what floor division left behind, once every leaf of the round is paid.
///
/// Completion is what gates this: until the last leaf is paid the balance still
/// holds outstanding shares, which are owed rather than left over.
fn close_round_if_complete(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    contributor_count: u32,
) -> Result<()> {
    let round = outbe_intex::api::certified_payout_round(storage, worldwide_day)?
        .ok_or(IntexFactoryError::NoCertifiedRound(worldwide_day))?;
    if round.paid_leaf_count != contributor_count {
        return Ok(());
    }
    // The per-batch cap keeps the sum within `amount`. A shortfall here means
    // the round accounting is corrupt.
    let remainder = round.amount.checked_sub(round.paid_so_far).ok_or_else(|| {
        PrecompileError::Fatal("certified payout exceeded the round amount".into())
    })?;
    if !remainder.is_zero() {
        storage.decrease_balance(INTEX_FACTORY_ADDRESS, remainder)?;
    }
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ContributorRoundClosed {
            worldwideDay: worldwide_day,
            paidAmount: round.paid_so_far,
            burnedAmount: remainder,
        },
    )
}

/// Destroy proceeds that arrived after the day's payout round had already opened.
fn burn_late_proceeds(storage: &StorageHandle<'_>, worldwide_day: u32, amount: U256) -> Result<()> {
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::LateProceedsBurned {
            worldwideDay: worldwide_day,
            amount,
        },
    )
}

/// Progress of one day's payout round. It is all-zero when no round is open.
pub(crate) fn contributor_payout_round(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
) -> Result<crate::precompile::IIntexFactory::ContributorRound> {
    let Some(round) = outbe_intex::api::certified_payout_round(storage, worldwide_day)? else {
        return Ok(crate::precompile::IIntexFactory::ContributorRound {
            amount: U256::ZERO,
            contributorCount: 0,
            paidSoFar: U256::ZERO,
            paidLeafCount: 0,
        });
    };
    let contributor_count = outbe_intex::api::certified_contributor_generation(
        storage,
        outbe_primitives::time::WorldwideDay::new(worldwide_day),
    )?
    .map_or(0, |generation| generation.contributor_count);
    Ok(crate::precompile::IIntexFactory::ContributorRound {
        amount: round.amount,
        contributorCount: contributor_count,
        paidSoFar: round.paid_so_far,
        paidLeafCount: round.paid_leaf_count,
    })
}

/// The ABI leaf and the canonical leaf carry the same three words, so this is a
/// plain field copy.
fn decode_contributor_leaf(
    leaf: &crate::precompile::IIntexFactory::ContributorLeaf,
) -> ContributorLeafData {
    ContributorLeafData {
        owner: leaf.owner,
        source_tribute_id: leaf.sourceTributeId,
        nominal: leaf.nominal,
    }
}

/// Begin-block sweep: settle every series whose proceeds fan-in deadline has
/// passed. The set holds one entry per day and releases it once its deadline is
/// out, so a whole pass is a handful of reads. Each series runs in its own
/// checkpoint so one failure is retried next block instead of halting the block.
pub(crate) fn sweep_proceeds_deadlines(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = outbe_intex::api::awaiting_proceeds_count(storage)?;
    // Read the set before settling: settling swap-removes from it.
    let mut worldwide_days = Vec::with_capacity(count as usize);
    for i in 0..count {
        worldwide_days.push(outbe_intex::api::awaiting_proceeds_at(storage, i)?);
    }
    for worldwide_day in worldwide_days {
        let res = storage.with_checkpoint(|| try_settle_proceeds(storage, worldwide_day, now));
        if let Err(e) = res {
            if e.sweep_failure() == SweepFailure::Propagate {
                return Err(e);
            }
            tracing::warn!(target: "outbe::intexfactory", worldwide_day = worldwide_day.value(), error = ?e, "proceeds sweep: skipping series");
        }
    }
    Ok(())
}

/// Burn the ownerless proceeds of a series with no recorded contributors:
/// destroy the native COEN held by the factory, reducing total supply.
fn burn_ownerless_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: WorldwideDay,
    amount: U256,
) -> Result<()> {
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsBurned {
            worldwideDay: worldwide_day.value(),
            amount,
        },
    )
}
