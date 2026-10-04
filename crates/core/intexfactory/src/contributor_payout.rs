//! Certified contributor proceeds: fan-in, floor shares, and the leaf-0 remainder.
//!
//! Ownerless days and proceeds that arrive after the round opened still burn.
//! An ordinary round does not. Its floor remainder is paid to the owner of
//! certified eligible leaf 0 once every leaf is paid.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;

use outbe_intex::payout::ContributorLeafData;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result, SweepFailure};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::errors::IntexFactoryError;

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Credit auction proceeds (native COEN, arriving as `amount` = msg.value) from
/// one target chain into the day's pot. Gated to the OriginRouter. The day's
/// payout round opens once every winning chain has routed its proceeds (or the
/// fan-in deadline passes); `payContributorBatch` pays it out. Because proceeds
/// arrive once per winning chain (loopback same-block, remote minutes later),
/// the credit only accumulates - it never reverts on a repeat or ownerless day,
/// which would strand that chain's delivery.
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
    // one - anything arriving after it opened missed the window.
    if outbe_intex::api::certified_payout_round(storage, worldwide_day.value())?.is_some() {
        let late = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
        if !late.is_zero() {
            burn_late_proceeds(storage, worldwide_day.value(), late)?;
        }
        return Ok(());
    }
    let deadline = outbe_intex::api::proceeds_deadline(storage, worldwide_day)?;
    if deadline == 0 {
        // Never armed - or already finalized. A certified day past finalization
        // (an ownerless one never opens a round) treats any re-delivery as late.
        if outbe_intex::api::certified_contributor_generation(storage, worldwide_day)?.is_some() {
            let late = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
            if !late.is_zero() {
                burn_late_proceeds(storage, worldwide_day.value(), late)?;
            }
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

    let pot = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if pot.is_zero() {
        // Nothing to pay, and nothing left to wait for: an incomplete fan-in only
        // reaches here past its deadline. Finalize either way, so a day no chain
        // ever paid into leaves the awaiting set instead of being re-swept forever.
        // A later arrival is still caught, and burned, by the branches above.
        outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
        return Ok(());
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
        outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
        return Ok(());
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
/// never the live balance. The floor remainder is not part of any batch; the
/// completing batch pays it to certified leaf 0.
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
            // Floor for every leaf, including a later batch. None of these shares
            // absorbs the remainder; completion pays that to leaf 0.
            let share = floor_share(
                round.amount,
                leaf.nominal,
                generation.eligible_nominal_total,
                worldwide_day,
            )?;
            paid = paid
                .checked_add(share)
                .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?;
            shares.push(share);
        }
        // One balance serves every day; bounding before any transfer keeps a
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
        // Mark reloads the round, so the recipient is written after it.
        if start_index == 0 {
            let owner = decoded
                .first()
                .ok_or(IntexFactoryError::BadContributorBatch)?
                .owner;
            remember_leaf_zero(storage, worldwide_day, owner)?;
        }
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

/// Record leaf 0's owner from a proof of the index-0 chunk.
///
/// Moves no balance and does not change the paid bitmap. A legacy round whose
/// leaf 0 was paid before this field existed can be completed only after this
/// call. A legacy round that already paid every leaf had its remainder burned;
/// this call rejects that round and does not mint a replacement.
pub(crate) fn record_contributor_residue_recipient(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    leaves: &[crate::precompile::IIntexFactory::ContributorLeaf],
    proof: &[B256],
) -> Result<()> {
    let round = outbe_intex::api::certified_payout_round(storage, worldwide_day)?
        .ok_or(IntexFactoryError::NoCertifiedRound(worldwide_day))?;
    let decoded: Vec<ContributorLeafData> = leaves.iter().map(decode_contributor_leaf).collect();
    let generation = outbe_intex::api::verify_certified_contributor_batch(
        storage,
        worldwide_day,
        0,
        &decoded,
        proof,
    )?;
    let owner = decoded
        .first()
        .ok_or(IntexFactoryError::BadContributorBatch)?
        .owner;
    // Fully paid under the old rule: the remainder is already gone. Paying it
    // again would take another day's pot from this factory balance.
    if round.residue_rule == 0
        && round.residue_recipient_set == 0
        && round.paid_leaf_count == generation.contributor_count
    {
        return Err(IntexFactoryError::LegacyRoundAlreadyClosed(worldwide_day).into());
    }
    if !leaf_zero_paid(storage, worldwide_day)? {
        return Err(IntexFactoryError::LeafZeroUnpaid(worldwide_day).into());
    }
    if round.residue_recipient_set != 0 {
        if round.residue_recipient != owner {
            return Err(IntexFactoryError::ResidueRecipientConflict(worldwide_day).into());
        }
        return Ok(());
    }
    outbe_intex::api::set_certified_residue_recipient(storage, worldwide_day, owner)
}

/// Pays the floor remainder to leaf 0 once every certified leaf is paid.
///
/// Completion is the gate: until the last leaf is paid the balance still holds
/// outstanding shares. A missing recipient fails the checkpoint, which rolls
/// the completing batch back, instead of burning the remainder.
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
    if round.residue_recipient_set == 0 {
        return Err(IntexFactoryError::ResidueRecipientUnknown(worldwide_day).into());
    }
    // The per-batch cap keeps the sum within `amount`; a shortfall here means
    // the round accounting is corrupt.
    let remainder = round.amount.checked_sub(round.paid_so_far).ok_or_else(|| {
        PrecompileError::Fatal("certified payout exceeded the round amount".into())
    })?;
    if !remainder.is_zero() {
        storage.transfer_balance(INTEX_FACTORY_ADDRESS, round.residue_recipient, remainder)?;
        outbe_intex::api::set_certified_paid_so_far(storage, worldwide_day, round.amount)?;
    }
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ContributorRoundClosed {
            worldwideDay: worldwide_day,
            paidAmount: round.amount,
            burnedAmount: U256::ZERO,
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

/// Progress of one day's payout round; all-zero when no round is open.
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

/// Floor share of one leaf. A zero certified total has no weight to divide:
/// a positive nominal is rejected, and a zero nominal takes nothing here so
/// the whole pot can reach leaf 0 at completion.
fn floor_share(amount: U256, nominal: U256, total: U256, worldwide_day: u32) -> Result<U256> {
    // The generation reader already rejects count > 0 with a zero total.
    // A caller that still passes that total must not pay a positive nominal.
    if total.is_zero() {
        return if nominal.is_zero() {
            Ok(U256::ZERO)
        } else {
            Err(IntexFactoryError::ZeroEligibleNominal(worldwide_day).into())
        };
    }
    Ok(amount
        .checked_mul(nominal)
        .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?
        / total)
}

/// Stores the proof-authenticated leaf-0 owner. The same owner is idempotent.
fn remember_leaf_zero(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    owner: Address,
) -> Result<()> {
    let round = outbe_intex::api::certified_payout_round(storage, worldwide_day)?
        .ok_or(IntexFactoryError::NoCertifiedRound(worldwide_day))?;
    if round.residue_recipient_set != 0 {
        if round.residue_recipient != owner {
            return Err(IntexFactoryError::ResidueRecipientConflict(worldwide_day).into());
        }
        return Ok(());
    }
    outbe_intex::api::set_certified_residue_recipient(storage, worldwide_day, owner)
}

fn leaf_zero_paid(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<bool> {
    let word = outbe_intex::api::paid_leaves_word(storage, worldwide_day, 0)?;
    Ok((word & U256::from(1u8)) != U256::ZERO)
}
