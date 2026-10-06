//! Transport-size admission for pool transactions during payload building.
//!
//! A sealed block must fit one consensus P2P message, so the builder rejects a
//! candidate transaction whose inclusion could push the final block over the
//! protocol cap. The builder takes the estimate before it finalizes the header,
//! so the estimate must bound every byte that finalization can still add.

use outbe_primitives::consensus::{OUTBE_MAX_BLOCK_SIZE, OUTBE_MAX_EXTRA_DATA_SIZE};
use reth_consensus_common::validation::MAX_RLP_BLOCK_SIZE;

/// Bytes reserved for every header field except `extra_data`, plus the list
/// framing of the block and its transaction and ommer lists. The widest-header
/// test in `tests/size_budget.rs` pins this bound.
const HEADER_ALLOWANCE: usize = 1024;

/// Encoded size of the largest `extra_data` the protocol accepts. The final
/// header artifacts are written after pool selection, so selection reserves
/// the protocol maximum instead of the pre-final artifacts it can see.
fn max_encoded_extra_data() -> usize {
    alloy_rlp::Header {
        list: false,
        payload_length: OUTBE_MAX_EXTRA_DATA_SIZE,
    }
    .length()
        + OUTBE_MAX_EXTRA_DATA_SIZE
}

/// A size limit that a candidate transaction would exceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SizeRejection {
    pub(super) size: usize,
    pub(super) limit: usize,
}

/// Running body size plus the fixed bytes that the sealed block will carry.
#[derive(Debug, Clone)]
pub(super) struct BlockSizeBudget {
    transactions: usize,
    reserved: usize,
    osaka: bool,
}

impl BlockSizeBudget {
    /// Starts an empty budget. `reserved_end_rlp_length` covers the end-zone
    /// system transactions appended after pool selection.
    pub(super) fn new(
        reserved_end_rlp_length: usize,
        withdrawals_rlp_length: usize,
        osaka: bool,
    ) -> Self {
        let reserved = reserved_end_rlp_length
            .saturating_add(withdrawals_rlp_length)
            .saturating_add(HEADER_ALLOWANCE)
            .saturating_add(max_encoded_extra_data());
        Self {
            transactions: 0,
            reserved,
            osaka,
        }
    }

    /// Accounts for a transaction that is now part of the block body.
    pub(super) fn record(&mut self, tx_rlp_length: usize) {
        // Saturation keeps an impossible overflow on the rejecting side: every
        // later candidate is then over the limit.
        self.transactions = self.transactions.saturating_add(tx_rlp_length);
    }

    /// Upper bound on the sealed block RLP length if the candidate is included.
    /// `None` means the bound does not fit in `usize`.
    pub(super) fn estimate(&self, tx_rlp_length: usize) -> Option<usize> {
        self.transactions
            .checked_add(tx_rlp_length)?
            .checked_add(self.reserved)
    }

    /// Checks whether a candidate still fits every active size limit.
    pub(super) fn admit(&self, tx_rlp_length: usize) -> Result<(), SizeRejection> {
        let size = self.estimate(tx_rlp_length).unwrap_or(usize::MAX);
        if self.osaka && size > MAX_RLP_BLOCK_SIZE {
            return Err(SizeRejection {
                size,
                limit: MAX_RLP_BLOCK_SIZE,
            });
        }
        // Outbe transport cap (always on, independent of the Osaka fork): a
        // block must fit one consensus P2P message. Skip txs that would push
        // the block over `OUTBE_MAX_BLOCK_SIZE` so the proposer never builds
        // a block validators would reject as undisseminable.
        if size > OUTBE_MAX_BLOCK_SIZE {
            return Err(SizeRejection {
                size,
                limit: OUTBE_MAX_BLOCK_SIZE,
            });
        }
        Ok(())
    }
}
