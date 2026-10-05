use super::*;

/// Crate-private raw fixture kernel.
///
/// Production mutations must never use this trait. It exists only in test or
/// `test-utils` builds so predecessor state and intentional corruption remain
/// owned by one private module instead of leaking through `MetadosisContract`.
pub(crate) trait FixtureKernelExt {
    #[cfg(test)]
    fn create_worldwide_day(
        &mut self,
        wwd: WorldwideDay,
        forming_start: u64,
        lookback_delay_hours: u64,
        offering_period_hours: u64,
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn delete_worldwide_day(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;

    #[cfg(test)]
    fn set_metadosis_limit(&mut self, wwd: WorldwideDay, amount: U256) -> PrecompileResult<()>;

    #[cfg(test)]
    fn fixture_set_wwd_status(
        &mut self,
        wwd: WorldwideDay,
        status: crate::WwdStatus,
    ) -> PrecompileResult<()>;

    fn fixture_create_ready_day(
        &mut self,
        wwd: WorldwideDay,
        day_limit: U256,
        previous_vwap: U256,
        current_vwap: U256,
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn corrupt_wwd_status_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()>;

    #[cfg(test)]
    fn corrupt_wwd_day_type_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()>;

    #[cfg(test)]
    fn fixture_seed_day_limit_formation(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;

    #[cfg(test)]
    fn fixture_set_scheduled_process_time(
        &mut self,
        wwd: WorldwideDay,
        scheduled_process_time: u64,
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn fixture_write_league_snapshot(
        &mut self,
        wwd: u32,
        owner: Address,
        value: u16,
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn corrupt_ocomp_job_record(
        &mut self,
        intent_id: B256,
        encoded_record: &[u8],
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn fixture_set_wwd_status_from_timestamp(
        &mut self,
        wwd: WorldwideDay,
        block_time: u64,
    ) -> PrecompileResult<crate::WwdStatus>;

    #[cfg(test)]
    fn set_wwd_day_type(
        &mut self,
        wwd: WorldwideDay,
        day_type: crate::WwdDayType,
    ) -> PrecompileResult<()>;

    #[cfg(test)]
    fn set_wwd_vwap(&mut self, wwd: WorldwideDay, vwap: U256) -> PrecompileResult<()>;

    fn add_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;

    #[cfg(test)]
    fn remove_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;
}

impl FixtureKernelExt for MetadosisContract<'_> {
    #[cfg(test)]
    fn create_worldwide_day(
        &mut self,
        wwd: WorldwideDay,
        forming_start: u64,
        lookback_delay_hours: u64,
        offering_period_hours: u64,
    ) -> PrecompileResult<()> {
        let seconds = |hours: u64, label: &str| {
            hours.checked_mul(SECONDS_PER_HOUR).ok_or_else(|| {
                PrecompileError::Revert(format!("Metadosis {label} duration overflow"))
            })
        };
        let forming_end = forming_start
            .checked_add(seconds(FORMING_PERIOD_HOURS, "forming")?)
            .ok_or_else(|| PrecompileError::Revert("Metadosis forming window overflow".into()))?;
        let lookback_end = forming_end
            .checked_add(seconds(lookback_delay_hours, "lookback")?)
            .ok_or_else(|| PrecompileError::Revert("Metadosis lookback window overflow".into()))?;
        let offering_end = lookback_end
            .checked_add(seconds(offering_period_hours, "offering")?)
            .ok_or_else(|| PrecompileError::Revert("Metadosis offering window overflow".into()))?;
        let scheduled_process_time = offering_end
            .checked_add(seconds(WAITING_PERIOD_HOURS, "waiting")?)
            .ok_or_else(|| PrecompileError::Revert("Metadosis waiting window overflow".into()))?;

        self.worldwide_days.create(&WorldwideDayRecord {
            wwd,
            status: crate::WwdStatus::Forming.as_u8(),
            day_type: crate::WwdDayType::Unknown.as_u8(),
            forming_start,
            forming_end,
            lookback_end,
            offering_end,
            scheduled_process_time,
            metadosis_limit_minor: U256::ZERO,
            previous_vwap: U256::ZERO,
            current_vwap: U256::ZERO,
        })
    }

    #[cfg(test)]
    fn delete_worldwide_day(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.worldwide_day_terminal_receipts
                .get_bytes(&wwd)
                .clear()?;
            self.worldwide_days.delete(wwd)?;
            if self
                .day_limit_formation_receipts
                .entry(wwd)
                .formed()
                .read()?
            {
                self.day_limit_formation_receipts.delete(wwd)?;
            }
            Ok(())
        })
    }

    #[cfg(test)]
    fn set_metadosis_limit(&mut self, wwd: WorldwideDay, amount: U256) -> PrecompileResult<()> {
        if self
            .day_limit_formation_receipts
            .entry(wwd)
            .formed()
            .read()?
        {
            return Err(PrecompileError::Revert(
                "formed OCOMP day limit is immutable".into(),
            ));
        }
        self.worldwide_days
            .entry(wwd)
            .metadosis_limit_minor()
            .write(amount)
    }

    #[cfg(test)]
    fn fixture_set_wwd_status(
        &mut self,
        wwd: WorldwideDay,
        status: crate::WwdStatus,
    ) -> PrecompileResult<()> {
        self.worldwide_days
            .entry(wwd)
            .status()
            .write(status.as_u8())
    }

    fn fixture_create_ready_day(
        &mut self,
        wwd: WorldwideDay,
        day_limit: U256,
        previous_vwap: U256,
        current_vwap: U256,
    ) -> PrecompileResult<()> {
        self.worldwide_days.create(&WorldwideDayRecord {
            wwd,
            status: crate::WwdStatus::Ready.as_u8(),
            day_type: crate::WwdDayType::Green.as_u8(),
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_minor: day_limit,
            previous_vwap,
            current_vwap,
        })
    }

    #[cfg(test)]
    fn corrupt_wwd_status_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        self.worldwide_days.entry(wwd).status().write(tag)
    }

    #[cfg(test)]
    fn corrupt_wwd_day_type_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        self.worldwide_days.entry(wwd).day_type().write(tag)
    }

    #[cfg(test)]
    fn fixture_seed_day_limit_formation(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let day_limit = self
            .worldwide_days
            .entry(wwd)
            .metadosis_limit_minor()
            .read()?;
        let receipt = self.day_limit_formation_receipts.entry(wwd);
        receipt.base_limit_minor().write(day_limit)?;
        receipt.promis_limit_before_minor().write(U256::ZERO)?;
        receipt.promis_limit_taken_minor().write(U256::ZERO)?;
        receipt.promis_limit_after_minor().write(U256::ZERO)?;
        receipt.metadosis_limit_minor().write(day_limit)?;
        receipt.block_number().write(1)?;
        receipt.formed().write(true)
    }

    #[cfg(test)]
    fn fixture_set_scheduled_process_time(
        &mut self,
        wwd: WorldwideDay,
        scheduled_process_time: u64,
    ) -> PrecompileResult<()> {
        self.worldwide_days
            .entry(wwd)
            .scheduled_process_time()
            .write(scheduled_process_time)
    }

    #[cfg(test)]
    fn fixture_write_league_snapshot(
        &mut self,
        wwd: u32,
        owner: Address,
        value: u16,
    ) -> PrecompileResult<()> {
        self.ocomp_fidelity_league_snapshot
            .write(&league_snapshot_key(wwd, owner), value)
    }

    #[cfg(test)]
    fn corrupt_ocomp_job_record(
        &mut self,
        intent_id: B256,
        encoded_record: &[u8],
    ) -> PrecompileResult<()> {
        let key = intent_storage_key(intent_id).map_err(|error| {
            PrecompileError::Fatal(format!("fixture OCOMP intent key is invalid: {error}"))
        })?;
        self.ocomp_job_records.get_bytes(&key).write(encoded_record)
    }

    #[cfg(test)]
    fn fixture_set_wwd_status_from_timestamp(
        &mut self,
        wwd: WorldwideDay,
        block_time: u64,
    ) -> PrecompileResult<crate::WwdStatus> {
        let day = self.worldwide_days.entry(wwd);
        let current = crate::WwdStatus::try_from(day.status().read()?)?;
        if current.is_terminal() {
            return Ok(current);
        }
        let next = if block_time < day.forming_end().read()? {
            crate::WwdStatus::Forming
        } else if block_time < day.lookback_end().read()? {
            crate::WwdStatus::LookbackDelay
        } else if block_time < day.offering_end().read()? {
            crate::WwdStatus::Offering
        } else if block_time < day.scheduled_process_time().read()? {
            crate::WwdStatus::Waiting
        } else {
            crate::WwdStatus::Ready
        };
        if next != current {
            day.status().write(next.as_u8())?;
        }
        Ok(next)
    }

    #[cfg(test)]
    fn set_wwd_day_type(
        &mut self,
        wwd: WorldwideDay,
        day_type: crate::WwdDayType,
    ) -> PrecompileResult<()> {
        self.worldwide_days
            .entry(wwd)
            .day_type()
            .write(day_type.as_u8())
    }

    #[cfg(test)]
    fn set_wwd_vwap(&mut self, wwd: WorldwideDay, vwap: U256) -> PrecompileResult<()> {
        if vwap.is_zero() {
            return Err(MetadosisError::VwapMustBeNonZero.into());
        }
        self.worldwide_days.entry(wwd).current_vwap().write(vwap)
    }

    fn add_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let active = self.active_wwd.read_all()?;
        if active.contains(&wwd) {
            return Ok(());
        }
        if active.len() >= MAX_ACTIVE_WWDS {
            return Err(PrecompileError::Fatal(format!(
                "Metadosis active WWD cap {MAX_ACTIVE_WWDS} reached before inserting {wwd}"
            )));
        }
        self.active_wwd.insert(wwd).map(|_| ())
    }

    #[cfg(test)]
    fn remove_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        self.active_wwd.remove(&wwd).map(|_| ())
    }
}
