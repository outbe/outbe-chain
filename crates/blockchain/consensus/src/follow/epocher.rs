//! Height→epoch strategy for the follower, read from the chain rather than
//! computed.
//!
//! **Why no formula.** A consensus epoch is a plain counter — activating a DKG
//! bumps it by one (`next_consensus_epoch_after_dkg_activation`). The height it
//! activates at is a running accumulator: the next planned activation is
//! `last_activation + L`, the anchor is then re-set to
//! `max(observed_height, planned)`, and a ceremony may legitimately land up to
//! `activation_grace_blocks` late. Nothing keeps epoch boundaries on multiples
//! of `L`, and a live chain has been observed with epoch 185 spanning
//! 222_001..=223_122 (1122 blocks) and epoch 186 starting at 223_123 — 78
//! blocks before any fixed grid would put it.
//!
//! That matters only for a follower. A validator's marshal takes finalizations
//! straight from the Simplex engine, keyed by `finalization.epoch()`. A
//! follower has no engine: every block arrives through the marshal's
//! resolver-delivery path, which keys the committee by
//! `epocher.containing(height).epoch()` and then asserts
//! `finalization.epoch() == that`. One wrong answer there rejects the block
//! permanently — the marshal re-requests it forever and the follower never
//! moves past it.
//!
//! So [`FollowerEpocher`] answers from observation: the block carrying epoch
//! `E`'s `BoundaryOutcome` IS `first(E)` (the producer emits it on the first
//! block the new committee signs), and the resolver records that height while
//! it is already decoding the artifact to register the committee. Heights below
//! the first recorded boundary are unknown rather than guessed.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use commonware_consensus::types::{Epoch, EpochInfo, Epocher, Height};

/// Observed epoch boundaries, shared between the resolver (which records them)
/// and the marshal (which reads them). Clones share one map.
#[derive(Clone, Debug, Default)]
pub struct FollowerEpocher {
    /// First height of each epoch, keyed by that height so a lookup for `h` is
    /// "the greatest boundary at or below `h`".
    boundaries: Arc<RwLock<BTreeMap<u64, u64>>>,
}

impl FollowerEpocher {
    /// An empty epocher. It knows nothing until boundaries are recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `epoch` begins at `height`.
    ///
    /// Idempotent, and monotone downward per epoch: fetches arrive out of
    /// order, so a later observation may reveal a LOWER first height for an
    /// epoch already known. Keeping the higher one would map the blocks in
    /// between to the previous epoch and have them rejected forever.
    pub fn record(&self, height: Height, epoch: Epoch) {
        let mut boundaries = match self.boundaries.write() {
            Ok(guard) => guard,
            // A poisoned lock means a writer panicked mid-update. The map is a
            // cache of observations, not authority — drop the write rather than
            // take down the follower.
            Err(poisoned) => poisoned.into_inner(),
        };
        let (height, epoch) = (height.get(), epoch.get());
        if let Some((&known_height, _)) = boundaries.iter().find(|(_, &e)| e == epoch) {
            if known_height <= height {
                return;
            }
            boundaries.remove(&known_height);
        }
        boundaries.insert(height, epoch);
    }
}

impl Epocher for FollowerEpocher {
    fn containing(&self, height: Height) -> Option<EpochInfo> {
        let boundaries = match self.boundaries.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let h = height.get();
        let (&first, &epoch) = boundaries.range(..=h).next_back()?;
        // The newest epoch has no successor yet, so it extends to the end of
        // the range. `EpochInfo` subtracts without checking, so `last` must
        // never fall below the queried height.
        let last = boundaries
            .range((h + 1)..)
            .next()
            .map_or(u64::MAX, |(&next_first, _)| next_first.saturating_sub(1));
        Some(EpochInfo::new(
            Epoch::new(epoch),
            height,
            Height::new(first),
            Height::new(last),
        ))
    }

    fn first(&self, epoch: Epoch) -> Option<Height> {
        let boundaries = match self.boundaries.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        boundaries
            .iter()
            .find(|(_, &e)| e == epoch.get())
            .map(|(&first, _)| Height::new(first))
    }

    fn last(&self, epoch: Epoch) -> Option<Height> {
        let boundaries = match self.boundaries.read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let first = boundaries
            .iter()
            .find(|(_, &e)| e == epoch.get())
            .map(|(&first, _)| first)?;
        let last = boundaries
            .range((first + 1)..)
            .next()
            .map_or(u64::MAX, |(&next_first, _)| next_first.saturating_sub(1));
        Some(Height::new(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth read off the LIVE chain with `outbe_getFinalization` (the
    /// certificate's first varint is the epoch), not invented:
    ///
    /// | height  | epoch |
    /// |---------|-------|
    /// | 222_001 | 185   |
    /// | 223_122 | 185   |
    /// | 223_123 | 186   |
    /// | 224_400 | 186   |
    ///
    /// Epoch 186 therefore begins at 223_123, while a fixed `L = 1200` grid
    /// would place it at 223_201 — the 78-block gap that stalls the follower,
    /// because the marshal asserts `finalization.epoch() == containing(h)`.
    #[test]
    fn containing_follows_recorded_boundaries_not_a_grid() {
        let e = FollowerEpocher::new();
        e.record(Height::new(222_001), Epoch::new(185));
        e.record(Height::new(223_123), Epoch::new(186));

        let epoch_of = |h: u64| e.containing(Height::new(h)).unwrap().epoch().get();
        assert_eq!(epoch_of(222_001), 185, "boundary block itself is epoch 185");
        assert_eq!(
            epoch_of(223_122),
            185,
            "last block before the next boundary"
        );
        assert_eq!(epoch_of(223_123), 186, "off-grid boundary starts epoch 186");
        assert_eq!(
            epoch_of(224_400),
            186,
            "still epoch 186 well past the 223_201 grid line"
        );
    }

    /// `EpochInfo::length()` and `relative()` subtract without checking, so a
    /// returned `first` above the queried height — or a `last` below it — is a
    /// panic waiting to happen inside commonware.
    #[test]
    fn containing_always_brackets_the_queried_height() {
        let e = FollowerEpocher::new();
        e.record(Height::new(100), Epoch::new(1));
        e.record(Height::new(200), Epoch::new(2));

        for h in [100u64, 101, 150, 199, 200, 201, 10_000] {
            let info = e.containing(Height::new(h)).unwrap();
            assert!(
                info.first().get() <= h,
                "first {} above queried height {h}",
                info.first().get()
            );
            assert!(
                info.last().get() >= h,
                "last {} below queried height {h}",
                info.last().get()
            );
        }
    }

    /// Out-of-order fetches can surface a lower first height for an epoch that
    /// is already recorded. Keeping the higher one would map the blocks in
    /// between to the previous epoch and get them rejected forever.
    #[test]
    fn record_lowers_an_already_known_boundary() {
        let e = FollowerEpocher::new();
        e.record(Height::new(500), Epoch::new(7));
        e.record(Height::new(480), Epoch::new(7));

        assert_eq!(e.first(Epoch::new(7)).unwrap().get(), 480);
        assert_eq!(e.containing(Height::new(480)).unwrap().epoch().get(), 7);
        assert_eq!(e.containing(Height::new(490)).unwrap().epoch().get(), 7);
    }

    /// A higher height for a known epoch must NOT move the boundary up.
    #[test]
    fn record_keeps_the_lowest_boundary_seen() {
        let e = FollowerEpocher::new();
        e.record(Height::new(480), Epoch::new(7));
        e.record(Height::new(500), Epoch::new(7));

        assert_eq!(e.first(Epoch::new(7)).unwrap().get(), 480);
    }

    /// Nothing recorded yet means "I do not know" — never a guess. The marshal
    /// answers `true` to a `None` and silently drops the delivery, so this has
    /// to be a deliberate, covered behaviour.
    #[test]
    fn empty_map_knows_nothing() {
        let e = FollowerEpocher::new();
        assert!(e.containing(Height::new(0)).is_none());
        assert!(e.containing(Height::new(1_000)).is_none());
        assert!(e.first(Epoch::new(0)).is_none());
        assert!(e.last(Epoch::new(0)).is_none());
    }

    /// Below the first recorded boundary the map must not invent an epoch.
    #[test]
    fn heights_below_the_first_boundary_are_unknown() {
        let e = FollowerEpocher::new();
        e.record(Height::new(222_001), Epoch::new(185));

        assert!(e.containing(Height::new(222_000)).is_none());
        assert_eq!(
            e.containing(Height::new(222_001)).unwrap().epoch().get(),
            185
        );
    }

    /// `last(E)` is bounded by the next recorded boundary; the newest epoch has
    /// no upper bound yet, so it must still bracket any height above it.
    #[test]
    fn last_tracks_the_following_boundary() {
        let e = FollowerEpocher::new();
        e.record(Height::new(100), Epoch::new(1));
        e.record(Height::new(200), Epoch::new(2));

        assert_eq!(e.last(Epoch::new(1)).unwrap().get(), 199);
        let newest = e.containing(Height::new(10_000)).unwrap();
        assert_eq!(newest.epoch().get(), 2);
        assert!(newest.last().get() >= 10_000);
    }

    /// The epocher is shared between the resolver (writer) and the marshal
    /// (reader) as a clone, so a write through one clone must be visible
    /// through the other.
    #[test]
    fn clones_share_one_map() {
        let writer = FollowerEpocher::new();
        let reader = writer.clone();
        writer.record(Height::new(42), Epoch::new(3));

        assert_eq!(reader.containing(Height::new(42)).unwrap().epoch().get(), 3);
    }
}
