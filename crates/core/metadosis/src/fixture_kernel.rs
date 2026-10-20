#[cfg(test)]
use crate::schema::DayLimitFormationReceiptStateEntryExt;
use crate::schema::MetadosisContract;
use alloy_primitives::U256;
use outbe_primitives::{error::Result as PrecompileResult, time::WorldwideDay};
use outbe_tribute::{worldwideday::fixture_kernel::WwdFixtureKernelExt, TributeContract};

/// Crate-private raw fixture kernel.
///
/// Production mutations must never use this trait. It exists only in test or
/// `test-utils` builds. This way, one private module owns predecessor state and
/// intentional corruption, and they do not leak through `MetadosisContract`.
#[allow(dead_code)]
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
        previous_vwap_minor: U256,
        current_vwap_minor: U256,
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
    fn set_wwd_day_type(
        &mut self,
        wwd: WorldwideDay,
        day_type: crate::WwdDayType,
    ) -> PrecompileResult<()>;

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
        TributeContract::new(self.storage.clone()).create_worldwide_day(
            wwd,
            forming_start,
            lookback_delay_hours,
            offering_period_hours,
        )
    }

    #[cfg(test)]
    fn delete_worldwide_day(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.delete_wwd_receipts_raw(wwd)?;
            TributeContract::new(storage.clone()).delete_worldwide_day_record(wwd)?;
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
            return Err(crate::errors::MetadosisCallerError::FormedDayLimitImmutable.into());
        }
        TributeContract::new(self.storage.clone()).set_wwd_metadosis_limit_raw(wwd, amount)
    }

    #[cfg(test)]
    fn fixture_set_wwd_status(
        &mut self,
        wwd: WorldwideDay,
        status: crate::WwdStatus,
    ) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).fixture_set_wwd_status(wwd, status)
    }

    fn fixture_create_ready_day(
        &mut self,
        wwd: WorldwideDay,
        day_limit: U256,
        previous_vwap_minor: U256,
        current_vwap_minor: U256,
    ) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).fixture_create_ready_day(
            wwd,
            day_limit,
            previous_vwap_minor,
            current_vwap_minor,
        )
    }

    #[cfg(test)]
    fn corrupt_wwd_status_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).corrupt_wwd_status_tag(wwd, tag)
    }

    #[cfg(test)]
    fn corrupt_wwd_day_type_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).corrupt_wwd_day_type_tag(wwd, tag)
    }

    #[cfg(test)]
    fn fixture_seed_day_limit_formation(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let day_limit = TributeContract::new(self.storage.clone()).get_wwd_metadosis_limit(wwd)?;
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
        TributeContract::new(self.storage.clone())
            .fixture_set_scheduled_process_time(wwd, scheduled_process_time)
    }

    #[cfg(test)]
    fn set_wwd_day_type(
        &mut self,
        wwd: WorldwideDay,
        day_type: crate::WwdDayType,
    ) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).set_wwd_day_type(wwd, day_type)
    }

    fn add_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).add_active_wwd(wwd)
    }

    #[cfg(test)]
    fn remove_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        TributeContract::new(self.storage.clone()).remove_active_wwd(wwd)
    }
}

#[cfg(test)]
impl<'storage> MetadosisContract<'storage> {
    pub(crate) fn tribute(&self) -> TributeContract<'storage> {
        TributeContract::new(self.storage.clone())
    }
}
