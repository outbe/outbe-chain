mod forfeiture;

use alloy_primitives::U256;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::errors::{date_step_error, TributeError};
use crate::precompile::ITribute;
use crate::schema::TributeContract;

pub use crate::state_api::LoadedTribute;

/// Result of a logical Tribute day retirement. Stored ciphertext remains available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetirementOutcome {
    NotPresent,
    Requested,
}

/// Constant-size owner receipt for a sealed Tribute partition forfeited by
/// Metadosis retained-cap policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeForfeitureReceipt {
    pub worldwide_day: WorldwideDay,
    pub forfeited_count: u32,
    pub forfeited_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement_outcome: RetirementOutcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeLysisReceipt {
    pub worldwide_day: WorldwideDay,
    pub consumed_count: u32,
    pub consumed_nominal: U256,
    pub source_generation: u64,
    pub retired_generation: u64,
    pub retirement_outcome: RetirementOutcome,
}

impl TributeContract<'_> {
    fn is_valid_lysis_partition(
        &self,
        day: WorldwideDay,
        totals: &crate::schema::DayTotals,
        verified_count: u32,
        verified_nominal: U256,
    ) -> Result<bool> {
        if self.retired_days.read(&day)? {
            return Ok(false);
        }
        let ready = totals.initialized && totals.is_sealed && verified_count > 0;
        let matches = totals.tribute_count == verified_count
            && totals.tribute_nominal_total_minor == verified_nominal;
        Ok(ready && matches)
    }

    pub fn consume_and_retire_lysis_partition(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: U256,
    ) -> Result<TributeLysisReceipt> {
        let storage = self.storage_handle();
        storage.require_metadosis_process_ready()?;
        storage.with_checkpoint(|| {
            let totals = self.get_day_totals(day)?;
            if !self.is_valid_lysis_partition(day, &totals, verified_count, verified_nominal)? {
                return Err(TributeError::LysisInputMismatch.into());
            }
            self.freeze_day_nominal(&totals)?;
            self.consume_lysis_partition_inner(day, verified_count, verified_nominal)?;
            let retirement_outcome = self.retire_completed_partition_inner(day)?;
            if retirement_outcome != RetirementOutcome::Requested {
                return Err(TributeError::LysisInputMismatch.into());
            }
            Ok(TributeLysisReceipt {
                worldwide_day: day,
                consumed_count: verified_count,
                consumed_nominal: verified_nominal,
                source_generation: 0,
                retired_generation: 1,
                retirement_outcome,
            })
        })
    }

    /// Applies ADR-011's one bulk accounting transition after every verified
    /// Tribute in the sealed WWD has produced exactly one Nod.
    pub fn consume_lysis_partition(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: alloy_primitives::U256,
    ) -> Result<()> {
        self.consume_lysis_partition_inner(day, verified_count, verified_nominal)
    }

    pub(crate) fn consume_lysis_partition_inner(
        &mut self,
        day: WorldwideDay,
        verified_count: u32,
        verified_nominal: alloy_primitives::U256,
    ) -> Result<()> {
        let mut totals = self.get_day_totals(day)?;
        if !totals.initialized
            || !totals.is_sealed
            || totals.tribute_count != verified_count
            || totals.tribute_nominal_total_minor != verified_nominal
        {
            return Err(TributeError::LysisInputMismatch.into());
        }
        let supply = self
            .total_supply
            .read()?
            .checked_sub(u64::from(verified_count))
            .ok_or(TributeError::TotalSupplyUnderflowDuringLysis)?;
        self.total_supply.write(supply)?;
        self.reset_day_nominal(&totals)?;
        totals.tribute_count = 0;
        totals.tribute_nominal_total_minor = alloy_primitives::U256::ZERO;
        self.store_day_totals(&totals)
    }

    /// Retires a sealed empty day in the same state checkpoint as completion.
    pub fn retire_completed_partition(&mut self, day: WorldwideDay) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.retire_completed_partition_inner(day))
    }

    /// Retires a sealed empty day when OFFERING was never opened.
    pub fn retire_empty_missed_offering_partition(
        &mut self,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            let totals = self.get_day_totals(day)?;
            if !totals.initialized
                || !totals.is_sealed
                || totals.tribute_count != 0
                || !totals.tribute_nominal_total_minor.is_zero()
            {
                return Err(TributeError::MissedOfferingRequiresSealedEmptyPartition.into());
            }
            self.retire_completed_partition_inner(day)
        })
    }

    /// Forfeits one sealed partition from its frozen state aggregates.
    /// The operation retains all ciphertext records and indexes.
    pub fn forfeit_sealed_partition(
        &mut self,
        day: WorldwideDay,
    ) -> Result<TributeForfeitureReceipt> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| forfeiture::apply(self, day))
    }

    pub(crate) fn retire_completed_partition_inner(
        &mut self,
        day: WorldwideDay,
    ) -> Result<RetirementOutcome> {
        Ok(
            if crate::state_api::retire_completed_day(&self.storage_handle(), day)? {
                RetirementOutcome::Requested
            } else {
                RetirementOutcome::NotPresent
            },
        )
    }

    pub fn burn(&mut self, tribute_id: WwdEntityId) -> Result<()> {
        let loaded = self
            .load_tribute(tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        self.burn_loaded(loaded)
    }

    /// Loads one Tribute while retaining its verified generic mutation capability.
    pub fn load_tribute(&self, tribute_id: WwdEntityId) -> Result<Option<LoadedTribute>> {
        crate::state_api::load(&self.storage_handle(), tribute_id)
    }

    /// Burns the exact state record retained by the loaded capability.
    pub fn burn_loaded(&mut self, loaded: LoadedTribute) -> Result<()> {
        crate::state_api::burn_loaded(&self.storage_handle(), loaded)
    }

    pub fn burn_all_by_wwd(&mut self, day: WorldwideDay) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            for id in self.get_tribute_ids_by_day(day)? {
                self.burn(id)?;
            }
            Ok(())
        })
    }

    pub fn seal_day(&mut self, day: WorldwideDay) -> Result<()> {
        self.set_day_sealed(day, true)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: true,
        })?;
        Ok(())
    }

    pub fn unseal_day(&mut self, day: WorldwideDay) -> Result<()> {
        if self.retired_days.read(&day)? || self.day_amount_record(day, true)?.is_some() {
            return Err(TributeError::WorldwideDaySealed.into());
        }
        self.set_day_sealed(day, false)?;
        self.emit(ITribute::TributeWorldwideDaySealed {
            worldwideDay: day.into(),
            isSealed: false,
        })?;
        Ok(())
    }

    /// Records the Stage 3 Metadosis league fractions and lysis limit for `day`.
    pub fn record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        lysis_limit_minor: U256,
        fractions: &std::collections::BTreeMap<u16, u64>,
        completed_at: u64,
    ) -> Result<()> {
        self.day_lysis_limit_minor.write(&day, lysis_limit_minor)?;
        self.day_metadosis_completed_at.write(&day, completed_at)?;
        for (&league, &fraction) in fractions {
            let key = crate::schema::day_league_key(day, league);
            self.day_fractions.write(&key, fraction)?;
        }
        Ok(())
    }

    fn sum_allocated_gratis_from_groups(
        fi_groups: &std::collections::BTreeMap<u16, (u32, U256)>,
        u256_fractions: &std::collections::BTreeMap<u16, U256>,
        lysis_limit_minor: U256,
    ) -> U256 {
        let nod_max: U256 = lysis_limit_minor >> 10usize;
        if nod_max.is_zero() {
            return U256::ZERO;
        }
        let scale = U256::from(1_000_000u64);
        let mut total_allocated = U256::ZERO;
        for (&league, &(count, nominal_sum)) in fi_groups {
            if count == 0 || nominal_sum.is_zero() {
                continue;
            }
            let fraction = u256_fractions.get(&league).copied().unwrap_or(U256::ZERO);
            if fraction.is_zero() {
                continue;
            }
            let league_cap = nod_max.saturating_mul(U256::from(count));
            let league_raw = outbe_primitives::math::scaled_math::checked_mul_div_floor(
                nominal_sum,
                fraction,
                scale,
            )
            .unwrap_or(league_cap);
            total_allocated = total_allocated.saturating_add(league_raw.min(league_cap));
        }
        total_allocated.min(lysis_limit_minor)
    }

    /// Computes league fractions from `fi_groups[day]` and `lysis_limit_minor`,
    /// stores them along with `completed_at`, and returns `(fractions, total_allocated_minor)`.
    pub fn compute_and_record_metadosis_fractions(
        &mut self,
        day: WorldwideDay,
        lysis_limit_minor: U256,
        completed_at: u64,
    ) -> Result<(std::collections::BTreeMap<u16, u64>, U256)> {
        let fi_groups = self.get_fi_groups(day)?;
        let mut tribute_count = 0u32;
        let mut total_interest = U256::ZERO;
        for &(count, nom) in fi_groups.values() {
            tribute_count = tribute_count
                .checked_add(count)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
            total_interest = total_interest
                .checked_add(nom)
                .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
        }
        let u256_fractions = if tribute_count == 0 || total_interest.is_zero() {
            std::collections::BTreeMap::new()
        } else {
            outbe_lysis::program_v1::compute_fraction_map_from_groups(
                &fi_groups,
                tribute_count,
                total_interest,
                lysis_limit_minor,
            )
            .map_err(|e| TributeError::LysisArithmetic(e.to_string()))?
        };
        let fractions: std::collections::BTreeMap<u16, u64> = u256_fractions
            .iter()
            .map(|(&league, &frac)| (league, u64::try_from(frac).unwrap_or(u64::MAX)))
            .collect();
        self.record_metadosis_fractions(day, lysis_limit_minor, &fractions, completed_at)?;
        let allocated =
            Self::sum_allocated_gratis_from_groups(&fi_groups, &u256_fractions, lysis_limit_minor);
        Ok((fractions, allocated))
    }

    /// Stage 5: Creates a `Nod` for `tribute_id` within 7 days after `Metadosis`
    /// and atomically burns the `Tribute`.
    pub fn create_nod(
        &mut self,
        caller: alloy_primitives::Address,
        tribute_id: WwdEntityId,
    ) -> Result<WwdEntityId> {
        let storage = self.storage_handle();
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
        storage.with_checkpoint(|| {
            let day = tribute_id.worldwide_day();
            if self.retired_days.read(&day)? {
                return Err(TributeError::TributeNotFound.into());
            }
            let completed_at = self.day_metadosis_completed_at.read(&day)?;
            if completed_at == 0 {
                return Err(TributeError::MetadosisNotCompleted.into());
            }
            let now = storage.now_secs()?;
            if now < completed_at
                || now > completed_at.saturating_add(7 * outbe_primitives::time::SECONDS_PER_DAY)
            {
                return Err(TributeError::NodCreationWindowExpired.into());
            }
            let record = crate::state_api::get_retained_record(&storage, tribute_id)?
                .ok_or(TributeError::TributeNotFound)?;
            if record.owner != caller {
                return Err(TributeError::CallerNotTributeOwner.into());
            }
            let raw_league = self.tribute_leagues.read(&tribute_id)?;
            let league = raw_league.max(1);
            let fraction = U256::from(
                self.day_fractions
                    .read(&crate::schema::day_league_key(day, league))?,
            );
            let lysis_limit = self.day_lysis_limit_minor.read(&day)?;
            let nominal = record
                .calculation_view()
                .map_err(TributeError::private_amount_read_failed)?
                .nominal_amount_minor;
            let nod_max: U256 = lysis_limit >> 10usize;
            let gratis_load_minor =
                outbe_lysis::program_v1::calculate_gratis_load(nominal, fraction, nod_max, 0)
                    .map_err(|e| TributeError::LysisArithmetic(e.to_string()))?;

            let price_day =
                outbe_nod::openings::entry_price_day(completed_at).map_err(date_step_error)?;
            let gratis_day_price = outbe_oracle::api::get_utc_day_vwap_for_iso(
                storage.clone(),
                price_day,
                record.reference_currency,
            )?
            .ok_or(TributeError::EntryPriceUnavailable {
                price_day,
                currency: record.reference_currency,
            })?;

            let encrypted_nod = outbe_primitives::nod_encryption::encode_v1_nod(
                outbe_primitives::nod_encryption::NodTermsV2 {
                    chain_id: storage.chain_id()?,
                    nod_id: tribute_id,
                    owner: record.owner,
                    worldwide_day: day,
                    league_id: league,
                    issuance_currency: record.issuance_currency,
                    reference_currency: record.reference_currency,
                    entry_price_minor: gratis_day_price,
                },
                gratis_load_minor,
            );
            outbe_nodfactory::api::issue_tribute_nod(&storage, &encrypted_nod, gratis_load_minor)?;

            // Atomically burn the Tribute upon Nod creation, keeping partition accounting consistent.
            self.bump_day_bucket_record(&record, false)?;
            if raw_league != 0 {
                self.adjust_fi_group(day, raw_league, nominal, false)?;
                self.tribute_leagues.clear(&tribute_id)?;
            }
            let supply = self
                .total_supply
                .read()?
                .checked_sub(1)
                .ok_or(TributeError::TotalSupplyUnderflowDuringBurn)?;
            self.total_supply.write(supply)?;
            self.delete_loaded_state_record(&record)?;
            self.emit(ITribute::TributeBurned {
                tributeId: tribute_id.to_u256(),
                owner: record.owner,
                worldwideDay: day.into(),
            })?;
            Ok(tribute_id)
        })
    }
}
