use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;
use outbe_primitives::reshare_artifact::LateFinalizeCreditsArtifact;
use outbe_primitives::reshare_artifact::PerBlockCredit;
use outbe_primitives::storage::StorageHandle;

/// authentication: verify a late-finalize `credit`'s
/// proposer-supplied `fb_number`/`epoch`/`committee_set_hash` against the
/// canonical binding escrowed for that finalized block (keyed by `fb_number` in
/// `Rewards`). The BLS proof binds only `fb_hash`, so this is what prevents a
/// proposer from spoofing `fb_number` (to shrink the inclusion distance `k` and
/// inflate decay weight) or referencing a wrong committee. FATAL on a missing
/// escrow or any mismatch. Shared by the begin-zone body and the pre-exec gate.
pub(crate) fn authenticate_late_credit(
    storage: &StorageHandle,
    credit: &PerBlockCredit,
) -> Result<()> {
    let rewards = storage.contract::<outbe_rewards::contract::Rewards<'_>>();
    let escrowed_hash = rewards.pending_fb_hash_at.read(&credit.fb_number)?;
    if escrowed_hash == B256::ZERO {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: no escrow for fb_number {} (credit fb_hash {})",
            credit.fb_number, credit.fb_hash
        )));
    }
    if escrowed_hash != credit.fb_hash {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: fb_hash mismatch for fb_number {} (escrow {escrowed_hash}, credit {})",
            credit.fb_number, credit.fb_hash
        )));
    }
    let escrowed_epoch = rewards.pending_epoch_at.read(&credit.fb_number)?;
    if escrowed_epoch != credit.epoch {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: epoch mismatch for fb_number {} (escrow {escrowed_epoch}, credit {})",
            credit.fb_number, credit.epoch
        )));
    }
    let escrowed_csh = rewards
        .pending_committee_set_hash_at
        .read(&credit.fb_number)?;
    if escrowed_csh != credit.committee_set_hash {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: committee_set_hash mismatch for fb_number {} (escrow {escrowed_csh}, credit {})",
            credit.fb_number, credit.committee_set_hash
        )));
    }
    // Pin the rest of the signed binding (view, parent_view) to the canonical
    // certificate, so a credit whose aggregate is over a non-canonical view of the
    // same fb_hash (cross-view equivocation) is rejected here, not only by the
    // pre-exec BLS verify (which ties the credit's view to its signatures, not to
    // the finalized view). full binding.
    let escrowed_view = rewards.pending_view_at.read(&credit.fb_number)?;
    if escrowed_view != credit.view {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: view mismatch for fb_number {} (escrow {escrowed_view}, credit {})",
            credit.fb_number, credit.view
        )));
    }
    let escrowed_parent_view = rewards.pending_parent_view_at.read(&credit.fb_number)?;
    if escrowed_parent_view != credit.parent_view {
        return Err(PrecompileError::Fatal(format!(
            "LateFinalizeCredits: parent_view mismatch for fb_number {} (escrow {escrowed_parent_view}, credit {})",
            credit.fb_number, credit.parent_view
        )));
    }
    Ok(())
}

/// LateFinalizeCredits system tx: record the verified
/// late-finalize voters of each in-window batch at their inclusion distance
/// `k`, then close the window that just matured (`settle_matured` for block
/// `N - K`). The escrow residue is burned for mint/burn parity inside
/// `settle_window`; here we additionally route that same residue to terminal
/// Metadosis emission headroom (`emission_sink::apply`), recycling unpaid fees
/// instead of permanently destroying them.
///
/// Determinism: every batch's BLS aggregate was already FATAL-verified in the
/// executor's pre-exec preflight (`verify_late_finalize_credits_in_preexec`),
/// proposer and validator alike. This body re-resolves the committee snapshot
/// only to map the verified signer indices to addresses; the re-`verify`
/// is the single source of truth for the bitmap->index decoding and yields the
/// same indices on every node. Empty artifacts (no gathered credits) reduce to
/// the window-close `settle_matured`, which is a no-op until block `K+1`.
pub(crate) fn run_late_finalize_credits(
    ctx: &BlockRuntimeContext,
    artifact: &LateFinalizeCreditsArtifact,
) -> Result<()> {
    use outbe_consensus::proof::verify_late_finalize_proof;
    use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};

    let block_number = ctx.block.block_number;

    for credit in &artifact.batches {
        // Inclusion distance k = block_number - fb_number, range-checked
        // `1 <= k <= K` on the *executed body* artifact (the pre-exec preflight
        // range-checks the header; the stateless validator binds header<->body -
        // but this path must stand on its own: a credit outside the window must
        // never be recorded). Checked FIRST, before the expensive snapshot read
        // + BLS verify, so an out-of-window credit is rejected cheaply.
        let k_u64 = block_number.checked_sub(credit.fb_number).ok_or_else(|| {
            PrecompileError::Fatal(format!(
                "LateFinalizeCredits: fb_number {} >= block {block_number}",
                credit.fb_number
            ))
        })?;
        if k_u64 == 0 || k_u64 > LATE_FINALIZE_WINDOW_K {
            return Err(PrecompileError::Fatal(format!(
                "LateFinalizeCredits: fb_number {} outside inclusion window \
                 (distance {k_u64}, K={LATE_FINALIZE_WINDOW_K}) for block {block_number}",
                credit.fb_number
            )));
        }
        let k = u8::try_from(k_u64).map_err(|_| {
            PrecompileError::Fatal(format!(
                "LateFinalizeCredits: inclusion distance {k_u64} exceeds u8 for block {block_number}"
            ))
        })?;

        // bind the proposer-supplied
        // fb_number/epoch/committee_set_hash to the escrowed canonical binding for
        // this finalized block before recording. The BLS proof binds only fb_hash,
        // so without this a proposer could spoof fb_number (shrink k -> inflate
        // weight) or reference a wrong committee.
        authenticate_late_credit(&ctx.storage, credit)?;

        // Re-resolve the epoch committee the proof was produced for, to map
        // verified signer indices -> addresses, and re-verify the BLS aggregate
        // (FATAL on failure - never a soft receipt). The snapshot must exist (the
        // pre-exec preflight already read and verified against it).
        let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
        let snapshot = read_committee_snapshot(ctx.storage.clone(), snapshot_key)?.ok_or_else(
            || {
                PrecompileError::Fatal(format!(
                    "LateFinalizeCredits: missing committee snapshot for epoch={} key={snapshot_key}",
                    credit.epoch
                ))
            },
        )?;

        let signer_indices = verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
            PrecompileError::Fatal(format!(
                "LateFinalizeCredits: proof verify failed for fb={}: {error}",
                credit.fb_hash
            ))
        })?;

        for idx in signer_indices {
            let voter = snapshot
                .committee
                .get(idx)
                .map(|entry| entry.address)
                .ok_or_else(|| {
                    PrecompileError::Fatal(format!(
                        "LateFinalizeCredits: signer index {idx} out of committee range"
                    ))
                })?;
            outbe_rewards::late_settlement::record_late_credit(ctx, credit.fb_hash, voter, k)?;
        }
    }

    // Window-close miss & slashing pass: record misses and apply
    // punitive slashing for every committee member who never voted within K, using
    // the FINAL credited set. Must run BEFORE `settle_matured`, which frees the
    // `late_voter_*` credited set.
    record_window_close_absentees(ctx, block_number)?;

    // Window close: settle block N - K (the window that just matured). No-op
    // before block K+1 or when nothing was escrowed at that number. The residue
    // burn + terminal-Metadosis recycle and the per-window state cleanup happen
    // inside `settle_window`; nothing further is needed here.
    outbe_rewards::late_settlement::settle_matured(ctx, block_number, LATE_FINALIZE_WINDOW_K)?;

    Ok(())
}

/// Window-close miss & slashing pass. For the
/// window maturing at this block (`fb_number = block_number - K`), every committee
/// member that never voted within `K` - `committee(fb_number) \ credited` - has its
/// finalized-participation miss recorded and `slash_voter` applied (force-exit +
/// stake slash once the felony threshold is crossed).
///
/// Determinism: the committee snapshot and credited set are committed chain state;
/// absentees are emitted in committee order. Idempotent via the per-`fb_hash`
/// guards inside the validatorset / slashindicator hooks. The committee snapshot
/// is written at the epoch boundary and never pruned, so it is always present in
/// production; a missing snapshot fails open (skip) rather than halting the block.
///
/// Runs BEFORE `settle_matured`, which frees the `late_voter_*` credited set.
fn record_window_close_absentees(ctx: &BlockRuntimeContext, block_number: u64) -> Result<()> {
    use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};
    use std::collections::BTreeSet;

    let Some(fb_number) = block_number.checked_sub(LATE_FINALIZE_WINDOW_K) else {
        return Ok(());
    };
    if fb_number == 0 {
        return Ok(());
    }
    let Some(info) = outbe_rewards::late_settlement::window_close_credited(ctx, fb_number)? else {
        return Ok(());
    };

    let snapshot_key = committee_snapshot_key(info.epoch, info.committee_set_hash);
    let Some(snapshot) = read_committee_snapshot(ctx.storage.clone(), snapshot_key)? else {
        // Always present in production (written at the epoch boundary, never
        // pruned). Fail open rather than halt the block on a slashing-accounting
        // input that is missing only in degenerate/under-seeded states.
        tracing::warn!(
            target: "outbe::slashing",
            fb_number,
            epoch = info.epoch,
            "window-close absentee pass: committee snapshot missing; skipping",
        );
        return Ok(());
    };

    let credited: BTreeSet<Address> = info.credited.into_iter().collect();
    // Absentees = committee members who never voted within `K`, restricted to
    // currently-registered validators. Committee members are always registered in
    // production (the snapshot IS the validator set, and none can fully deregister
    // within `K` blocks), so the filter is a no-op there; it only guards a stray
    // non-registered binding from reverting the whole settlement phase via
    // `record_finalized_participation`'s strict registered-validator contract.
    let vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    let mut absentees: Vec<Address> = Vec::new();
    for entry in &snapshot.committee {
        if credited.contains(&entry.address) {
            continue;
        }
        if vs.is_validator(entry.address)? {
            absentees.push(entry.address);
        }
    }
    if absentees.is_empty() {
        return Ok(());
    }

    // Metric (E8 relocation): count val_missed_votes against the true absentee set
    // at window close. Idempotent via `finalized_participation_recorded[fb_hash]`.
    outbe_validatorset::hooks::record_finalized_participation(
        ctx.storage.clone(),
        info.fb_hash,
        &[],
        &absentees,
    )?;

    // Punitive: increment voter_miss_count and force-exit + slash at the felony
    // threshold. Idempotent + bounded via the per-`fb_hash` `voter_window_slashed`
    // guard; this whole absentee pass is atomic per finalized block.
    outbe_slashindicator::hooks::slash_window_voters(
        ctx.storage.clone(),
        info.fb_hash,
        &absentees,
    )?;

    Ok(())
}
