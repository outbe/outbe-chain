use crate::digest::Digest;
use alloy_primitives::B256;
use alloy_rpc_types_engine::ForkchoiceState;
use alloy_rpc_types_engine::PayloadId;
use commonware_consensus::types::Height;
use commonware_utils::channel::oneshot;
use outbe_primitives::OutbePayloadAttributes;
use std::time::Duration;
use std::time::SystemTime;
use tracing::info;
use tracing::warn;

/// Immutable forkchoice tracking state.
///
/// Methods return a new `LastCanonicalized` without mutating self.
/// The caller commits by assigning the new value only after a successful FCU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LastCanonicalized {
    pub(super) forkchoice: ForkchoiceState,
    pub(super) head_height: Height,
    pub(super) finalized_height: Height,
}

impl LastCanonicalized {
    pub(super) fn from_recovered(
        genesis_hash: B256,
        finalized_height: u64,
        finalized_hash: B256,
    ) -> Self {
        if finalized_height > 0 {
            info!(
                target: "outbe::executor::recover",
                finalized_height,
                finalized_hash = %finalized_hash,
                "executor state recovered from persisted finalized block"
            );
            Self {
                forkchoice: ForkchoiceState {
                    head_block_hash: finalized_hash,
                    safe_block_hash: finalized_hash,
                    finalized_block_hash: finalized_hash,
                },
                head_height: Height::new(finalized_height),
                finalized_height: Height::new(finalized_height),
            }
        } else {
            info!(
                target: "outbe::executor::recover",
                genesis_hash = %genesis_hash,
                "executor state seeded from genesis (no finalized recovery)"
            );
            Self::new(genesis_hash)
        }
    }

    pub(super) fn new(genesis_hash: B256) -> Self {
        Self {
            forkchoice: ForkchoiceState {
                head_block_hash: genesis_hash,
                safe_block_hash: genesis_hash,
                finalized_block_hash: genesis_hash,
            },
            head_height: Height::zero(),
            finalized_height: Height::zero(),
        }
    }

    /// Returns new state with updated head.
    ///
    /// Rejects head below finalized height, and rejects head at finalized
    /// height with a hash that conflicts with the committed finalized hash.
    /// Allows rollback to the finalized block itself (view-timeout scenario
    /// where Simplex parent rolls back to the last finalized block).
    ///
    /// Pre-finalization head changes are expected on view timeout / leader
    /// rotation; logging the flip-flop and rollback paths here makes those
    /// otherwise-silent canonical-tip transitions auditable from the logs.
    pub(super) fn update_head(self, height: Height, digest: Digest) -> Self {
        let mut this = self;
        if height < this.finalized_height {
            return this;
        }
        if height == this.finalized_height && digest.0 != this.forkchoice.finalized_block_hash {
            crate::metrics::record_executor_head_finalized_conflict();
            warn!(
                target: "outbe::executor::head_conflict",
                %height,
                finalized_hash = %this.forkchoice.finalized_block_hash,
                requested_hash = %digest.0,
                "update_head rejected: conflicting hash at finalized height"
            );
            return this;
        }
        if this.head_height == height && this.forkchoice.head_block_hash != digest.0 {
            crate::metrics::record_executor_head_flip();
            warn!(
                target: "outbe::executor::head_flip",
                %height,
                old_head = %this.forkchoice.head_block_hash,
                new_head = %digest.0,
                finalized_height = %this.finalized_height,
                "canonical head reorged at same height (pre-finalization, expected on view timeout)"
            );
        } else if height < this.head_height {
            crate::metrics::record_executor_head_rollback();
            warn!(
                target: "outbe::executor::head_rollback",
                old_height = %this.head_height,
                old_head = %this.forkchoice.head_block_hash,
                new_height = %height,
                new_head = %digest.0,
                finalized_height = %this.finalized_height,
                "canonical head moved to lower height (pre-finalization parent switch)"
            );
        }
        this.head_height = height;
        this.forkchoice.head_block_hash = digest.0;
        this
    }

    /// Returns new state with updated finalized (and head if needed).
    ///
    /// The strict `>` check enforces protocol invariant
    /// "finalization is monotonic". Stale and conflicting attempts are
    /// silently ignored by the check; we log them so that any future bug
    /// or upstream wire-up regression surfaces immediately instead of
    /// silently dropping a finalize message.
    pub(super) fn update_finalized(self, height: Height, digest: Digest) -> Self {
        let mut this = self;
        if height > this.finalized_height {
            this.finalized_height = height;
            this.forkchoice.safe_block_hash = digest.0;
            this.forkchoice.finalized_block_hash = digest.0;
            if height >= this.head_height {
                this.head_height = height;
                this.forkchoice.head_block_hash = digest.0;
            }
        } else if height == this.finalized_height
            && digest.0 != this.forkchoice.finalized_block_hash
        {
            crate::metrics::record_executor_finalized_conflict();
            tracing::error!(
                target: "outbe::executor::finalized_conflict",
                %height,
                committed = %this.forkchoice.finalized_block_hash,
                attempted = %digest.0,
                "attempted finalized rewrite at same height - protocol invariant violation, ignored"
            );
        } else if height < this.finalized_height {
            crate::metrics::record_executor_finalized_stale();
            warn!(
                target: "outbe::executor::finalized_stale",
                %height,
                committed_height = %this.finalized_height,
                "stale finalized message ignored"
            );
        }
        this
    }
}

pub(super) fn next_deadline(now: SystemTime, interval: Duration) -> SystemTime {
    match now.checked_add(interval) {
        Some(deadline) => deadline,
        None => now,
    }
}

/// Whether to just canonicalize or also build a payload.
#[allow(clippy::large_enum_variant)]
pub(super) enum MaybeBuild {
    JustCanonicalize {
        response: oneshot::Sender<eyre::Result<()>>,
    },
    AlsoBuild {
        attributes: OutbePayloadAttributes,
        response: oneshot::Sender<eyre::Result<PayloadId>>,
    },
}

impl MaybeBuild {
    pub(super) fn attributes(&self) -> Option<&OutbePayloadAttributes> {
        match self {
            Self::JustCanonicalize { .. } => None,
            Self::AlsoBuild { attributes, .. } => Some(attributes),
        }
    }

    pub(super) fn send_error(self, err: eyre::Report) {
        match self {
            Self::JustCanonicalize { response } => {
                let _ = response.send(Err(err));
            }
            Self::AlsoBuild { response, .. } => {
                let _ = response.send(Err(err));
            }
        }
    }
}
