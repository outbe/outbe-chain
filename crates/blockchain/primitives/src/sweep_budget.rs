//! Work one sweep of a right may do in a block. Every right draws from the same limits.

/// Entities or queue slots read and decided.
pub const SWEEP_VISITS_PER_BLOCK: u32 = 256;
/// EVM state changes: a call, a burned or expired member, a void, a sent notice.
pub const SWEEP_WRITES_PER_BLOCK: u32 = 64;
/// Compressed-entity body removals, which also cost end-of-block processing.
pub const SWEEP_BODY_WRITES_PER_BLOCK: u32 = 8;

/// What is left of one sweep's work in this block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepBudget {
    visits: u32,
    writes: u32,
    body_writes: u32,
    written: bool,
}

impl SweepBudget {
    pub const fn per_block() -> Self {
        Self::new(
            SWEEP_VISITS_PER_BLOCK,
            SWEEP_WRITES_PER_BLOCK,
            SWEEP_BODY_WRITES_PER_BLOCK,
        )
    }

    pub const fn new(visits: u32, writes: u32, body_writes: u32) -> Self {
        Self {
            visits,
            writes,
            body_writes,
            written: false,
        }
    }

    pub fn visit(&mut self) -> bool {
        take(&mut self.visits, 1)
    }

    pub fn write(&mut self) -> bool {
        self.admit_writes(1)
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

    pub fn body_write(&mut self) -> bool {
        take(&mut self.body_writes, 1)
    }

    pub const fn visits_left(&self) -> u32 {
        self.visits
    }

    pub const fn writes_left(&self) -> u32 {
        self.writes
    }

    pub const fn body_writes_left(&self) -> u32 {
        self.body_writes
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
        let mut budget = SweepBudget::new(1, 1, 1);
        assert!(budget.visit());
        assert!(!budget.visit());
        assert!(budget.write());
        assert!(!budget.write());
        assert!(budget.body_write());
        assert!(!budget.body_write());
    }

    #[test]
    fn a_walk_stops_when_visits_or_writes_run_out() {
        assert!(!SweepBudget::new(1, 1, 0).spent());
        assert!(SweepBudget::new(0, 1, 0).spent());
        let mut budget = SweepBudget::new(1, 1, 0);
        assert!(budget.write());
        assert!(budget.spent());
    }

    #[test]
    fn a_wide_batch_passes_whole_only_as_the_first_write() {
        let mut budget = SweepBudget::new(0, 4, 0);
        assert!(budget.admit_writes(10));
        assert_eq!(budget.writes_left(), 0);

        let mut budget = SweepBudget::new(0, 4, 0);
        assert!(budget.write());
        assert!(!budget.admit_writes(10));
        assert!(budget.admit_writes(3));
        assert_eq!(budget.writes_left(), 0);
    }

    #[test]
    fn the_block_limits_are_shared() {
        let budget = SweepBudget::per_block();
        assert_eq!(budget.visits_left(), SWEEP_VISITS_PER_BLOCK);
        assert_eq!(budget.writes_left(), SWEEP_WRITES_PER_BLOCK);
        assert_eq!(budget.body_writes_left(), SWEEP_BODY_WRITES_PER_BLOCK);
    }
}
