//! Replay guards of an offer: its SU hashes and one offer for each owner and Worldwide Day.

use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::finalized_guard_ring::FinalizedGuardRing;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::WorldwideDay;

use crate::errors::NodError;
use crate::NodContract;

/// Number of recent SU hashes retained in the replay-guard ring.
///
/// An SU id is valid across the WorldwideDay pipeline horizon, from the forming
/// start at 10:00 D-1 to the offering end at 12:00 D+2 (74h). At about
/// 50,000 SU hashes per day, `1 << 20` (`1_048_576`) entries cover about 21
/// days of volume before eviction. A change to this value is a hard fork.
pub const SU_HASH_RETAIN: u64 = 1 << 20;

fn offer_mark_key(worldwide_day: WorldwideDay, owner: Address) -> B256 {
    let mut buf = [0u8; 24];
    buf[0..4].copy_from_slice(&worldwide_day.value().to_be_bytes());
    buf[4..24].copy_from_slice(owner.as_slice());
    keccak256(buf)
}

impl<'storage> NodContract<'storage> {
    pub fn has_used_su_hash(&self, su_hash: B256) -> Result<bool> {
        self.used_su_hashes.read(&su_hash)
    }

    fn su_hash_prune_ring(&self) -> FinalizedGuardRing<'_, 'storage> {
        FinalizedGuardRing {
            entries: &self.su_hash_ring,
            cursor: &self.su_hash_ring_seq,
        }
    }

    fn mark_su_hashes_used(&mut self, hashes: &[B256]) -> Result<()> {
        for hash in hashes {
            if self.has_used_su_hash(*hash)? {
                return Err(NodError::SuHashAlreadyUsed.into());
            }
            self.used_su_hashes.write(hash, true)?;
        }
        let ring = self.su_hash_prune_ring();
        for hash in hashes {
            ring.record_with_retain::<SU_HASH_RETAIN>(
                *hash,
                |evicted| self.used_su_hashes.write(&evicted, false),
                || NodError::SuHashRingSeqOverflow.into(),
            )?;
        }
        Ok(())
    }
}

/// Marks the offer of `owner` for `worldwide_day` and its SU hashes. A second offer or a
/// used SU hash reverts.
pub fn mark_offer(
    storage: &StorageHandle<'_>,
    (worldwide_day, owner): (WorldwideDay, Address),
    su_hashes: &[B256],
) -> Result<()> {
    let mut nod = NodContract::new(storage.clone());
    let key = offer_mark_key(worldwide_day, owner);
    if nod.offer_marks.read(&key)? {
        return Err(NodError::OfferAlreadyMade.into());
    }
    nod.offer_marks.write(&key, true)?;
    nod.mark_su_hashes_used(su_hashes)
}
