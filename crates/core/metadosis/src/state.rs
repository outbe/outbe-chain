use crate::constants::*;
use crate::errors::MetadosisError;
use crate::schema::{day_type, status, MetadosisContract, WorldwideDay, WorldwideDayEntryExt};
use alloy_primitives::U256;
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_primitives::{error::Result, time::timestamp_to_date_key};

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
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
        })
    }

    /// Deletes all stored fields for a worldwide day.
    pub fn delete_worldwide_day(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        self.worldwide_days.delete(wwd)
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

    pub fn set_day_type(&mut self, wwd: WorldwideDayKey, dtype: u8) -> Result<()> {
        self.worldwide_days.entry(wwd).day_type().write(dtype)
    }

    pub fn get_day_type(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).day_type().read()
    }

    pub fn set_vwap(&mut self, wwd: WorldwideDayKey, vwap: U256) -> Result<()> {
        if vwap.is_zero() {
            return Err(MetadosisError::VwapMustBeNonZero.into());
        }
        self.worldwide_days.entry(wwd).current_vwap().write(vwap)
    }

    pub fn get_vwap(&self, wwd: WorldwideDayKey) -> Result<U256> {
        self.worldwide_days.entry(wwd).current_vwap().read()
    }

    pub fn mark_completed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let entry = self.worldwide_days.entry(wwd);
        let current = entry.status().read()?;
        if current != status::READY {
            return Err(MetadosisError::InvalidTransitionToCompleted { wwd, current }.into());
        }
        entry.status().write(status::COMPLETED)
    }

    pub fn mark_failed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let entry = self.worldwide_days.entry(wwd);
        let current = entry.status().read()?;
        if current == status::COMPLETED {
            return Err(MetadosisError::InvalidTransitionToFailed { wwd }.into());
        }
        entry.status().write(status::FAILED)
    }

    // --- Day Metadosis Limit ---

    pub fn record_day_limit(&mut self, date_key: WorldwideDayKey, amount: U256) -> Result<()> {
        let exists = self.day_limit_exists.read(&date_key)?;
        if !exists {
            self.add_day_limit_date(date_key)?;
        }

        let current = self.day_limit_amount.read(&date_key)?;
        self.day_limit_amount.write(&date_key, current + amount)?;
        self.day_limit_exists.write(&date_key, true)?;
        self.cleanup_old_day_limits()
    }

    pub fn record_day_limit_at(&mut self, timestamp: u64, amount: U256) -> Result<()> {
        self.record_day_limit(timestamp_to_date_key(timestamp).into(), amount)
    }

    pub fn get_day_limit(&self, date_key: WorldwideDayKey) -> Result<U256> {
        self.day_limit_amount.read(&date_key)
    }

    pub fn has_day_limit(&self, date_key: WorldwideDayKey) -> Result<bool> {
        self.day_limit_exists.read(&date_key)
    }

    pub fn mark_day_limit_used(&mut self, date_key: WorldwideDayKey) -> Result<()> {
        self.day_limit_used.write(&date_key, true)
    }

    pub fn is_day_limit_used(&self, date_key: WorldwideDayKey) -> Result<bool> {
        self.day_limit_used.read(&date_key)
    }

    pub fn get_all_day_limit_dates(&self) -> Result<Vec<WorldwideDayKey>> {
        let count = self.day_limit_count.read()?;
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            let date = self.day_limit_dates.read(&i)?;
            if date != 0 {
                result.push(date.into());
            }
        }
        Ok(result)
    }

    pub fn delete_day_limit(&mut self, date_key: WorldwideDayKey) -> Result<()> {
        self.day_limit_amount.write(&date_key, U256::ZERO)?;
        self.day_limit_used.write(&date_key, false)?;
        self.day_limit_exists.write(&date_key, false)?;
        self.remove_day_limit_date(date_key)
    }

    fn add_day_limit_date(&mut self, date_key: WorldwideDayKey) -> Result<()> {
        let count = self.day_limit_count.read()?;
        self.day_limit_dates.write(&count, u32::from(date_key))?;
        self.day_limit_count.write(count + 1)
    }

    fn remove_day_limit_date(&mut self, date_key: WorldwideDayKey) -> Result<()> {
        let count = self.day_limit_count.read()?;
        let mut found = None;
        for i in 0..count {
            if self.day_limit_dates.read(&i)? == u32::from(date_key) {
                found = Some(i);
                break;
            }
        }

        if let Some(idx) = found {
            let last = count - 1;
            if idx != last {
                let last_val = self.day_limit_dates.read(&last)?;
                self.day_limit_dates.write(&idx, last_val)?;
            }
            self.day_limit_dates.write(&last, 0)?;
            self.day_limit_count.write(last)?;
        }
        Ok(())
    }

    fn cleanup_old_day_limits(&mut self) -> Result<()> {
        loop {
            let dates = self.get_all_day_limit_dates()?;
            if dates.len() <= MAX_DAY_LIMITS_KEPT {
                return Ok(());
            }
            if let Some(oldest) = dates.iter().min().copied() {
                self.delete_day_limit(oldest)?;
            } else {
                return Ok(());
            }
        }
    }

    // --- Active WWD List ---

    pub fn add_active_wwd(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let count = self.active_wwd_count.read()?;
        self.active_wwds.write(&count, u32::from(wwd))?;
        self.active_wwd_count.write(count + 1)?;
        Ok(())
    }

    pub fn remove_active_wwd(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let count = self.active_wwd_count.read()?;
        let mut found = None;
        for i in 0..count {
            if self.active_wwds.read(&i)? == u32::from(wwd) {
                found = Some(i);
                break;
            }
        }
        if let Some(idx) = found {
            let last = count - 1;
            if idx != last {
                let last_val = self.active_wwds.read(&last)?;
                self.active_wwds.write(&idx, last_val)?;
            }
            self.active_wwds.write(&last, 0)?;
            self.active_wwd_count.write(last)?;
        }
        Ok(())
    }

    pub fn get_all_active_wwds(&self) -> Result<Vec<WorldwideDayKey>> {
        let count = self.active_wwd_count.read()?;
        let mut result = Vec::with_capacity(count as usize);
        for i in 0..count {
            result.push(self.active_wwds.read(&i)?.into());
        }
        Ok(result)
    }

    pub fn get_active_wwds_by_status(&self, wanted_status: u8) -> Result<Vec<WorldwideDayKey>> {
        let mut result = Vec::new();
        for wwd in self.get_all_active_wwds()? {
            if self.get_status(wwd)? == wanted_status {
                result.push(wwd);
            }
        }
        Ok(result)
    }

    pub fn get_status(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).status().read()
    }

    // --- Bootstrap ---

    pub fn set_bootstrap_end_time(&mut self, end_time: u64) -> Result<()> {
        self.bootstrap_end_time.write(end_time)
    }

    pub fn get_bootstrap_end_time(&self) -> Result<u64> {
        self.bootstrap_end_time.read()
    }
}
