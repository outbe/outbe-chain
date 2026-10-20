use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

use crate::precompile::ITribute;
use crate::schema::TributeContract;

use super::{
    aggregate::{WwdDayType, WwdStatus},
    commit::{CommitPermit, NewWwdSchedule, WwdRateResolution},
    constants::{MAX_ACTIVE_WWDS, MAX_RECORDS_KEPT},
    errors::{WwdCallerError, WwdStorageCorruptionError},
    schema::{WorldwideDay, WorldwideDayEntryExt},
};

impl TributeContract<'_> {
    // --- WorldwideDay Management ---

    pub(super) fn commit_create_worldwide_day(
        &mut self,
        _permit: &CommitPermit<'_>,
        wwd: WorldwideDayKey,
        schedule: NewWwdSchedule,
    ) -> Result<()> {
        self.create_worldwide_day_raw(wwd, schedule)
    }

    pub(super) fn commit_set_wwd_rate_resolution(
        &mut self,
        _permit: &CommitPermit<'_>,
        wwd: WorldwideDayKey,
        resolution: WwdRateResolution,
    ) -> Result<()> {
        let day = self.worldwide_days.entry(wwd);
        day.previous_vwap_minor()
            .write(resolution.previous_vwap_minor)?;
        day.current_vwap_minor()
            .write(resolution.current_vwap_minor)?;
        day.day_type().write(resolution.day_type.as_u8())
    }

    pub(super) fn commit_set_metadosis_limit(
        &mut self,
        _permit: &CommitPermit<'_>,
        wwd: WorldwideDayKey,
        day_limit: U256,
    ) -> Result<()> {
        self.worldwide_days
            .entry(wwd)
            .metadosis_limit_minor()
            .write(day_limit)
    }

    fn create_worldwide_day_raw(
        &mut self,
        wwd: WorldwideDayKey,
        schedule: NewWwdSchedule,
    ) -> Result<()> {
        let NewWwdSchedule {
            forming_start,
            forming_period_seconds,
            lookback_delay_seconds,
            offering_period_seconds,
            waiting_period_seconds,
        } = schedule;
        let forming_end = forming_start
            .checked_add(forming_period_seconds)
            .ok_or(WwdCallerError::FormingWindowOverflow)?;
        let lookback_end = forming_end
            .checked_add(lookback_delay_seconds)
            .ok_or(WwdCallerError::LookbackWindowOverflow)?;
        let offering_end = lookback_end
            .checked_add(offering_period_seconds)
            .ok_or(WwdCallerError::OfferingWindowOverflow)?;
        let scheduled_process_time = offering_end
            .checked_add(waiting_period_seconds)
            .ok_or(WwdCallerError::WaitingWindowOverflow)?;

        self.worldwide_days.create(&WorldwideDay {
            wwd,
            status: WwdStatus::Forming.as_u8(),
            day_type: WwdDayType::Unknown.as_u8(),
            forming_start,
            forming_end,
            lookback_end,
            offering_end,
            scheduled_process_time,
            metadosis_limit_minor: U256::ZERO,
            previous_vwap_minor: U256::ZERO,
            current_vwap_minor: U256::ZERO,
        })
    }

    pub(super) fn commit_delete_worldwide_day(
        &mut self,
        _permit: &CommitPermit<'_>,
        wwd_key: WorldwideDayKey,
    ) -> Result<()> {
        self.worldwide_days.delete(wwd_key)
    }

    pub fn get_wwd_status(&self, wwd: WorldwideDayKey) -> Result<WwdStatus> {
        WwdStatus::try_from(self.worldwide_days.entry(wwd).status().read()?)
    }

    pub fn get_wwd_day_type(&self, wwd: WorldwideDayKey) -> Result<WwdDayType> {
        WwdDayType::try_from(self.worldwide_days.entry(wwd).day_type().read()?)
    }

    pub fn get_wwd_metadosis_limit(&self, wwd: WorldwideDayKey) -> Result<U256> {
        self.worldwide_days
            .entry(wwd)
            .metadosis_limit_minor()
            .read()
    }

    /// Moves a now-terminal day out of the active set and onto the bounded
    /// delete-queue. When the queue exceeds `MAX_RECORDS_KEPT`, pops the oldest
    /// day from the front and deletes its record (emitting `WorldwideDayCleanedUp`).
    /// Returns the list of evicted days so external orchestrators can clean up
    /// any auxiliary receipts.
    pub(super) fn commit_retire_terminal_wwd(
        &mut self,
        permit: &CommitPermit<'_>,
        wwd: WorldwideDayKey,
    ) -> Result<Vec<WorldwideDayKey>> {
        self.remove_active_wwd_raw(wwd)?;
        self.closed_wwd.push_back(wwd)?;
        let mut evicted_days = Vec::new();
        while self.closed_wwd.len()? > MAX_RECORDS_KEPT as u64 {
            let Some(evicted) = self.closed_wwd.pop_front()? else {
                break;
            };
            let final_status = self.get_wwd_status(evicted)?;
            self.commit_delete_worldwide_day(permit, evicted)?;
            self.emit(ITribute::WorldwideDayCleanedUp {
                worldwideDay: evicted.into(),
                finalStatus: final_status.as_u8(),
            })?;
            evicted_days.push(evicted);
        }
        Ok(evicted_days)
    }

    // --- Active WWD List ---

    pub(super) fn commit_add_active_wwd(
        &mut self,
        _permit: &CommitPermit<'_>,
        wwd_key: WorldwideDayKey,
    ) -> Result<()> {
        self.add_active_wwd_raw(wwd_key)
    }

    fn add_active_wwd_raw(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        let active = self.active_wwd.read_all()?;
        if active.contains(&wwd_key) {
            return Ok(());
        }
        if active.len() >= MAX_ACTIVE_WWDS {
            return Err(WwdStorageCorruptionError::ActiveWwdCapReached {
                max_active_wwds: MAX_ACTIVE_WWDS,
                wwd: wwd_key,
            }
            .into());
        }
        self.active_wwd.insert(wwd_key)?;
        Ok(())
    }

    fn remove_active_wwd_raw(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.active_wwd.remove(&wwd_key)?;
        Ok(())
    }

    pub fn get_active_wwd_by_status(
        &self,
        wanted_status: WwdStatus,
    ) -> Result<Vec<WorldwideDayKey>> {
        let mut result = Vec::new();
        for wwd in self.active_wwd.read_all()? {
            if self.get_wwd_status(wwd)? == wanted_status {
                result.push(wwd);
            }
        }
        if wanted_status.is_terminal() {
            for wwd in self.closed_wwd.read_all()? {
                if self.get_wwd_status(wwd)? == wanted_status {
                    result.push(wwd);
                }
            }
        }
        Ok(result)
    }
}
