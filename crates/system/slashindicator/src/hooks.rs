use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::schema::SlashIndicator;

/// Called from post-execution when a voter was absent for a finalized
/// block. Idempotent under metadata-tx replays: the
/// `slashed_voter_for_block[fb_hash][validator]` guard short-circuits
/// the inner counter increment if this `(fb_hash, validator)` pair has
/// already been processed.
///
/// `fb_hash` is the finalized block hash (`metadata.finalized_block_hash`)
/// — the natural composite key for "was this miss already counted?".
pub fn slash_voter(storage: StorageHandle, fb_hash: B256, validator: Address) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    if si
        .slashed_voter_for_block
        .get_nested(&fb_hash)
        .read(&validator)?
    {
        return Ok(());
    }
    si.slash_voter(validator)?;
    si.slashed_voter_for_block
        .get_nested(&fb_hash)
        .write(&validator, true)?;
    Ok(())
}

/// Called from Phase 1 finalized-parent metadata for each missed-proposer event.
///
/// `metadata.missed_proposers` is an ordered event list, so the same proposer may
/// appear multiple times before one finalized block when multiple views were
/// skipped. The idempotency key includes the event index to replay exact
/// metadata safely without collapsing those duplicate events.
pub fn slash_proposer_event(
    storage: StorageHandle,
    fb_hash: B256,
    event_index: u64,
    validator: Address,
) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    let event_key = proposer_event_key(fb_hash, event_index, validator);
    if si.slashed_proposer_event.read(&event_key)? {
        return Ok(());
    }
    si.slash_proposer(validator)?;
    si.slashed_proposer_event.write(&event_key, true)?;
    Ok(())
}

fn proposer_event_key(fb_hash: B256, event_index: u64, validator: Address) -> B256 {
    let mut bytes = [0u8; 60];
    bytes[..32].copy_from_slice(fb_hash.as_slice());
    bytes[32..40].copy_from_slice(&event_index.to_be_bytes());
    bytes[40..].copy_from_slice(validator.as_slice());
    keccak256(bytes)
}

/// Called from post-execution when consensus detects byzantine behavior
/// (equivocation). Already idempotent via the `evidence_processed` guard
/// inside `slash_byzantine`; not affected by this refactor.
pub fn slash_byzantine(storage: StorageHandle, validator: Address) -> Result<()> {
    let mut si = SlashIndicator::new(storage);
    si.slash_byzantine(validator)
}
