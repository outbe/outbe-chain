use alloy_primitives::B256;
use outbe_primitives::error::Result;
use outbe_primitives::storage::finalized_guard_ring::FinalizedGuardRing;

use crate::errors::TributeFactoryError;
use crate::schema::{TributeFactoryContract, SU_HASH_RETAIN};

impl<'storage> TributeFactoryContract<'storage> {
    /// The prune ring of the used SU hash guards: the ring entries (slot 7)
    /// and the write cursor (slot 8).
    pub(crate) fn su_hash_prune_ring(&self) -> FinalizedGuardRing<'_, 'storage, SU_HASH_RETAIN> {
        FinalizedGuardRing {
            entries: &self.su_hash_ring,
            cursor: &self.su_hash_ring_seq,
        }
    }

    pub fn has_used_su_hash(&self, su_hash: B256) -> Result<bool> {
        self.used_su_hashes.read(&su_hash)
    }

    pub(crate) fn mark_su_hash_used(&mut self, su_hash: B256) -> Result<()> {
        if self.has_used_su_hash(su_hash)? {
            return Err(TributeFactoryError::SuHashAlreadyUsed.into());
        }
        // Silent marker. Replay rejection reads this flag. No per-hash log.
        self.used_su_hashes.write(&su_hash, true)?;
        // Record the hash in the prune ring. The ring clears the guard of the
        // hash recorded `SU_HASH_RETAIN` hashes ago.
        self.su_hash_prune_ring().record(
            su_hash,
            |evicted| self.used_su_hashes.write(&evicted, false),
            || TributeFactoryError::SuHashRingSeqOverflow.into(),
        )
    }

    pub(crate) fn mark_su_hashes_used(&mut self, hashes: &[B256]) -> Result<()> {
        for hash in hashes {
            self.mark_su_hash_used(*hash)?;
        }
        Ok(())
    }
}
