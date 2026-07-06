//! Height→epoch strategy for the follower, matching outbe's on-chain
//! committee-epoch boundaries.
//!
//! **Why not [`FixedEpocher`].** outbe's consensus epoch `E` (the epoch carried
//! in every finalization certificate, `finalization.epoch()`) spans blocks
//! `[E·L+1, (E+1)·L]` — the boundary `BoundaryOutcome` that activates epoch `E`
//! rides block `E·L+1` (the FIRST block epoch `E` signs), and block `E·L` is the
//! LAST block of epoch `E-1`. (Verified on a live localnet, `L=60`: block 60 →
//! cert epoch 0, block 61 → cert epoch 1, block 120 → cert epoch 1, block 121 →
//! cert epoch 2; boundary outcomes ride blocks 1, 61, 121, 181.)
//!
//! `FixedEpocher(L)` instead puts the boundary at `E·L` (`containing(h)=h/L`),
//! so it disagrees with the cert epoch by one at every block that is a multiple
//! of `L` (block 60: `FixedEpocher`→epoch 1, but the cert is epoch 0). A
//! validator never notices: its marshal verifies finalizations delivered by the
//! Simplex *engine*, keyed by `finalization.epoch()` directly. A follower has no
//! engine and verifies *every* block through the marshal's resolver-delivery
//! path, which keys the committee by `epocher.containing(height).epoch()` AND
//! asserts `finalization.epoch() == that`. With `FixedEpocher` that assertion
//! fails on every block `E·L` (60, 120, …), stalling the follower one block
//! before each epoch boundary.
//!
//! [`FollowerEpocher`] fixes this by placing epoch boundaries where the chain
//! actually puts them: `containing(h).epoch() = (h−1)/L` for `h ≥ 1`, with epoch
//! `E≥1` covering `[E·L+1, (E+1)·L]`. Epoch 0 covers `[0, L]` — it additionally
//! owns the genesis anchor at height 0, which has no committee (it is never
//! verified via a certificate), so the extra slot is harmless. With this
//! epocher `containing(height) == finalization.epoch()` for every certified
//! block, and `first(E)` lands exactly on epoch `E`'s boundary-outcome block —
//! which is also where the follower driver must look to register epoch `E`'s
//! committee.

use commonware_consensus::types::{Epoch, EpochInfo, Epocher, Height};

/// Boundary-aligned epocher for the follower marshal + driver. Epoch `E≥1`
/// covers `[E·L+1, (E+1)·L]`; epoch 0 covers `[0, L]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowerEpocher {
    length: u64,
}

impl FollowerEpocher {
    /// Create an epocher for epoch length `length` blocks (must be > 0).
    pub const fn new(length: u64) -> Self {
        Self { length }
    }

    /// `(first, last)` height bounds for `epoch`, or `None` on overflow.
    fn bounds(&self, epoch: Epoch) -> Option<(Height, Height)> {
        let e = epoch.get();
        let last = e.checked_add(1)?.checked_mul(self.length)?;
        let first = if e == 0 {
            0
        } else {
            e.checked_mul(self.length)?.checked_add(1)?
        };
        Some((Height::new(first), Height::new(last)))
    }
}

impl Epocher for FollowerEpocher {
    fn containing(&self, height: Height) -> Option<EpochInfo> {
        // Epoch E covers (E·L, (E+1)·L]; height 0 (genesis anchor) is epoch 0.
        // So epoch = ceil(h / L) − 1 = (h − 1) / L for h ≥ 1, and 0 for h = 0.
        let h = height.get();
        let epoch = Epoch::new(h.saturating_sub(1) / self.length);
        let (first, last) = self.bounds(epoch)?;
        Some(EpochInfo::new(epoch, height, first, last))
    }

    fn first(&self, epoch: Epoch) -> Option<Height> {
        self.bounds(epoch).map(|(first, _)| first)
    }

    fn last(&self, epoch: Epoch) -> Option<Height> {
        self.bounds(epoch).map(|(_, last)| last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin `containing(height).epoch()` against the live-localnet ground truth
    /// (`L=60`): block 60 → cert epoch 0, 61 → 1, 120 → 1, 121 → 2. With this
    /// mapping the marshal's `finalization.epoch() == containing(height)` check
    /// holds for every certified block, so a follower can verify boundary blocks.
    #[test]
    fn containing_matches_onchain_cert_epochs() {
        let e = FollowerEpocher::new(60);
        let epoch_of = |h: u64| e.containing(Height::new(h)).unwrap().epoch().get();
        assert_eq!(epoch_of(0), 0, "genesis anchor is epoch 0");
        assert_eq!(epoch_of(1), 0);
        assert_eq!(epoch_of(59), 0);
        assert_eq!(epoch_of(60), 0, "block 60 is the LAST block of epoch 0");
        assert_eq!(epoch_of(61), 1, "block 61 (boundary) is the FIRST of epoch 1");
        assert_eq!(epoch_of(120), 1, "block 120 is the LAST block of epoch 1");
        assert_eq!(epoch_of(121), 2);
        assert_eq!(epoch_of(180), 2);
        assert_eq!(epoch_of(181), 3);
    }

    /// `first(E)` lands on epoch `E`'s boundary-outcome block (1, 61, 121, …) —
    /// exactly where the driver fetches to register epoch `E`'s committee.
    #[test]
    fn first_lands_on_boundary_block() {
        let e = FollowerEpocher::new(60);
        // first(0) = 0 (genesis anchor); epoch 0's boundary outcome rides block 1.
        assert_eq!(e.first(Epoch::new(0)).unwrap().get(), 0);
        assert_eq!(e.first(Epoch::new(1)).unwrap().get(), 61);
        assert_eq!(e.first(Epoch::new(2)).unwrap().get(), 121);
        assert_eq!(e.first(Epoch::new(3)).unwrap().get(), 181);
    }

    /// `last(E)` is the block `(E+1)·L` (60, 120, 180) — the last block epoch `E`
    /// signs, contiguous with `first(E+1) − 1`.
    #[test]
    fn last_is_epoch_final_block() {
        let e = FollowerEpocher::new(60);
        assert_eq!(e.last(Epoch::new(0)).unwrap().get(), 60);
        assert_eq!(e.last(Epoch::new(1)).unwrap().get(), 120);
        assert_eq!(e.last(Epoch::new(2)).unwrap().get(), 180);
        // Contiguity: last(E) + 1 == first(E+1) for E >= 1.
        for ep in 1..5u64 {
            assert_eq!(
                e.last(Epoch::new(ep)).unwrap().get() + 1,
                e.first(Epoch::new(ep + 1)).unwrap().get()
            );
        }
    }
}
