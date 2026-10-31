use alloy_primitives::B256;
use outbe_primitives::{error::Result, storage::finalized_guard_ring::FinalizedGuardRing};

use crate::errors::TributeFactoryError;
use crate::schema::{TributeFactoryContract, SU_HASH_RETAIN};

impl<'storage> TributeFactoryContract<'storage> {
    pub fn has_used_su_hash(&self, su_hash: B256) -> Result<bool> {
        self.used_su_hashes.read(&su_hash)
    }

    pub(crate) fn su_hash_prune_ring(&self) -> FinalizedGuardRing<'_, 'storage> {
        FinalizedGuardRing {
            entries: &self.su_hash_ring,
            cursor: &self.su_hash_ring_seq,
        }
    }

    pub(crate) fn mark_su_hashes_used(&mut self, hashes: &[B256]) -> Result<()> {
        for hash in hashes {
            if self.has_used_su_hash(*hash)? {
                return Err(TributeFactoryError::SuHashAlreadyUsed.into());
            }
            // Silent marker. Replay rejection reads this flag. No per-hash log.
            self.used_su_hashes.write(hash, true)?;
        }

        let ring = self.su_hash_prune_ring();
        for hash in hashes {
            ring.record_with_retain::<SU_HASH_RETAIN>(
                *hash,
                |evicted| self.used_su_hashes.write(&evicted, false),
                || TributeFactoryError::SuHashRingSeqOverflow.into(),
            )?;
        }
        Ok(())
    }
}
