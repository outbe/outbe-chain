//! Shared state read by both the application handler and the
//! `FinalizationActor`.
//!
//! [`FinalizationView`] holds the small set of fields that the
//! application's `build_block` path needs (`prev_randao`,
//! `last_timestamp_millis`) plus the canonical view of the last
//! finalized block (`last_finalized_number`, `last_finalized_round`,
//! `forkchoice`). The `FinalizationActor` writes this view; the
//! application reads from it under a short-lived `RwLock::read`
//! guard. After step 21 the application will no longer carry its
//! own copies of these fields.

use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use commonware_consensus::types::Round;
use std::sync::{Arc, RwLock};

/// Snapshot of the canonical finalization-side state shared between
/// the application handler (read-only consumer in `build_block`) and
/// the `FinalizationActor` (sole writer).
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
/// finalization actor takes a write guard while updating;
/// `build_block` and any other reader takes a short-lived read guard
/// so concurrent finalization processing does not block proposal
/// building beyond the lock window.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Multiple concurrent readers.
        let r1 = h.read().unwrap();
        let r2 = h.read().unwrap();
        assert_eq!(r1.last_finalized_number, 7);
        assert_eq!(r2.last_finalized_number, 7);
        drop(r1);
        drop(r2);

        // Writer takes exclusive lock, mutation visible after release.
        {
            let mut w = h.write().unwrap();
            w.last_finalized_number = 8;
            w.prev_randao = B256::with_last_byte(0x02);
        }
        let r = h.read().unwrap();
        assert_eq!(r.last_finalized_number, 8);
        assert_eq!(r.prev_randao, B256::with_last_byte(0x02));
    }
}
