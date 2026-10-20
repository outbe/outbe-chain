use alloy_primitives::U256;
use outbe_primitives::{error::Result as PrecompileResult, time::WorldwideDay};

use crate::schema::TributeContract;

use super::{
    aggregate::{WwdDayType, WwdStatus},
    constants::{FORMING_PERIOD_HOURS, MAX_ACTIVE_WWDS, SECONDS_PER_HOUR, WAITING_PERIOD_HOURS},
    errors::{WwdCallerError, WwdTestSupportError},
    schema::{WorldwideDay as WorldwideDayRecord, WorldwideDayEntryExt},
};

/// Test-only raw fixture kernel for WorldwideDay state on `TributeContract`.
///
/// Production mutations must never use this trait. It exists only in test or
/// `test-utils` builds so tests can seed predecessor state and intentional
/// corruption without leaking raw mutation methods onto `TributeContract`.
pub trait WwdFixtureKernelExt {
    fn create_worldwide_day(
        &mut self,
        wwd: WorldwideDay,
        forming_start: u64,
        lookback_delay_hours: u64,
        offering_period_hours: u64,
    ) -> PrecompileResult<()>;

    fn delete_worldwide_day_record(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;

    fn set_wwd_metadosis_limit_raw(
        &mut self,
        wwd: WorldwideDay,
        amount: U256,
    ) -> PrecompileResult<()>;

    fn fixture_set_wwd_status(
        &mut self,
        wwd: WorldwideDay,
        status: WwdStatus,
    ) -> PrecompileResult<()>;

    fn fixture_create_ready_day(
        &mut self,
        wwd: WorldwideDay,
        day_limit: U256,
        previous_vwap_minor: U256,
        current_vwap_minor: U256,
    ) -> PrecompileResult<()>;

    fn corrupt_wwd_status_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()>;

    fn corrupt_wwd_day_type_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()>;

    fn fixture_set_scheduled_process_time(
        &mut self,
        wwd: WorldwideDay,
        scheduled_process_time: u64,
    ) -> PrecompileResult<()>;

    fn set_wwd_day_type(&mut self, wwd: WorldwideDay, day_type: WwdDayType)
        -> PrecompileResult<()>;

    fn add_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;

    fn remove_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()>;
}

impl WwdFixtureKernelExt for TributeContract<'_> {
    fn create_worldwide_day(
        &mut self,
        wwd: WorldwideDay,
        forming_start: u64,
        lookback_delay_hours: u64,
        offering_period_hours: u64,
    ) -> PrecompileResult<()> {
        let seconds = |hours: u64, label: &'static str| -> PrecompileResult<u64> {
            hours
                .checked_mul(SECONDS_PER_HOUR)
                .ok_or_else(|| WwdCallerError::DurationOverflow { label }.into())
        };
        let forming_end = forming_start
            .checked_add(seconds(FORMING_PERIOD_HOURS, "forming")?)
            .ok_or(WwdCallerError::FormingWindowOverflow)?;
        let lookback_end = forming_end
            .checked_add(seconds(lookback_delay_hours, "lookback")?)
            .ok_or(WwdCallerError::LookbackWindowOverflow)?;
        let offering_end = lookback_end
            .checked_add(seconds(offering_period_hours, "offering")?)
            .ok_or(WwdCallerError::OfferingWindowOverflow)?;
        let scheduled_process_time = offering_end
            .checked_add(seconds(WAITING_PERIOD_HOURS, "waiting")?)
            .ok_or(WwdCallerError::WaitingWindowOverflow)?;

        self.worldwide_days.create(&WorldwideDayRecord {
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

    fn delete_worldwide_day_record(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        self.worldwide_days.delete(wwd)
    }

    fn set_wwd_metadosis_limit_raw(
        &mut self,
        wwd: WorldwideDay,
        amount: U256,
    ) -> PrecompileResult<()> {
        self.worldwide_days
            .entry(wwd)
            .metadosis_limit_minor()
            .write(amount)
    }

    fn fixture_set_wwd_status(
        &mut self,
        wwd: WorldwideDay,
        status: WwdStatus,
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
        previous_vwap_minor: U256,
        current_vwap_minor: U256,
    ) -> PrecompileResult<()> {
        self.worldwide_days.create(&WorldwideDayRecord {
            wwd,
            status: WwdStatus::Ready.as_u8(),
            day_type: WwdDayType::Green.as_u8(),
            forming_start: 1,
            forming_end: 2,
            lookback_end: 3,
            offering_end: 4,
            scheduled_process_time: 5,
            metadosis_limit_minor: day_limit,
            previous_vwap_minor,
            current_vwap_minor,
        })
    }

    fn corrupt_wwd_status_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        self.worldwide_days.entry(wwd).status().write(tag)
    }

    fn corrupt_wwd_day_type_tag(&mut self, wwd: WorldwideDay, tag: u8) -> PrecompileResult<()> {
        self.worldwide_days.entry(wwd).day_type().write(tag)
    }

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

    fn set_wwd_day_type(
        &mut self,
        wwd: WorldwideDay,
        day_type: WwdDayType,
    ) -> PrecompileResult<()> {
        self.worldwide_days
            .entry(wwd)
            .day_type()
            .write(day_type.as_u8())
    }

    fn add_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        let active = self.active_wwd.read_all()?;
        if active.contains(&wwd) {
            return Ok(());
        }
        if active.len() >= MAX_ACTIVE_WWDS {
            return Err(WwdTestSupportError::ActiveWwdCapReached {
                max_active_wwds: MAX_ACTIVE_WWDS,
                wwd,
            }
            .into());
        }
        self.active_wwd.insert(wwd).map(|_| ())
    }

    fn remove_active_wwd(&mut self, wwd: WorldwideDay) -> PrecompileResult<()> {
        self.active_wwd.remove(&wwd).map(|_| ())
    }
}
