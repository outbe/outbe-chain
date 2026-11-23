//! Replay guards of an offer: its SU hashes and one offer for each owner and Worldwide Day.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::nod_encryption::EncryptedNodGroupLoadV1;
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

fn day_sum_scope(domain: &[u8], wwd: WorldwideDay) -> B256 {
    let mut bytes = domain.to_vec();
    bytes.extend_from_slice(&wwd.value().to_be_bytes());
    keccak256(bytes)
}

/// The scope of the encrypted nominal sum of the Nods of `wwd`.
pub fn day_nominal_scope(wwd: WorldwideDay) -> B256 {
    day_sum_scope(b"outbe/nod/day-nominal/v1", wwd)
}

/// The scope of the encrypted load sum of the Nods of `wwd`.
pub fn day_load_scope(wwd: WorldwideDay) -> B256 {
    day_sum_scope(b"outbe/nod/day-load/v1", wwd)
}

/// Decodes a stored day sum. An empty sum has no record.
pub fn decode_day_sum(bytes: &[u8]) -> Result<Option<EncryptedNodGroupLoadV1>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    postcard::from_bytes(bytes)
        .map(Some)
        .map_err(|_| NodError::EncryptedTermsMismatch.into())
}

pub fn encode_day_sum(record: &EncryptedNodGroupLoadV1) -> Result<Vec<u8>> {
    postcard::to_allocvec(record).map_err(|_| NodError::EncryptedTermsMismatch.into())
}

/// The plain sum a stored day sum of `scope` holds, read in the enclave. Zero when empty.
pub fn read_day_sum(storage: &StorageHandle<'_>, scope: B256, bytes: &[u8]) -> Result<U256> {
    let Some(record) = decode_day_sum(bytes)? else {
        return Ok(U256::ZERO);
    };
    if record.scope != scope || record.chain_id != storage.chain_id()? {
        return Err(NodError::EncryptedTermsMismatch.into());
    }
    crate::api::read_day_sum(storage, &record)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precompile::INod;
    use alloy_sol_types::SolError;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    fn reverts_with<E: SolError>(error: PrecompileError, expected: E) {
        assert!(
            matches!(&error, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected.abi_encode()),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn su_hash_can_only_be_used_once() {
        let mut provider = HashMapStorageProvider::new(1);
        let events_before = provider.get_ordered_events().to_vec();
        StorageHandle::enter(&mut provider, |storage| {
            let (a, b, c) = (
                B256::repeat_byte(0xAA),
                B256::repeat_byte(0xBB),
                B256::repeat_byte(0xDD),
            );
            let mut nod = NodContract::new(storage.clone());
            nod.mark_su_hashes_used(&[a, b]).unwrap();
            assert_eq!(nod.su_hash_ring_seq.read().unwrap(), 2);
            assert_eq!(nod.su_hash_ring.read(&0).unwrap(), a);
            assert_eq!(nod.su_hash_ring.read(&1).unwrap(), b);

            reverts_with(
                nod.mark_su_hashes_used(&[a]).unwrap_err(),
                INod::SuHashAlreadyUsed {},
            );
            let dup = B256::repeat_byte(0xCC);
            reverts_with(
                storage
                    .with_checkpoint(|| {
                        NodContract::new(storage.clone()).mark_su_hashes_used(&[dup, dup])
                    })
                    .unwrap_err(),
                INod::SuHashAlreadyUsed {},
            );

            // At the wrap point `[c, a]` still rejects `a`, which `c` would evict.
            nod.su_hash_ring_seq.write(SU_HASH_RETAIN).unwrap();
            reverts_with(
                storage
                    .with_checkpoint(|| {
                        NodContract::new(storage.clone()).mark_su_hashes_used(&[c, a])
                    })
                    .unwrap_err(),
                INod::SuHashAlreadyUsed {},
            );
            assert!(nod.has_used_su_hash(a).unwrap());
            assert!(!nod.has_used_su_hash(c).unwrap());

            nod.mark_su_hashes_used(&[c]).unwrap();
            assert!(!nod.has_used_su_hash(a).unwrap());
            assert!(nod.has_used_su_hash(b).unwrap());
            assert!(nod.has_used_su_hash(c).unwrap());
            assert_eq!(nod.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);
            assert_eq!(nod.su_hash_ring.read(&0).unwrap(), c);

            let self_hash = B256::repeat_byte(0xEE);
            nod.su_hash_prune_ring()
                .seed_for_test_with_retain::<SU_HASH_RETAIN>(SU_HASH_RETAIN, self_hash)
                .unwrap();
            nod.mark_su_hashes_used(&[self_hash]).unwrap();
            assert!(nod.has_used_su_hash(self_hash).unwrap());
            assert_eq!(nod.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);

            nod.su_hash_ring_seq.write(u64::MAX).unwrap();
            let tail = u64::MAX % SU_HASH_RETAIN;
            let tail_before = nod.su_hash_ring.read(&tail).unwrap();
            reverts_with(
                storage
                    .with_checkpoint(|| NodContract::new(storage.clone()).mark_su_hashes_used(&[a]))
                    .unwrap_err(),
                INod::SuHashRingSeqOverflow {},
            );
            assert!(!nod.has_used_su_hash(a).unwrap());
            assert_eq!(nod.su_hash_ring_seq.read().unwrap(), u64::MAX);
            assert_eq!(nod.su_hash_ring.read(&tail).unwrap(), tail_before);
        });
        assert_eq!(provider.get_ordered_events(), events_before);
    }

    #[test]
    fn an_owner_offers_once_for_each_worldwide_day() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let owner = Address::repeat_byte(0x11);
            let day = WorldwideDay::new(20_261_018);
            let next_day = WorldwideDay::new(20_261_019);
            mark_offer(&storage, (day, owner), &[B256::repeat_byte(1)]).unwrap();
            reverts_with(
                storage
                    .with_checkpoint(|| mark_offer(&storage, (day, owner), &[B256::repeat_byte(2)]))
                    .unwrap_err(),
                INod::OfferAlreadyMade {},
            );
            assert!(!NodContract::new(storage.clone())
                .has_used_su_hash(B256::repeat_byte(2))
                .unwrap());
            mark_offer(&storage, (next_day, owner), &[B256::repeat_byte(2)]).unwrap();
            mark_offer(
                &storage,
                (day, Address::repeat_byte(0x22)),
                &[B256::repeat_byte(3)],
            )
            .unwrap();
        });
    }
}
