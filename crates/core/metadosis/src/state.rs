use crate::aggregate::WwdDayType;
#[cfg(test)]
use crate::aggregate::WwdStatus;
use crate::errors::{MetadosisCallerError, MetadosisStorageCorruptionError};
use crate::schema::{DayLimitFormationReceiptStateEntryExt, MetadosisContract};
use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay as WorldwideDayKey;
use outbe_tribute::TributeContract;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DayLimitFormation {
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
    Formed(DayLimitFormation),
}

impl MetadosisContract<'_> {
    pub(crate) fn validate_day_limit_binding(
        &self,
        current: &crate::aggregate::WwdProjection,
        reason: &'static str,
    ) -> Result<()> {
        match self.day_limit_formation(current.worldwide_day)? {
            Some(formation) if formation.day_limit == current.metadosis_limit_minor => Ok(()),
            Some(_) => Err(
                MetadosisStorageCorruptionError::FormedDayLimitDoesNotMatchWwdState { reason }
                    .into(),
            ),
            None if current.metadosis_limit_minor.is_zero() => Ok(()),
            None => {
                Err(MetadosisStorageCorruptionError::DayLimitWithoutFormation { reason }.into())
            }
        }
    }

    pub(crate) fn commit_write_day_limit_formation(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        formation: DayLimitFormation,
    ) -> Result<()> {
        if self
            .day_limit_formation_receipts
            .entry(formation.worldwide_day)
            .formed()
            .read()?
        {
            return Err(MetadosisCallerError::FormedDayLimitImmutable.into());
        }
        let mut tribute = TributeContract::new(self.storage.clone());
        outbe_tribute::worldwideday::commit_set_day_limit(
            &mut tribute,
            formation.worldwide_day,
            formation.day_limit,
            || {
                MetadosisStorageCorruptionError::DayLimitFormationMissingPersistedWwd {
                    worldwide_day: formation.worldwide_day,
                }
                .into()
            },
            || MetadosisStorageCorruptionError::DayLimitFormationRequiresNoopOuterTransition.into(),
        )?;
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

    pub fn day_limit_formation(
        &self,
        wwd_key: WorldwideDayKey,
    ) -> Result<Option<DayLimitFormation>> {
        let receipt = self.day_limit_formation_receipts.entry(wwd_key);
        if !receipt.formed().read()? {
            return Ok(None);
        }
        let persisted_day_limit =
            TributeContract::new(self.storage.clone()).get_wwd_metadosis_limit(wwd_key)?;
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
            return Err(MetadosisStorageCorruptionError::FormedDayLimitReceiptInconsistent.into());
        }
        Ok(Some(DayLimitFormation {
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
        self.day_limit_formation(wwd_key)
            .map(|formed| formed.map(DayLimitFormationReceipt::Formed))
    }

    pub(crate) fn cleanup_evicted_wwd_receipts(
        &mut self,
        _permit: &crate::commit::CommitPermit<'_>,
        evicted_days: &[WorldwideDayKey],
    ) -> Result<()> {
        for &wwd_key in evicted_days {
            self.delete_wwd_receipts_raw(wwd_key)?;
        }
        Ok(())
    }

    pub(crate) fn delete_wwd_receipts_raw(&mut self, wwd_key: WorldwideDayKey) -> Result<()> {
        self.worldwide_day_terminal_receipts
            .get_bytes(&wwd_key)
            .clear()?;
        if self
            .day_limit_formation_receipts
            .entry(wwd_key)
            .formed()
            .read()?
        {
            self.day_limit_formation_receipts.delete(wwd_key)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn get_wwd_status(&self, wwd: WorldwideDayKey) -> Result<WwdStatus> {
        TributeContract::new(self.storage.clone()).get_wwd_status(wwd)
    }

    pub fn get_wwd_day_type(&self, wwd: WorldwideDayKey) -> Result<WwdDayType> {
        TributeContract::new(self.storage.clone()).get_wwd_day_type(wwd)
    }

    #[cfg(test)]
    pub fn get_active_wwd_by_status(
        &self,
        wanted_status: WwdStatus,
    ) -> Result<Vec<WorldwideDayKey>> {
        TributeContract::new(self.storage.clone()).get_active_wwd_by_status(wanted_status)
    }

    // --- Bootstrap ---

    pub fn set_bootstrap_end_time(&mut self, end_time: u64) -> Result<()> {
        self.bootstrap_end_time.write(end_time)
    }

    pub fn get_bootstrap_end_time(&self) -> Result<u64> {
        self.bootstrap_end_time.read()
    }
}
