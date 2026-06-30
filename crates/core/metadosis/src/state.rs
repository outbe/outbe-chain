use crate::constants::*;
use crate::errors::MetadosisError;
use crate::precompile::IMetadosis;
use crate::schema::{
    day_type, status, DayType, MetadosisContract, Status, WorldwideDay, WorldwideDayEntryExt,
};
use alloy_primitives::U256;
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_primitives::error::{PrecompileError, Result};

fn checked_hours_to_seconds(hours: u64, label: &'static str) -> Result<u64> {
    hours
        .checked_mul(SECONDS_PER_HOUR)
        .ok_or_else(|| PrecompileError::Revert(format!("metadosis {label} seconds overflow")))
}

fn checked_timestamp_add(base: u64, delta: u64, label: &'static str) -> Result<u64> {
    base.checked_add(delta)
        .ok_or_else(|| PrecompileError::Revert(format!("metadosis {label} timestamp overflow")))
}

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
        let forming_end = checked_timestamp_add(
            forming_start,
            checked_hours_to_seconds(FORMING_PERIOD_HOURS, "forming period")?,
            "forming_end",
        )?;
        let lookback_end = checked_timestamp_add(
            forming_end,
            checked_hours_to_seconds(lookback_delay_hours, "lookback delay")?,
            "lookback_end",
        )?;
        let offering_end = checked_timestamp_add(
            lookback_end,
            checked_hours_to_seconds(offering_period_hours, "offering period")?,
            "offering_end",
        )?;
        let scheduled_process_time = checked_timestamp_add(
            offering_end,
            checked_hours_to_seconds(WAITING_PERIOD_HOURS, "waiting period")?,
            "scheduled_process_time",
        )?;

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

    /// The single low-level writer of a day's `status` field: every status
    /// transition — clock (`worldwideday::persist_status_change`) and settlement
    /// (`mark_wwd_*`) — routes its write here, so the field has one home while the
    /// event/retire policy stays the caller's concern.
    pub(crate) fn write_status(&mut self, wwd: WorldwideDayKey, new: Status) -> Result<()> {
        self.worldwide_days.entry(wwd).status().write(new as u8)
    }

    pub fn get_wwd_status(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).status().read()
    }

    pub(crate) fn get_wwd_status_checked(&self, wwd: WorldwideDayKey) -> Result<Status> {
        Status::try_from(self.get_wwd_status(wwd)?)
    }

    pub(crate) fn set_wwd_day_type(&mut self, wwd: WorldwideDayKey, dtype: DayType) -> Result<()> {
        self.worldwide_days.entry(wwd).day_type().write(dtype as u8)
    }

    pub fn get_wwd_day_type(&self, wwd: WorldwideDayKey) -> Result<u8> {
        self.worldwide_days.entry(wwd).day_type().read()
    }

    /// READY → IN_PROGRESS: the metadosis run begins. Not terminal, so the day
    /// stays in the active set.
    pub fn mark_wwd_in_progress(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let current = self.get_wwd_status_checked(wwd)?;
        if current != Status::Ready {
            return Err(MetadosisError::InvalidTransitionToInProgress {
                wwd,
                current: current as u8,
            }
            .into());
        }
        self.write_status(wwd, Status::InProgress)
    }

    pub fn mark_wwd_completed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let current = self.get_wwd_status_checked(wwd)?;
        if current != Status::InProgress {
            return Err(MetadosisError::InvalidTransitionToCompleted {
                wwd,
                current: current as u8,
            }
            .into());
        }
        self.write_status(wwd, Status::Completed)?;
        self.retire_terminal_wwd(wwd)
    }

    pub fn mark_wwd_failed(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        let current = self.get_wwd_status_checked(wwd)?;
        if current == Status::Completed {
            return Err(MetadosisError::InvalidTransitionToFailed { wwd }.into());
        }
        if current == Status::Failed {
            // Already terminal: idempotent re-fail must not double-enqueue.
            return Ok(());
        }
        self.write_status(wwd, Status::Failed)?;
        self.retire_terminal_wwd(wwd)
    }

    /// Moves a now-terminal day out of the active set and onto the bounded
    /// delete-queue; once the queue exceeds `MAX_RECORDS_KEPT`, pops the oldest
    /// from the front and deletes its record (emitting `WorldwideDayCleanedUp`).
    pub(crate) fn retire_terminal_wwd(&mut self, wwd: WorldwideDayKey) -> Result<()> {
        self.remove_active_wwd(wwd)?;
        self.closed_wwd.push_back(wwd)?;
        // usize -> u64 is a widening, lossless conversion.
        while self.closed_wwd.len()? > MAX_RECORDS_KEPT as u64 {
            let Some(evicted) = self.closed_wwd.pop_front()? else {
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
        if self.worldwide_days.entry(wwd_key).forming_start().read()? == 0 {
            return Err(PrecompileError::Revert(format!(
                "cannot activate missing worldwide day {wwd_key}"
            )));
        }
        match self.get_wwd_status_checked(wwd_key)? {
            Status::Completed | Status::Failed => {
                return Err(PrecompileError::Revert(format!(
                    "cannot activate terminal worldwide day {wwd_key}"
                )));
            }
            _ => {}
        }
        self.active_wwd.insert(wwd_key)?;
        Ok(())
    }

    pub(crate) fn remove_active_wwd(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.active_wwd.remove(&wwd_key)?;
        Ok(())
    }

    pub fn get_active_wwd_by_status(&self, wanted_status: u8) -> Result<Vec<WorldwideDayKey>> {
        let wanted = Status::try_from(wanted_status)?;
        let mut result = Vec::new();
        for wwd in self.active_wwd.read_all()? {
            if self.get_wwd_status_checked(wwd)? == wanted {
                result.push(wwd);
            }
        }
        // Terminal records live in the bounded delete-queue, not active_wwd, so
        // COMPLETED/FAILED status queries must also scan the queue. The two sets
        // are disjoint (active = non-terminal, queue = terminal), so no dedup.
        if wanted == Status::Completed || wanted == Status::Failed {
            for wwd in self.closed_wwd.read_all()? {
                if self.get_wwd_status_checked(wwd)? == wanted {
                    result.push(wwd);
                }
            }
        }
        result.sort_unstable();
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
