use crate::aggregate::{WwdDayType, WwdStatus};
use crate::constants::*;
use crate::precompile::IMetadosis;
use crate::schema::{
    DayLimitFormationReceiptStateEntryExt, MetadosisContract, WorldwideDay, WorldwideDayEntryExt,
};
use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompDayLimitFormation {
    pub worldwide_day: WorldwideDayKey,
    pub base_limit: U256,
    pub carry_over_before: U256,
    pub carry_over_taken: U256,
    pub carry_over_after: U256,
    pub day_limit: U256,
    pub block_number: u64,
}

/// Durable semantic result owned by Metadosis for one Cycle day-limit slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayLimitFormationReceipt {
    Formed(OcompDayLimitFormation),
}

impl MetadosisContract<'_> {
    pub(crate) fn validate_day_limit_binding(
        &self,
        current: &crate::aggregate::WwdProjection,
        reason: &'static str,
    ) -> Result<()> {
        match self.ocomp_day_limit_formation(current.worldwide_day)? {
            Some(formation) if formation.day_limit == current.metadosis_limit_minor => Ok(()),
            Some(_) => Err(crate::errors::storage_corruption(format!(
                "{reason} formed day limit does not match WWD state"
            ))),
            None if current.metadosis_limit_minor.is_zero() => Ok(()),
            None => Err(crate::errors::storage_corruption(format!(
                "{reason} has a day limit with no formation"
            ))),
        }
    }

    // --- WorldwideDay Management ---

    pub(crate) fn commit_create_worldwide_day(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        wwd: WorldwideDayKey,
        schedule: crate::commit::NewWwdSchedule,
    ) -> Result<()> {
        self.create_worldwide_day_raw(wwd, schedule)
    }

    pub(crate) fn commit_set_wwd_rate_resolution(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        wwd: WorldwideDayKey,
        resolution: crate::commit::WwdRateResolution,
    ) -> Result<()> {
        let day = self.worldwide_days.entry(wwd);
        day.previous_vwap().write(resolution.previous_vwap)?;
        day.current_vwap().write(resolution.current_vwap)?;
        day.day_type().write(resolution.day_type.as_u8())
    }

    pub(crate) fn commit_write_day_limit_formation(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        formation: OcompDayLimitFormation,
    ) -> Result<()> {
        if self
            .day_limit_formation_receipts
            .entry(formation.worldwide_day)
            .formed()
            .read()?
        {
            return Err(crate::errors::caller_rejection(
                "formed OCOMP day limit is immutable",
            ));
        }
        self.worldwide_days
            .entry(formation.worldwide_day)
            .metadosis_limit_minor()
            .write(formation.day_limit)?;
        let receipt = self
            .day_limit_formation_receipts
            .entry(formation.worldwide_day);
        receipt.base_limit_minor().write(formation.base_limit)?;
        receipt
            .promis_limit_before_minor()
            .write(formation.carry_over_before)?;
        receipt
            .promis_limit_taken_minor()
            .write(formation.carry_over_taken)?;
        receipt
            .promis_limit_after_minor()
            .write(formation.carry_over_after)?;
        receipt.metadosis_limit_minor().write(formation.day_limit)?;
        receipt.block_number().write(formation.block_number)
    }

    pub(crate) fn commit_seal_day_limit_formation(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        wwd: WorldwideDayKey,
    ) -> Result<()> {
        self.day_limit_formation_receipts
            .entry(wwd)
            .formed()
            .write(true)
    }

    pub(crate) fn note_worldwide_day_created(&self) -> Result<()> {
        let created = self.worldwide_days_created.read()?;
        let next = created.checked_add(1).ok_or_else(|| {
            crate::errors::storage_corruption(
                "Metadosis Worldwide Day creation count overflow".into(),
            )
        })?;
        self.worldwide_days_created.write(next)
    }

    fn create_worldwide_day_raw(
        &mut self,
        wwd: WorldwideDayKey,
        schedule: crate::commit::NewWwdSchedule,
    ) -> Result<()> {
        let crate::commit::NewWwdSchedule {
            forming_start,
            forming_period_seconds,
            lookback_delay_seconds,
            offering_period_seconds,
            waiting_period_seconds,
        } = schedule;
        let forming_end = forming_start
            .checked_add(forming_period_seconds)
            .ok_or_else(|| crate::errors::caller_rejection("Metadosis forming window overflow"))?;
        let lookback_end = forming_end
            .checked_add(lookback_delay_seconds)
            .ok_or_else(|| crate::errors::caller_rejection("Metadosis lookback window overflow"))?;
        let offering_end = lookback_end
            .checked_add(offering_period_seconds)
            .ok_or_else(|| crate::errors::caller_rejection("Metadosis offering window overflow"))?;
        let scheduled_process_time = offering_end
            .checked_add(waiting_period_seconds)
            .ok_or_else(|| crate::errors::caller_rejection("Metadosis waiting window overflow"))?;

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
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
        })?;
        self.note_worldwide_day_created()
    }

    pub fn ocomp_day_limit_formation(
        &self,
        wwd_key: WorldwideDayKey,
    ) -> Result<Option<OcompDayLimitFormation>> {
        let receipt = self.day_limit_formation_receipts.entry(wwd_key);
        if !receipt.formed().read()? {
            return Ok(None);
        }
        let persisted_day_limit = self
            .worldwide_days
            .entry(wwd_key)
            .metadosis_limit_minor()
            .read()?;
        let base_limit = receipt.base_limit_minor().read()?;
        let carry_over_before = receipt.promis_limit_before_minor().read()?;
        let carry_over_taken = receipt.promis_limit_taken_minor().read()?;
        let carry_over_after = receipt.promis_limit_after_minor().read()?;
        let day_limit = receipt.metadosis_limit_minor().read()?;
        let block_number = receipt.block_number().read()?;
        let valid_arithmetic = base_limit.checked_add(carry_over_taken) == Some(day_limit)
            && carry_over_before.checked_sub(carry_over_taken) == Some(carry_over_after);
        let valid_binding = day_limit == persisted_day_limit && block_number != 0;
        if !valid_arithmetic || !valid_binding {
            return Err(crate::errors::storage_corruption(
                "formed OCOMP day-limit receipt is inconsistent".into(),
            ));
        }
        Ok(Some(OcompDayLimitFormation {
            worldwide_day: wwd_key,
            base_limit,
            carry_over_before,
            carry_over_taken,
            carry_over_after,
            day_limit,
            block_number,
        }))
    }

    pub(crate) fn day_limit_formation_receipt(
        &self,
        wwd_key: WorldwideDayKey,
    ) -> Result<Option<DayLimitFormationReceipt>> {
        self.ocomp_day_limit_formation(wwd_key)
            .map(|formed| formed.map(DayLimitFormationReceipt::Formed))
    }

    pub(crate) fn commit_delete_worldwide_day(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        wwd_key: WorldwideDayKey,
    ) -> Result<()> {
        self.delete_worldwide_day_raw(wwd_key)
    }

    fn delete_worldwide_day_raw(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        (|| {
            // The day's terminal-evidence index dies with the day; without
            // this, retired days would leak index entries forever.
            // `worldwide_days_created` stays. An empty retained index is not
            // evidence that no Worldwide Day was ever created.
            self.delete_terminal_index(wwd_key)?;
            self.worldwide_day_terminal_receipts
                .get_bytes(&wwd_key)
                .clear()?;
            self.worldwide_days.delete(wwd_key)?;
            if self
                .day_limit_formation_receipts
                .entry(wwd_key)
                .formed()
                .read()?
            {
                self.day_limit_formation_receipts.delete(wwd_key)?;
            }
            Ok(())
        })()
    }

    pub fn get_wwd_status(&self, wwd: WorldwideDayKey) -> Result<WwdStatus> {
        WwdStatus::try_from(self.worldwide_days.entry(wwd).status().read()?)
    }

    pub fn get_wwd_day_type(&self, wwd: WorldwideDayKey) -> Result<WwdDayType> {
        WwdDayType::try_from(self.worldwide_days.entry(wwd).day_type().read()?)
    }

    /// Moves a now-terminal day out of the active set and onto the bounded
    /// delete-queue; once the queue exceeds `MAX_RECORDS_KEPT`, pops the oldest
    /// from the front and deletes its record (emitting `WorldwideDayCleanedUp`).
    pub(crate) fn commit_retire_terminal_wwd(
        &mut self,
        permit: &crate::commit::CommitPermit<'_>,
        wwd: WorldwideDayKey,
    ) -> Result<()> {
        self.remove_active_wwd_raw(wwd)?;
        self.closed_wwd.push_back(wwd)?;
        // usize -> u64 is a widening, lossless conversion.
        while self.closed_wwd.len()? > MAX_RECORDS_KEPT as u64 {
            let Some(evicted) = self.closed_wwd.pop_front()? else {
                break;
            };
            let final_status = self.get_wwd_status(evicted)?;
            self.commit_delete_worldwide_day(permit, evicted)?;
            self.emit(IMetadosis::WorldwideDayCleanedUp {
                worldwideDay: evicted.into(),
                finalStatus: final_status.as_u8(),
            })?;
        }
        Ok(())
    }

    // --- Active WWD List ---

    pub(crate) fn commit_add_active_wwd(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
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
            return Err(crate::errors::storage_corruption(format!(
                "Metadosis active WWD cap {MAX_ACTIVE_WWDS} reached before inserting {wwd_key}"
            )));
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
        // Terminal records live in the bounded delete-queue, not active_wwd, so
        // COMPLETED/FAILED status queries must also scan the queue. The two sets
        // are disjoint (active = non-terminal, queue = terminal), so no dedup.
        if wanted_status.is_terminal() {
            for wwd in self.closed_wwd.read_all()? {
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
