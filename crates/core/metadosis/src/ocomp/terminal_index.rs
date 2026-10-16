//! Per-WorldwideDay terminal-evidence index.
//!
//! One WorldwideDay owns exactly one terminal IntentId. Entries live in
//! the sparse `ocomp_terminal_intents` mapping under a domain-separated
//! composite key. `ocomp_terminal_counts` is the sole authority for each
//! day's length. Entries are immutable after the write. Retirement deletes
//! them together with the day.

use alloy_primitives::{keccak256, B256};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::{errors::MetadosisStorageCorruptionError, schema::MetadosisContract};

/// Domain separator for terminal-index entry keys. Versioned so a future
/// layout change cannot collide with V1 keys.
const TERMINAL_INDEX_DOMAIN: &[u8] = b"OUTBE_OCOMP_TERMINAL_INDEX_V1";

/// Derives the entry key for one `(worldwide_day, index)` position:
/// `keccak(domain || wwd_be || index_be)`. Big-endian fixed-width preimage,
/// deterministic across proposer and validator.
pub(crate) fn terminal_entry_key(wwd: WorldwideDay, index: u16) -> B256 {
    let mut preimage = [0_u8; 35];
    preimage[..29].copy_from_slice(TERMINAL_INDEX_DOMAIN);
    preimage[29..33].copy_from_slice(&wwd.value().to_be_bytes());
    preimage[33..35].copy_from_slice(&index.to_be_bytes());
    keccak256(preimage)
}

impl MetadosisContract<'_> {
    /// Reads the day's terminal record count. A day with no terminal history
    /// reads zero.
    pub(crate) fn terminal_intent_count(&self, wwd: WorldwideDay) -> Result<u16> {
        self.ocomp_terminal_counts.read(&wwd)
    }

    /// Reads the terminal IntentId at one position, `None` when the slot is
    /// vacant. Positions below the day's count must never be vacant. Callers
    /// that iterate use [`Self::terminal_intents_for`], which enforces that.
    #[cfg(test)]
    pub(crate) fn terminal_intent_at(&self, wwd: WorldwideDay, index: u16) -> Result<Option<B256>> {
        let entry = self
            .ocomp_terminal_intents
            .read(&terminal_entry_key(wwd, index))?;
        Ok((!entry.is_zero()).then_some(entry))
    }

    /// Materializes the day's terminal IntentIds in canonical attempt order.
    /// Fails closed when the stored count violates the single-attempt model or
    /// the terminal position is vacant.
    #[cfg(test)]
    pub(crate) fn terminal_intents_for(&self, wwd: WorldwideDay) -> Result<Vec<B256>> {
        let count = self.terminal_intent_count(wwd)?;
        if count > 1 {
            return Err(
                MetadosisStorageCorruptionError::OcompTerminalIndexViolatesSingleAttemptModel
                    .into(),
            );
        }
        let mut intents = Vec::new();
        intents
            .try_reserve_exact(usize::from(count))
            .map_err(|_| MetadosisStorageCorruptionError::AllocateBoundedOcompTerminalIndex)?;
        for index in 0..count {
            let intent_id = self.terminal_intent_at(wwd, index)?.ok_or(
                MetadosisStorageCorruptionError::OcompTerminalIndexSparseBelowRecordedCount,
            )?;
            intents.push(intent_id);
        }
        Ok(intents)
    }

    /// Appends one terminal IntentId to the day's list. Rejects a zero id, a
    /// day that already has a terminal job, and any overwrite of its occupied
    /// position. The function writes the entry before the count, so the count
    /// never references a vacant slot.
    pub(crate) fn push_terminal_intent(
        &mut self,
        wwd: WorldwideDay,
        intent_id: B256,
    ) -> Result<()> {
        if intent_id.is_zero() {
            return Err(
                MetadosisStorageCorruptionError::OcompTerminalIndexRejectsZeroIntentId.into(),
            );
        }
        let count = self.terminal_intent_count(wwd)?;
        if count != 0 {
            return Err(
                MetadosisStorageCorruptionError::OcompWorldwideDayAlreadyHasTerminalJob.into(),
            );
        }
        let key = terminal_entry_key(wwd, count);
        if !self.ocomp_terminal_intents.read(&key)?.is_zero() {
            return Err(MetadosisStorageCorruptionError::OcompTerminalIndexEntryImmutable.into());
        }
        let next = count
            .checked_add(1)
            .ok_or(MetadosisStorageCorruptionError::OcompTerminalIndexCountOverflow)?;
        self.ocomp_terminal_intents.write(&key, intent_id)?;
        self.ocomp_terminal_counts.write(&wwd, next)?;
        Ok(())
    }

    /// Deletes the day's terminal index entries and count word. WorldwideDay
    /// retirement calls this. The day's own recorded count bounds the work.
    pub(crate) fn delete_terminal_index(&mut self, wwd: WorldwideDay) -> Result<()> {
        let count = self.terminal_intent_count(wwd)?;
        for index in 0..count {
            self.ocomp_terminal_intents
                .write(&terminal_entry_key(wwd, index), B256::ZERO)?;
        }
        self.ocomp_terminal_counts.write(&wwd, 0)?;
        Ok(())
    }
}
