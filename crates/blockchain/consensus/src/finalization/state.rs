//! Shared state read by both the application handler and the
//! `FinalizationActor`.
//!
//! [`FinalizationView`] holds the small set of fields that the
//! application's `build_block` path needs (`prev_randao`,
//! `last_timestamp_millis`) plus the canonical view of the last
//! finalized block (`last_finalized_number`, `last_finalized_round`,
//! `forkchoice`).
//!
//! The `FinalizationActor` is the full-state writer (it replaces every
//! field on finalization) and owns the struct directly. The application
//! handler does not reach into the fields or the lock: it goes through the
//! [`FinalizationViewAccess`] seam, which exposes the narrow set of reads it
//! needs plus the monotonic `advance_timestamp_floor` write. The handle uses
//! `parking_lot::RwLock`, which has no poison semantics, so the critical
//! sections never surface a `PoisonError` and callers never `unwrap` a guard.
//!
//! Critical-section invariant: code holding a write guard must stay
//! infallible (no `?`, no `unwrap`, no panic between field assignments) so a
//! reader always observes a consistent snapshot.

use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use commonware_consensus::types::Round;
use parking_lot::RwLock;
use std::sync::Arc;

/// Canonical finalization-side state shared between the `FinalizationActor`
/// (full-state writer) and the application handler, which reads it and
/// advances only the monotonic `last_timestamp_millis` floor — both through
/// the [`FinalizationViewAccess`] seam.
#[derive(Clone, Debug)]
pub struct FinalizationView {
    /// Forkchoice state derived from the last finalized block.
    pub forkchoice: ForkchoiceState,

    /// Last finalized block number.
    pub last_finalized_number: u64,

    /// Last finalized consensus round processed by the actor.
    pub last_finalized_round: Option<Round>,

    /// VRF seed from the last finalized block's BLS threshold
    /// signature. Read by the application's `build_block` to set
    /// `header.prev_randao`.
    pub prev_randao: B256,

    /// Monotonic clock floor for block building. Updated when the
    /// finalization actor sees a block with a later timestamp; read
    /// by `build_block` to guarantee the new block's timestamp is
    /// strictly greater than the previous one.
    pub last_timestamp_millis: u64,
}

impl FinalizationView {
    /// Construct an initial view from the recovered finalized block at
    /// startup. The application handler will see this as soon as it is
    /// constructed.
    pub fn from_recovered(
        recovered_finalized_hash: B256,
        recovered_finalized_number: u64,
        recovered_finalized_round: Option<Round>,
    ) -> Self {
        let forkchoice = if recovered_finalized_number > 0 {
            ForkchoiceState {
                head_block_hash: recovered_finalized_hash,
                safe_block_hash: recovered_finalized_hash,
                finalized_block_hash: recovered_finalized_hash,
            }
        } else {
            ForkchoiceState {
                head_block_hash: B256::ZERO,
                safe_block_hash: B256::ZERO,
                finalized_block_hash: B256::ZERO,
            }
        };
        Self {
            forkchoice,
            last_finalized_number: recovered_finalized_number,
            last_finalized_round: recovered_finalized_round,
            prev_randao: B256::ZERO,
            last_timestamp_millis: 0,
        }
    }
}

/// Shared, thread-safe handle to the [`FinalizationView`]. The
/// finalization actor takes a write guard while updating; readers take a
/// short-lived read guard so concurrent finalization processing does not
/// block proposal building beyond the lock window. The application handler
/// never touches this handle directly — it uses [`FinalizationViewAccess`].
pub type FinalizationViewHandle = Arc<RwLock<FinalizationView>>;

/// Constructs a fresh `FinalizationViewHandle` from recovered state.
pub fn new_finalization_view(
    recovered_finalized_hash: B256,
    recovered_finalized_number: u64,
    recovered_finalized_round: Option<Round>,
) -> FinalizationViewHandle {
    Arc::new(RwLock::new(FinalizationView::from_recovered(
        recovered_finalized_hash,
        recovered_finalized_number,
        recovered_finalized_round,
    )))
}

/// Consistent snapshot of the last finalized block, returned by
/// [`FinalizationViewAccess::finalized_anchor`]. Bundling the three fields in
/// one snapshot keeps the read atomic under a single guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedAnchor {
    pub number: u64,
    pub finalized_head_hash: B256,
    pub round: Option<Round>,
}

/// Narrow read+write seam over [`FinalizationViewHandle`] for the application
/// handler. It hides the lock and the struct fields: the handler asks for the
/// snapshots it needs and advances the monotonic timestamp floor, without ever
/// taking a raw guard. The `FinalizationActor` (full-state writer) and tests
/// keep direct field access — this trait is the consumer-defined interface for
/// the handler only.
pub trait FinalizationViewAccess {
    /// Consistent snapshot of the last finalized block (number, finalized head
    /// hash, finalized round).
    fn finalized_anchor(&self) -> FinalizedAnchor;

    /// Current monotonic block-timestamp floor (`last_timestamp_millis`).
    fn timestamp_floor(&self) -> u64;

    /// Raise the timestamp floor to at least `candidate_millis` and return the
    /// resulting floor. Monotonic: never lowers the stored value.
    fn advance_timestamp_floor(&self, candidate_millis: u64) -> u64;

    /// Raise the timestamp floor to at least `candidate_millis` and return the
    /// current `prev_randao`, both under a single write guard so the floor
    /// advance and the seed read observe one consistent state.
    fn advance_floor_and_read_prev_randao(&self, candidate_millis: u64) -> B256;
}

impl FinalizationViewAccess for FinalizationViewHandle {
    fn finalized_anchor(&self) -> FinalizedAnchor {
        let view = self.read();
        FinalizedAnchor {
            number: view.last_finalized_number,
            finalized_head_hash: view.forkchoice.finalized_block_hash,
            round: view.last_finalized_round,
        }
    }

    fn timestamp_floor(&self) -> u64 {
        self.read().last_timestamp_millis
    }

    fn advance_timestamp_floor(&self, candidate_millis: u64) -> u64 {
        let mut view = self.write();
        view.last_timestamp_millis = std::cmp::max(view.last_timestamp_millis, candidate_millis);
        view.last_timestamp_millis
    }

    fn advance_floor_and_read_prev_randao(&self, candidate_millis: u64) -> B256 {
        let mut view = self.write();
        view.last_timestamp_millis = std::cmp::max(view.last_timestamp_millis, candidate_millis);
        view.prev_randao
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_consensus::types::{Epoch, View};

    #[test]
    fn from_recovered_zero_block_number_yields_zero_forkchoice() {
        let view = FinalizationView::from_recovered(B256::with_last_byte(0xAB), 0, None);
        assert_eq!(view.forkchoice.head_block_hash, B256::ZERO);
        assert_eq!(view.forkchoice.safe_block_hash, B256::ZERO);
        assert_eq!(view.forkchoice.finalized_block_hash, B256::ZERO);
        assert_eq!(view.last_finalized_number, 0);
        assert_eq!(view.last_finalized_round, None);
        assert_eq!(view.prev_randao, B256::ZERO);
        assert_eq!(view.last_timestamp_millis, 0);
    }

    #[test]
    fn from_recovered_nonzero_block_seeds_forkchoice_with_hash() {
        let h = B256::with_last_byte(0xCD);
        let view = FinalizationView::from_recovered(h, 42, None);
        assert_eq!(view.forkchoice.head_block_hash, h);
        assert_eq!(view.forkchoice.safe_block_hash, h);
        assert_eq!(view.forkchoice.finalized_block_hash, h);
        assert_eq!(view.last_finalized_number, 42);
    }

    #[test]
    fn handle_supports_concurrent_read_and_serialised_write() {
        let h = new_finalization_view(B256::with_last_byte(0x01), 7, None);
        // Multiple concurrent readers (parking_lot guards, no poison).
        let r1 = h.read();
        let r2 = h.read();
        assert_eq!(r1.last_finalized_number, 7);
        assert_eq!(r2.last_finalized_number, 7);
        drop(r1);
        drop(r2);

        // Writer takes exclusive lock, mutation visible after release.
        {
            let mut w = h.write();
            w.last_finalized_number = 8;
            w.prev_randao = B256::with_last_byte(0x02);
        }
        let r = h.read();
        assert_eq!(r.last_finalized_number, 8);
        assert_eq!(r.prev_randao, B256::with_last_byte(0x02));
    }

    #[test]
    fn finalized_anchor_returns_consistent_snapshot() {
        let h = new_finalization_view(
            B256::with_last_byte(0x09),
            11,
            Some(Round::new(Epoch::new(2), View::new(5))),
        );
        let anchor = h.finalized_anchor();
        assert_eq!(anchor.number, 11);
        assert_eq!(anchor.finalized_head_hash, B256::with_last_byte(0x09));
        assert_eq!(anchor.round, Some(Round::new(Epoch::new(2), View::new(5))));
    }

    #[test]
    fn advance_timestamp_floor_is_monotonic() {
        let h = new_finalization_view(B256::ZERO, 0, None);
        assert_eq!(h.timestamp_floor(), 0);
        assert_eq!(h.advance_timestamp_floor(100), 100);
        // Lower candidate does not lower the floor.
        assert_eq!(h.advance_timestamp_floor(50), 100);
        assert_eq!(h.advance_timestamp_floor(150), 150);
        assert_eq!(h.timestamp_floor(), 150);
    }

    #[test]
    fn advance_floor_and_read_prev_randao_advances_and_returns_seed() {
        let h = new_finalization_view(B256::ZERO, 0, None);
        {
            let mut w = h.write();
            w.prev_randao = B256::with_last_byte(0x42);
            w.last_timestamp_millis = 200;
        }
        // Lower candidate keeps the floor; returns the current seed.
        assert_eq!(
            h.advance_floor_and_read_prev_randao(100),
            B256::with_last_byte(0x42)
        );
        assert_eq!(h.timestamp_floor(), 200);
        // Higher candidate advances the floor.
        assert_eq!(
            h.advance_floor_and_read_prev_randao(300),
            B256::with_last_byte(0x42)
        );
        assert_eq!(h.timestamp_floor(), 300);
    }
}
