//! Per-validator success, abstain and miss counters of the slash window.

use alloy_primitives::Address;
use outbe_primitives::error::Result;

use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// Increments success counter for a validator.
    pub fn increment_success(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_success_count.read(validator)?;
        self.penalty_success_count.write(validator, c + 1)
    }

    /// Increments abstain counter for a validator.
    pub fn increment_abstain(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_abstain_count.read(validator)?;
        self.penalty_abstain_count.write(validator, c + 1)
    }

    /// Increments miss counter for a validator.
    pub fn increment_miss(&mut self, validator: &Address) -> Result<()> {
        let c = self.penalty_miss_count.read(validator)?;
        self.penalty_miss_count.write(validator, c + 1)
    }

    /// Resets all penalty counters for a validator.
    pub fn reset_penalty_counter(&mut self, validator: &Address) -> Result<()> {
        self.penalty_success_count.write(validator, 0)?;
        self.penalty_abstain_count.write(validator, 0)?;
        self.penalty_miss_count.write(validator, 0)?;
        Ok(())
    }

    /// Returns slash window progress for a validator.
    ///
    /// Returns `(success, abstain, miss, slash_window)`.
    pub fn get_slash_window_progress(&self, validator: &Address) -> Result<(u64, u64, u64, u64)> {
        let success = self.penalty_success_count.read(validator)?;
        let abstain = self.penalty_abstain_count.read(validator)?;
        let miss = self.penalty_miss_count.read(validator)?;
        let slash_window = self.config_slash_window.read()?;
        Ok((success, abstain, miss, slash_window))
    }
}
