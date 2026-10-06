use alloy_primitives::{Address, B256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::schema::SlashIndicator;

/// Number of recent finalized blocks whose per-`fb_hash` slash-window
/// guards (`voter_window_slashed`, `proposer_window_slashed`) stay live. The
/// replay horizon is the K-block late-finalize window, so retaining the last 64
/// finalized blocks is generous. [`prune_slash_guards`] prunes older guards.
/// Mirrors `FINALIZED_PARTICIPATION_RETAIN` /
/// `BLOCK_GUARD_RETAIN`. Changing it is a hard fork.
pub const SLASH_GUARD_RETAIN: u64 = 64;

/// Slash every window-close absentee of the finalized block `fb_hash`, exactly
/// once across metadata replays.
///
/// The window-close absentee pass is atomic per finalized block. A single
/// `voter_window_slashed[fb_hash]` bool therefore replaces the former unbounded
/// `mapping(fb_hash => mapping(address => bool))`:
/// - If the guard is already set, the whole pass is a no-op.
/// - Otherwise, the pass slashes every absentee and sets the guard.
///
/// `absentees` is the deterministic absentee set that the caller computed
/// from the committed committee snapshot.
pub fn slash_window_voters(
    storage: StorageHandle,
    fb_hash: B256,
    absentees: &[Address],
) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    if si.voter_window_slashed.read(&fb_hash)? {
        return Ok(());
    }
    for absentee in absentees {
        si.slash_voter(*absentee)?;
    }
    si.voter_window_slashed.write(&fb_hash, true)?;
    Ok(())
}

/// Slash the missed-proposer events of the finalized block `fb_hash`, exactly
/// once across metadata replays.
///
/// `missed` is the ordered missed-proposer event list from the finalized
/// parent's Phase 1 metadata. The same proposer may appear more than once
/// (multiple skipped views). This one atomic pass slashes that proposer once per
/// occurrence. A single `proposer_window_slashed[fb_hash]` bool replaces the
/// former per-event `keccak256(fb_hash||index||addr)` guard, which grew without
/// bound.
pub fn slash_window_proposers(
    storage: StorageHandle,
    fb_hash: B256,
    missed: &[Address],
) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    if si.proposer_window_slashed.read(&fb_hash)? {
        return Ok(());
    }
    for validator in missed {
        si.slash_proposer(*validator)?;
    }
    si.proposer_window_slashed.write(&fb_hash, true)?;
    Ok(())
}

/// Record `fb_hash` in the prune ring and clear the slash-window guards of
/// the finalized block evicted `SLASH_GUARD_RETAIN` records ago.
///
/// The Phase 1 (`CertifiedParentAccounting`) path calls this once per finalized
/// block. That path sees every finalized block exactly once as a direct parent. The
/// evicted block is `SLASH_GUARD_RETAIN` >> K blocks old, so its window can no
/// longer be replayed. Clearing its guards cannot weaken replay protection
/// for any block that is still inside the window. Without this, `voter_window_slashed`
/// and `proposer_window_slashed` accumulate one entry per finalized block with
/// any persistent miss rate, forever.
pub fn prune_slash_guards(storage: StorageHandle, fb_hash: B256) -> Result<()> {
    let si = SlashIndicator::new(storage);
    let seq = si.slash_guard_ring_seq.read()?;
    let idx = seq % SLASH_GUARD_RETAIN;
    let evicted = si.slash_guard_ring.read(&idx)?;
    if evicted != B256::ZERO && evicted != fb_hash {
        si.voter_window_slashed.write(&evicted, false)?;
        si.proposer_window_slashed.write(&evicted, false)?;
    }
    si.slash_guard_ring.write(&idx, fb_hash)?;
    si.slash_guard_ring_seq.write(
        seq.checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("slash_guard_ring_seq overflow".into()))?,
    )?;
    Ok(())
}

/// Called from post-execution when consensus detects byzantine behavior
/// (equivocation). The `evidence_processed` guard inside `slash_byzantine`
/// already makes it idempotent. This refactor does not affect it.
pub fn slash_byzantine(storage: StorageHandle, validator: Address) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    si.slash_byzantine(validator)
}
