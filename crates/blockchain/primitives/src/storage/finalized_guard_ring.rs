//! Bounded prune ring of recent finalized-block hashes.
//!
//! System contracts keep per-finalized-block replay guards. A guard of a block
//! older than the late-finalize window can never be used again. Each contract
//! records every finalized block in a ring of [`FINALIZED_GUARD_RETAIN`] entries
//! and clears the guards of the block that the new entry replaces.

use alloy_primitives::B256;

use crate::consensus::LATE_FINALIZE_WINDOW_K;
use crate::error::{PrecompileError, Result};
use crate::storage::types::{Mapping, Slot};

/// Number of recent finalized blocks whose replay guards stay live. A change to
/// this value is a hard fork.
pub const FINALIZED_GUARD_RETAIN: u64 = 64;

// The ring must keep every block that a late finalization can still replay.
const _: () = assert!(FINALIZED_GUARD_RETAIN > LATE_FINALIZE_WINDOW_K);

/// The storage of one prune ring: the entries and the write cursor.
pub struct FinalizedGuardRing<'a, 'storage> {
    /// Ring entries, keyed by `cursor % FINALIZED_GUARD_RETAIN`.
    pub entries: &'a Mapping<'storage, u64, B256>,
    /// Monotonic count of recorded blocks.
    pub cursor: &'a Slot<'storage, u64>,
}

impl FinalizedGuardRing<'_, '_> {
    /// Records `fb_hash` in the ring.
    ///
    /// The operations occur in this order:
    /// 1. Read the cursor and the entry at `cursor % FINALIZED_GUARD_RETAIN`.
    /// 2. If the entry is set and is not `fb_hash`, call `evict` with it.
    /// 3. Write `fb_hash` to the entry.
    /// 4. Write `cursor + 1`. At the cursor maximum, return `overflow()`
    ///    instead. The entry write of step 3 stays.
    pub fn record(
        &self,
        fb_hash: B256,
        evict: impl FnOnce(B256) -> Result<()>,
        overflow: impl FnOnce() -> PrecompileError,
    ) -> Result<()> {
        let seq = self.cursor.read()?;
        let idx = seq % FINALIZED_GUARD_RETAIN;
        let evicted = self.entries.read(&idx)?;
        if evicted != B256::ZERO && evicted != fb_hash {
            evict(evicted)?;
        }
        self.entries.write(&idx, fb_hash)?;
        self.cursor
            .write(seq.checked_add(1).ok_or_else(overflow)?)?;
        Ok(())
    }
}

/// Finalized-block hash `i` of prune-ring tests.
#[cfg(any(test, feature = "test-utils"))]
pub fn test_ring_hash(i: u64) -> B256 {
    B256::left_padding_from(&i.to_be_bytes())
}

/// One ring entry and the ring cursor, as prune-ring tests read them.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, PartialEq, Eq)]
pub struct RingPosition {
    /// The entry at the index that the test reads.
    pub entry: B256,
    /// The cursor.
    pub seq: u64,
}

#[cfg(any(test, feature = "test-utils"))]
impl FinalizedGuardRing<'_, '_> {
    /// Writes the cursor `seq`, then `entry` at `seq % FINALIZED_GUARD_RETAIN`.
    pub fn seed_for_test(&self, seq: u64, entry: B256) -> Result<()> {
        self.cursor.write(seq)?;
        self.entries.write(&(seq % FINALIZED_GUARD_RETAIN), entry)
    }

    /// Reads the entry at `idx`, then the cursor.
    pub fn position_for_test(&self, idx: u64) -> Result<RingPosition> {
        Ok(RingPosition {
            entry: self.entries.read(&idx)?,
            seq: self.cursor.read()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};

    use super::*;
    use crate::storage::hashmap::{HashMapStorageProvider, MutationPrefixViews};
    use crate::storage::StorageHandle;

    const OWNER: Address = Address::repeat_byte(0x51);
    const ENTRIES_SLOT: u64 = 1;
    const CURSOR_SLOT: u64 = 2;
    const GUARD_SLOT: u64 = 3;

    /// The ring storage and the guard map of the test contract.
    struct TestRing<'storage> {
        entries: Mapping<'storage, u64, B256>,
        cursor: Slot<'storage, u64>,
        guards: Mapping<'storage, B256, bool>,
    }

    impl<'storage> TestRing<'storage> {
        fn new(storage: StorageHandle<'storage>) -> Self {
            Self {
                entries: Mapping::new(U256::from(ENTRIES_SLOT), OWNER, storage.clone()),
                cursor: Slot::new(U256::from(CURSOR_SLOT), OWNER, storage.clone()),
                guards: Mapping::new(U256::from(GUARD_SLOT), OWNER, storage),
            }
        }

        fn ring(&self) -> FinalizedGuardRing<'_, 'storage> {
            FinalizedGuardRing {
                entries: &self.entries,
                cursor: &self.cursor,
            }
        }

        /// Records `fb_hash`. The eviction clears the guard of the evicted block.
        fn record(&self, fb_hash: B256) -> Result<()> {
            self.ring().record(
                fb_hash,
                |evicted| self.guards.write(&evicted, false),
                || PrecompileError::Revert("test ring overflow".into()),
            )
        }
    }

    /// The guard of one block and the ring position.
    #[derive(Debug, PartialEq)]
    struct RingView {
        guard: bool,
        ring: RingPosition,
    }

    fn ring_view(guard: bool, entry: B256, seq: u64) -> RingView {
        RingView {
            guard,
            ring: RingPosition { entry, seq },
        }
    }

    /// Storage with the cursor `seq`, `entry` at `seq % RETAIN` and the guard
    /// of every block in `guarded`.
    fn seeded(seq: u64, entry: B256, guarded: &[B256]) -> HashMapStorageProvider {
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            let test_ring = TestRing::new(storage);
            test_ring.ring().seed_for_test(seq, entry).unwrap();
            for hash in guarded {
                test_ring.guards.write(hash, true).unwrap();
            }
        });
        provider
    }

    fn view(provider: &mut HashMapStorageProvider, guarded: B256, idx: u64) -> RingView {
        provider.enter(|storage| {
            let test_ring = TestRing::new(storage);
            RingView {
                guard: test_ring.guards.read(&guarded).unwrap(),
                ring: test_ring.ring().position_for_test(idx).unwrap(),
            }
        })
    }

    #[test]
    fn record_evicts_then_writes_the_entry_then_the_cursor() {
        let evicted = test_ring_hash(1);
        let fb_hash = test_ring_hash(2);
        let seq = FINALIZED_GUARD_RETAIN * 2 + 9;
        let views = HashMapStorageProvider::mutation_prefix_views(
            || seeded(seq, evicted, &[evicted]),
            |storage| TestRing::new(storage).record(fb_hash),
            |provider| view(provider, evicted, 9),
        )
        .unwrap();
        assert_eq!(
            views,
            MutationPrefixViews {
                before_mutation: vec![
                    ring_view(true, evicted, seq),
                    ring_view(false, evicted, seq),
                    ring_view(false, fb_hash, seq),
                ],
                complete: ring_view(false, fb_hash, seq + 1),
                mutations: 3,
            }
        );
    }

    #[test]
    fn record_never_evicts_an_empty_entry_or_the_recorded_block() {
        let fb_hash = test_ring_hash(7);
        for entry in [B256::ZERO, fb_hash] {
            let mut provider = seeded(3, entry, &[entry]);
            provider.clear_mutation_failure();
            provider
                .enter(|storage| TestRing::new(storage).record(fb_hash))
                .unwrap();
            assert_eq!(provider.clear_mutation_failure(), 2, "entry {entry}");
            assert_eq!(
                view(&mut provider, entry, 3),
                ring_view(true, fb_hash, 4),
                "entry {entry}"
            );
        }
    }

    /// After `FINALIZED_GUARD_RETAIN + 1` records only the first block is
    /// evicted. The last `FINALIZED_GUARD_RETAIN` blocks keep their guards.
    #[test]
    fn record_retains_the_last_retain_blocks() {
        let hashes: Vec<B256> = (1..=FINALIZED_GUARD_RETAIN + 1)
            .map(test_ring_hash)
            .collect();
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            let test_ring = TestRing::new(storage);
            for hash in &hashes {
                test_ring.guards.write(hash, true).unwrap();
                test_ring.record(*hash).unwrap();
            }
            let (first, retained) = hashes.split_first().unwrap();
            assert!(
                !test_ring.guards.read(first).unwrap(),
                "first block evicted"
            );
            for hash in retained {
                assert!(
                    test_ring.guards.read(hash).unwrap(),
                    "block {hash} retained"
                );
            }
            assert_eq!(
                test_ring.ring().position_for_test(0).unwrap(),
                RingPosition {
                    entry: *hashes.last().unwrap(),
                    seq: FINALIZED_GUARD_RETAIN + 1,
                }
            );
        });
    }

    #[test]
    fn eviction_failure_stops_before_the_entry_write() {
        let evicted = test_ring_hash(1);
        let mut provider = seeded(5, evicted, &[evicted]);
        let result = provider.enter(|storage| {
            TestRing::new(storage).ring().record(
                test_ring_hash(2),
                |_| Err(PrecompileError::Fatal("evict failed".into())),
                || PrecompileError::Revert("test ring overflow".into()),
            )
        });
        assert!(
            matches!(&result, Err(PrecompileError::Fatal(message)) if message == "evict failed"),
            "{result:?}"
        );
        assert_eq!(view(&mut provider, evicted, 5), ring_view(true, evicted, 5));
    }

    #[test]
    fn cursor_overflow_returns_the_caller_error_after_the_entry_write() {
        let evicted = test_ring_hash(1);
        let fb_hash = test_ring_hash(2);
        let mut provider = seeded(u64::MAX, evicted, &[evicted]);
        let result = provider.enter(|storage| TestRing::new(storage).record(fb_hash));
        assert!(
            matches!(&result, Err(PrecompileError::Revert(message)) if message == "test ring overflow"),
            "{result:?}"
        );
        assert_eq!(
            view(&mut provider, evicted, u64::MAX % FINALIZED_GUARD_RETAIN),
            ring_view(false, fb_hash, u64::MAX)
        );
    }
}
