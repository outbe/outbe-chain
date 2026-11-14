//! One-slot Phase 1 accounting progress at `ACCOUNTING_PROGRESS_ADDRESS`.

use alloy_sol_types::SolError;
use outbe_macros::contract;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::types::Slot;
use outbe_system_common::addresses::ACCOUNTING_PROGRESS_ADDRESS;
use thiserror::Error;

alloy_sol_types::sol! {
    error BlockNumberRegression(uint64 current, uint64 attempted);
    error ZeroAccountingProgress();
    error AccountingProgressGap(uint64 current, uint64 attempted);
}

/// Failures of the exact-successor Phase 1 progress write.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AccountingProgressError {
    /// The caller passed block number 0.
    #[error("accounting progress input is zero")]
    ZeroBlockNumber,
    /// The caller passed a height below the stored height.
    #[error("last_accounted_block_number regression: current={current}, attempted={attempted}")]
    BlockNumberRegression {
        /// Stored height.
        current: u64,
        /// Rejected height.
        attempted: u64,
    },
    /// The caller skipped at least one height.
    #[error("accounting progress is not the next block: current={current}, attempted={attempted}")]
    BlockNumberGap {
        /// Stored height.
        current: u64,
        /// Rejected height.
        attempted: u64,
    },
}

impl From<AccountingProgressError> for PrecompileError {
    fn from(error: AccountingProgressError) -> Self {
        let bytes = match error {
            AccountingProgressError::ZeroBlockNumber => ZeroAccountingProgress {}.abi_encode(),
            AccountingProgressError::BlockNumberRegression { current, attempted } => {
                BlockNumberRegression { current, attempted }.abi_encode()
            }
            AccountingProgressError::BlockNumberGap { current, attempted } => {
                AccountingProgressGap { current, attempted }.abi_encode()
            }
        };
        PrecompileError::RevertBytes(bytes.into())
    }
}

/// V2 Phase 1 accounting-progress storage facade (`ACCOUNTING_PROGRESS_ADDRESS`).
///
/// Slot 0 is `last_accounted_block_number`. Slots 1 through 15 stay reserved.
#[contract(addr = ACCOUNTING_PROGRESS_ADDRESS)]
pub struct Accounting {
    /// Highest exact-parent block number whose Phase 1 system tx has committed.
    ///
    /// `0` means no Phase 1 has committed on this chain.
    pub last_accounted_block_number: Slot<u64>,
}

/// Records exact Phase 1 progress for `block_number`.
///
/// Zero returns [`AccountingProgressError::ZeroBlockNumber`] and does not read
/// storage. A lower height returns [`AccountingProgressError::BlockNumberRegression`].
/// A height equal to the stored height returns `Ok` and does not write. The only
/// write is the successor `current + 1`, including the first commit `0 -> 1`.
/// Any other forward height returns [`AccountingProgressError::BlockNumberGap`].
pub fn record_phase1_progress(ctx: &BlockRuntimeContext<'_>, block_number: u64) -> Result<()> {
    if block_number == 0 {
        return Err(AccountingProgressError::ZeroBlockNumber.into());
    }
    let current = read_last_accounted_block_number(ctx)?;
    if block_number < current {
        return Err(AccountingProgressError::BlockNumberRegression {
            current,
            attempted: block_number,
        }
        .into());
    }
    if block_number == current {
        return Ok(());
    }
    if current.checked_add(1) != Some(block_number) {
        return Err(AccountingProgressError::BlockNumberGap {
            current,
            attempted: block_number,
        }
        .into());
    }
    let accounting: Accounting<'_> = ctx.storage.contract::<Accounting<'_>>();
    accounting.last_accounted_block_number.write(block_number)
}

/// Reads `last_accounted_block_number`. A fresh chain returns `0`.
pub fn read_last_accounted_block_number(ctx: &BlockRuntimeContext<'_>) -> Result<u64> {
    let accounting: Accounting<'_> = ctx.storage.contract::<Accounting<'_>>();
    accounting.last_accounted_block_number.read()
}
