//! Work one pass of a right may do in a block. Every right draws from the same limits.

use alloy_primitives::U256;

use crate::error::{PrecompileError, Result};

/// Entities or queue slots read and decided.
pub const SWEEP_VISITS_PER_BLOCK: u32 = 256;
/// EVM state changes: a call, a burned or expired member, a void, a sent notice.
pub const SWEEP_WRITES_PER_BLOCK: u32 = 64;

/// What is left of one pass's work in this block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassBudget {
    visits: u32,
    writes: u32,
    written: bool,
}

/// This structure stores the allowance and the only day selected for a pass at this block height.
/// The packed value contains these fields:
///
/// - block height: 64 bits
/// - calendar day: 32 bits
/// - visits: 32 bits
/// - writes: 32 bits
/// - written flag: 1 bit
/// - initialized flag: 1 bit
///
/// All remaining high bits must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAllowance {
    pub height: u64,
    pub day: u32,
    pub budget: PassBudget,
}

impl BlockAllowance {
    pub fn new(height: u64, day: u32) -> Self {
        Self {
            height,
            day,
            budget: PassBudget::per_block(),
        }
    }

    /// This method resumes the stored allowance. It resets the allowance only at a newer block height.
    /// Pure-deletion queues can use a separate stored allowance with `day = 0`.
    /// Callers must store [`Self::pack`] after processing succeeds within the budget.
    pub fn for_block(value: U256, height: u64, day: u32) -> Result<Self> {
        match Self::unpack(value)? {
            Some(previous) if previous.height > height => Err(PrecompileError::revert(
                "daily sweep block height regressed",
            )),
            Some(previous) if previous.height == height => Ok(previous),
            _ => Ok(Self::new(height, day)),
        }
    }

    pub fn pack(self) -> U256 {
        U256::from(self.height)
            | (U256::from(self.day) << 64)
            | (U256::from(self.budget.visits) << 96)
            | (U256::from(self.budget.writes) << 128)
            | (U256::from(u8::from(self.budget.written)) << 160)
            | (U256::from(1) << 161)
    }

    pub fn unpack(value: U256) -> Result<Option<Self>> {
        if value.is_zero() {
            return Ok(None);
        }
        if value >> 162 != U256::ZERO || (value >> 161) != U256::from(1) {
            return Err(PrecompileError::revert("invalid daily sweep allowance"));
        }
        // Each mask proves the narrowing conversion fits its exact field width.
        // Tests cover the maximum height, the maximum day, and malformed allowances.
        let height = (value & U256::from(u64::MAX)).to::<u64>();
        let day = ((value >> 64usize) & U256::from(u32::MAX)).to::<u32>();
        let visits = ((value >> 96usize) & U256::from(u32::MAX)).to::<u32>();
        let writes = ((value >> 128usize) & U256::from(u32::MAX)).to::<u32>();
        let written = ((value >> 160) & U256::from(1)) != U256::ZERO;
        if visits > SWEEP_VISITS_PER_BLOCK || writes > SWEEP_WRITES_PER_BLOCK {
            return Err(PrecompileError::revert("invalid daily sweep budget limits"));
        }
        Ok(Some(Self {
            height,
            day,
            budget: PassBudget {
                visits,
                writes,
                written,
            },
        }))
    }
}

impl PassBudget {
    pub const fn per_block() -> Self {
        Self::new(SWEEP_VISITS_PER_BLOCK, SWEEP_WRITES_PER_BLOCK)
    }

    pub const fn new(visits: u32, writes: u32) -> Self {
        Self {
            visits,
            writes,
            written: false,
        }
    }

    pub fn visit(&mut self) -> bool {
        take(&mut self.visits, 1)
    }

    pub fn write(&mut self) -> bool {
        self.admit_writes(1)
    }

    /// A member that exhausts gas must not retry through another entrypoint during the same block.
    /// This method sets both counters to zero and disables the first-write exception before the caller stores the allowance.
    pub fn stop(&mut self) {
        self.visits = 0;
        self.writes = 0;
        self.written = true;
    }

    /// Whether a batch of `count` writes that must not split would be taken now.
    pub fn fits_writes(&self, count: u32) -> bool {
        count <= self.writes || !self.written
    }

    /// Takes `count` writes at once. A batch that must not split passes whole when
    /// nothing was written yet, so a batch wider than the budget still gets a block.
    pub fn admit_writes(&mut self, count: u32) -> bool {
        if !self.fits_writes(count) {
            return false;
        }
        self.writes = self.writes.saturating_sub(count);
        self.written |= count != 0;
        true
    }

    /// Whether a walk whose next visit may write has to stop for this block.
    pub fn spent(&self) -> bool {
        self.visits == 0 || !self.fits_writes(1)
    }

    pub const fn visits_left(&self) -> u32 {
        self.visits
    }

    pub const fn writes_left(&self) -> u32 {
        self.writes
    }
}

fn take(left: &mut u32, count: u32) -> bool {
    match left.checked_sub(count) {
        Some(rest) => {
            *left = rest;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_limit_runs_out_on_its_own() {
        let mut budget = PassBudget::new(1, 1);
        assert!(budget.visit());
        assert!(!budget.visit());
        assert!(budget.write());
        assert!(!budget.write());
    }

    #[test]
    fn a_walk_stops_when_visits_or_writes_run_out() {
        assert!(!PassBudget::new(1, 1).spent());
        assert!(PassBudget::new(0, 1).spent());
        let mut budget = PassBudget::new(1, 1);
        assert!(budget.write());
        assert!(budget.spent());
    }

    #[test]
    fn a_wide_batch_passes_whole_only_as_the_first_write() {
        let mut budget = PassBudget::new(0, 4);
        assert!(budget.admit_writes(10));
        assert_eq!(budget.writes_left(), 0);

        let mut budget = PassBudget::new(0, 4);
        assert!(budget.write());
        assert!(!budget.admit_writes(10));
        assert!(budget.admit_writes(3));
        assert_eq!(budget.writes_left(), 0);
    }

    #[test]
    fn the_block_limits_are_shared() {
        let budget = PassBudget::per_block();
        assert_eq!(budget.visits_left(), SWEEP_VISITS_PER_BLOCK);
        assert_eq!(budget.writes_left(), SWEEP_WRITES_PER_BLOCK);
    }

    #[test]
    fn persisted_allowance_preserves_wide_batch_admission_and_boundaries() {
        let mut allowance = BlockAllowance::new(u64::MAX, u32::MAX);
        assert_eq!(
            BlockAllowance::unpack(allowance.pack()).unwrap(),
            Some(allowance)
        );
        assert!(allowance.budget.visit());
        assert!(allowance.budget.admit_writes(100));
        let mut restored = BlockAllowance::unpack(allowance.pack()).unwrap().unwrap();
        assert_eq!(restored, allowance);
        assert!(!restored.budget.admit_writes(100));
        assert_eq!(restored.budget.visits_left(), 255);
        assert_eq!(BlockAllowance::unpack(U256::ZERO).unwrap(), None);
    }

    #[test]
    fn corrupted_packed_allowances_are_rejected() {
        let allowance = BlockAllowance::new(0, 0).pack();
        assert!(BlockAllowance::unpack(U256::from(1)).is_err());
        assert!(BlockAllowance::unpack(allowance | (U256::from(1) << 200)).is_err());
        assert!(BlockAllowance::unpack(allowance | (U256::from(512) << 96)).is_err());
        assert!(BlockAllowance::unpack(allowance | (U256::from(128) << 128)).is_err());
    }

    #[test]
    fn only_a_new_height_replenishes_allowance_or_changes_the_selected_day() {
        let mut allowance = BlockAllowance::new(7, 20260101);
        assert!(allowance.budget.visit());
        assert!(allowance.budget.write());
        let packed = allowance.pack();
        assert_eq!(
            BlockAllowance::for_block(packed, 7, 20260102).unwrap(),
            allowance
        );
        assert_eq!(
            BlockAllowance::for_block(packed, 8, 20260102).unwrap(),
            BlockAllowance::new(8, 20260102)
        );
        assert!(BlockAllowance::for_block(packed, 6, 20260101).is_err());
        assert_eq!(
            BlockAllowance::for_block(U256::ZERO, 0, 0).unwrap(),
            BlockAllowance::new(0, 0)
        );
    }

    #[test]
    fn gas_stop_persists_and_disables_the_first_write_exception() {
        let mut allowance = BlockAllowance::new(7, 20260101);
        allowance.budget.stop();
        let mut resumed = BlockAllowance::for_block(allowance.pack(), 7, 20260101).unwrap();
        assert!(resumed.budget.spent());
        assert!(!resumed.budget.visit());
        assert!(!resumed.budget.admit_writes(100));
        assert!(!BlockAllowance::for_block(allowance.pack(), 8, 20260101)
            .unwrap()
            .budget
            .spent());
    }
}
