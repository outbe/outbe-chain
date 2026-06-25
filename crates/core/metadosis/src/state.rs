use crate::constants::*;
use crate::errors::MetadosisError;
use crate::precompile::IMetadosis;
use crate::schema::{day_type, status, MetadosisContract, WorldwideDay, WorldwideDayEntryExt};
use alloy_primitives::U256;
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_primitives::error::Result;

impl MetadosisContract<'_> {
    // --- WorldwideDay Management ---

    /// Creates a new worldwide day entry.
    pub fn create_worldwide_day(
        &mut self,
        wwd: WorldwideDayKey,
        forming_start: u64,
        lookback_delay_hours: u64,
        offering_period_hours: u64,
    ) -> Result<()> {
        let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
        let lookback_end = forming_end + lookback_delay_hours * SECONDS_PER_HOUR;
        let offering_end = lookback_end + offering_period_hours * SECONDS_PER_HOUR;
        let scheduled_process_time = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;

        self.worldwide_days.create(&WorldwideDay {
            wwd,
            status: status::FORMING,
            day_type: day_type::UNKNOWN,
            forming_start,
            forming_end,
            lookback_end,
            offering_end,
            scheduled_process_time,
            metadosis_limit_amount: U256::ZERO,
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
        })
    }

    pub fn set_metadosis_limit(&mut self, wwd_key: WorldwideDayKey, amount: U256) -> Result<()> {
        self.worldwide_days
            .entry(wwd_key)
            .metadosis_limit_amount()
            .write(amount)?;
        Ok(())
    }

    /// Deletes all stored fields for a worldwide day.
    pub fn delete_worldwide_day(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.worldwide_days.delete(wwd_key)
    }

    /// Updates worldwide day status based on block time.
    /// Returns the new status.
    pub fn update_wwd_status(&mut self, wwd: WorldwideDayKey, block_time: u64) -> Result<u8> {
        let day = self.worldwide_days.entry(wwd);
        let current_status = day.status().read()?;

        if current_status == status::COMPLETED || current_status == status::FAILED {
            return Ok(current_status);
        }

        let forming_end = day.forming_end().read()?;
        let lookback_end = day.lookback_end().read()?;
        let offering_end = day.offering_end().read()?;
        let scheduled = day.scheduled_process_time().read()?;

        let new_status = if block_time < forming_end {
            status::FORMING
        } else if block_time < lookback_end {
            status::LOOKBACK_DELAY
        } else if block_time < offering_end {
            status::OFFERING
        } else if block_time < scheduled {
            status::WAITING
        } else {
            status::READY
        };

        if new_status != current_status {
            day.status().write(new_status)?;
        }

        Ok(new_status)
    }

    pub fn get_wwd_status(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).status().read()
    }

    pub fn set_wwd_day_type(&mut self, wwd: WorldwideDayKey, dtype: u8) -> Result<()> {
        self.worldwide_days.entry(wwd).day_type().write(dtype)
    }

    pub fn get_wwd_day_type(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).day_type().read()
    }

    pub fn set_wwd_vwap(&mut self, wwd: WorldwideDayKey, vwap: U256) -> Result<()> {
        if vwap.is_zero() {
            return Err(MetadosisError::VwapMustBeNonZero.into());
        }
        self.worldwide_days.entry(wwd).current_vwap().write(vwap)
    }

    pub fn get_wwd_vwap(&self, wwd: WorldwideDayKey) -> Result<U256> {
        self.worldwide_days.entry(wwd).current_vwap().read()
    }

    pub fn mark_wwd_completed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let current = self.get_wwd_status(wwd)?;
        if current != status::READY {
            return Err(MetadosisError::InvalidTransitionToCompleted { wwd, current }.into());
        }
        self.worldwide_days
            .entry(wwd)
            .status()
            .write(status::COMPLETED)?;
        self.retire_terminal_wwd(wwd)
    }

    pub fn mark_wwd_failed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let current = self.get_wwd_status(wwd)?;
        if current == status::COMPLETED {
            return Err(MetadosisError::InvalidTransitionToFailed { wwd }.into());
        }
        if current == status::FAILED {
            // Already terminal: idempotent re-fail must not double-enqueue.
            return Ok(());
        }
        self.worldwide_days
            .entry(wwd)
            .status()
            .write(status::FAILED)?;
        self.retire_terminal_wwd(wwd)
    }

    /// Moves a now-terminal day out of the active set and onto the bounded
    /// delete-queue; once the queue exceeds `MAX_RECORDS_KEPT`, pops the oldest
    /// from the front and deletes its record (emitting `WorldwideDayCleanedUp`).
    fn retire_terminal_wwd(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        self.remove_active_wwd(wwd)?;
        self.closed_worldwidedays.push_back(wwd)?;
        // usize -> u64 is a widening, lossless conversion.
        while self.closed_worldwidedays.len()? > MAX_RECORDS_KEPT as u64 {
            let Some(evicted) = self.closed_worldwidedays.pop_front()? else {
                break;
            };
            let final_status = self.get_wwd_status(evicted)?;
            self.delete_worldwide_day(evicted)?;
            self.emit(IMetadosis::WorldwideDayCleanedUp {
                worldwideDay: evicted.into(),
                finalStatus: final_status,
            })?;
        }
        Ok(())
    }

    // --- Active WWD List ---

    pub fn add_active_wwd(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.active_wwd.insert(wwd_key)?;
        Ok(())
    }

    pub fn remove_active_wwd(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.active_wwd.remove(&wwd_key)?;
        Ok(())
    }

    pub fn get_active_wwd_by_status(&self, wanted_status: u8) -> Result<Vec<WorldwideDayKey>> {
        let mut result = Vec::new();
        for wwd in self.active_wwd.read_all()? {
            if self.get_wwd_status(wwd)? == wanted_status {
                result.push(wwd);
            }
        }
        // Terminal records live in the bounded delete-queue, not active_wwd, so
        // COMPLETED/FAILED status queries must also scan the queue. The two sets
        // are disjoint (active = non-terminal, queue = terminal), so no dedup.
        if wanted_status == status::COMPLETED || wanted_status == status::FAILED {
            for wwd in self.closed_worldwidedays.read_all()? {
                if self.get_wwd_status(wwd)? == wanted_status {
                    result.push(wwd);
                }
            }
        }
        Ok(result)
    }

    // --- Bootstrap ---

    pub fn set_bootstrap_end_time(&mut self, end_time: u64) -> Result<()> {
        self.bootstrap_end_time.write(end_time)
    }

    pub fn get_bootstrap_end_time(&self) -> Result<u64> {
        self.bootstrap_end_time.read()
    }
}
