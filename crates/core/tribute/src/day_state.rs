//! Storage boundary for encrypted daily amounts and transient calculation views.

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::{
    error::Result,
    time::WorldwideDay,
    tribute_day_encryption::{
        EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1, EncryptedTributeLeagueAmountV1,
    },
};
use outbe_tee::tribute_day::{
    TributeDayOpRequestV2, TributeDayOperationV2, TributeLeagueOpRequestV1,
};

use crate::{day_schema::StoredDayTotals, schema::day_league_key, DayTotals, TributeContract};

impl TributeContract<'_> {
    pub fn freeze_day_nominal(&self, totals: &DayTotals) -> Result<()> {
        self.storage_handle().require_metadosis_process_ready()?;
        if self
            .day_amount_record(totals.worldwide_day, true)?
            .is_some()
        {
            if self.day_nominal_amount(totals.worldwide_day, true)?
                != totals.tribute_nominal_total_minor
            {
                return Err(
                    crate::errors::TributeError::SealedDayMetadataDiffersFromEncryptedAmount.into(),
                );
            }
            return Ok(());
        }
        let mut context = b"outbe/tribute/day-freeze/v2".to_vec();
        context.extend_from_slice(&totals.worldwide_day.value().to_be_bytes());
        context.extend_from_slice(&totals.tribute_count.to_be_bytes());
        context.push(u8::from(totals.is_sealed));
        self.apply_day_amount(
            totals.worldwide_day,
            TributeDayOperationV2::Freeze,
            keccak256(context),
        )
    }

    pub fn encrypted_day_nominal(&self, day: WorldwideDay, frozen: bool) -> Result<Vec<u8>> {
        if frozen {
            self.frozen_day_nominal_ct.bytes(&day).read()
        } else {
            self.day_nominal_ct.bytes(&day).read()
        }
    }

    fn decode_validated_record<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
        matches_identity: impl FnOnce(&T, u64) -> bool,
    ) -> Result<Option<T>> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let record: T = postcard::from_bytes(bytes).map_err(crate::errors::TributeError::from)?;
        if !matches_identity(&record, self.storage_handle().chain_id()?) {
            return Err(crate::errors::TributeError::InvalidEncryptedDayIdentity.into());
        }
        Ok(Some(record))
    }

    pub(crate) fn day_amount_record(
        &self,
        day: WorldwideDay,
        frozen: bool,
    ) -> Result<Option<EncryptedTributeDayAmountV2>> {
        let bytes = self.encrypted_day_nominal(day, frozen)?;
        self.decode_validated_record(&bytes, |record: &EncryptedTributeDayAmountV2, chain_id| {
            record.worldwide_day == day
                && record.frozen == frozen
                && record.chain_id == chain_id
                && record.version().is_some()
        })
    }

    pub(crate) fn day_nominal_amount(&self, day: WorldwideDay, frozen: bool) -> Result<U256> {
        match self.day_amount_record(day, frozen)? {
            None => Ok(U256::ZERO),
            Some(record) => crate::enclave_client::read_day_amount(&record)
                .map_err(|err| crate::errors::TributeError::DayEnclave(err).into()),
        }
    }

    pub(crate) fn apply_day_amount(
        &self,
        day: WorldwideDay,
        operation: TributeDayOperationV2,
        public_state_hash: B256,
    ) -> Result<()> {
        let frozen = matches!(operation, TributeDayOperationV2::Freeze);
        let request = TributeDayOpRequestV2 {
            chain_id: self.storage_handle().chain_id()?,
            worldwide_day: day,
            previous: self.day_amount_record(day, false)?,
            operation,
            public_state_hash,
        };
        let record = crate::enclave_client::apply_day_operation(request)
            .map_err(crate::errors::TributeError::DayEnclave)?;
        if record.frozen != frozen {
            return Err(crate::errors::TributeError::DayResponseScopeMismatch.into());
        }
        let bytes = postcard::to_allocvec(&record).map_err(crate::errors::TributeError::from)?;
        if frozen {
            self.frozen_day_nominal_ct.bytes(&day).write(&bytes)
        } else {
            self.day_nominal_ct.bytes(&day).write(&bytes)
        }
    }

    pub(crate) fn set_day_sealed(&mut self, day: WorldwideDay, is_sealed: bool) -> Result<()> {
        let mut stored = self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| StoredDayTotals::with_key(day));
        stored.initialized = true;
        stored.is_sealed = is_sealed;
        if self.day_totals.exists(day)? {
            self.day_totals.update(&stored)
        } else {
            self.day_totals.create(&stored)
        }
    }

    #[cfg(any(test, feature = "test-enclave"))]
    pub(crate) fn store_day_metadata(&mut self, totals: &DayTotals) -> Result<()> {
        let mut stored = StoredDayTotals::with_key(totals.worldwide_day);
        stored.initialized = totals.initialized;
        stored.tribute_count = totals.tribute_count;
        stored.is_sealed = totals.is_sealed;
        if self.day_totals.exists(totals.worldwide_day)? {
            self.day_totals.update(&stored)
        } else {
            self.day_totals.create(&stored)
        }
    }

    pub fn encrypted_league_nominal(&self, day: WorldwideDay, league: u16) -> Result<Vec<u8>> {
        self.fi_group_nominal_ct
            .bytes(&day_league_key(day, league))
            .read()
    }

    pub(crate) fn league_amount_record(
        &self,
        day: WorldwideDay,
        league: u16,
    ) -> Result<Option<EncryptedTributeLeagueAmountV1>> {
        let bytes = self.encrypted_league_nominal(day, league)?;
        self.decode_validated_record(
            &bytes,
            |record: &EncryptedTributeLeagueAmountV1, chain_id| {
                record.worldwide_day == day
                    && record.league == league
                    && record.chain_id == chain_id
                    && record.version().is_some()
            },
        )
    }

    pub(crate) fn league_nominal_amount(&self, day: WorldwideDay, league: u16) -> Result<U256> {
        match self.league_amount_record(day, league)? {
            None => Ok(U256::ZERO),
            Some(record) => crate::enclave_client::read_league_amount(&record)
                .map_err(|err| crate::errors::TributeError::DayEnclave(err).into()),
        }
    }

    pub(crate) fn apply_league_amount(
        &self,
        day: WorldwideDay,
        league: u16,
        operation: TributeDayOperationV2,
        public_state_hash: B256,
    ) -> Result<()> {
        let request = TributeLeagueOpRequestV1 {
            chain_id: self.storage_handle().chain_id()?,
            worldwide_day: day,
            league,
            previous: self.league_amount_record(day, league)?,
            operation,
            public_state_hash,
        };
        let record = crate::enclave_client::apply_league_operation(request)
            .map_err(crate::errors::TributeError::DayEnclave)?;
        if record.worldwide_day != day || record.league != league {
            return Err(crate::errors::TributeError::DayResponseScopeMismatch.into());
        }
        let bytes = postcard::to_allocvec(&record).map_err(crate::errors::TributeError::from)?;
        self.fi_group_nominal_ct
            .bytes(&day_league_key(day, league))
            .write(&bytes)
    }

    pub fn encrypted_day_fractions(&self, emission_day: u32) -> Result<Vec<u8>> {
        self.day_fraction_map_ct.bytes(&emission_day).read()
    }

    pub(crate) fn day_fractions_record(
        &self,
        emission_day: u32,
    ) -> Result<Option<EncryptedTributeDayFractionsV1>> {
        let bytes = self.encrypted_day_fractions(emission_day)?;
        self.decode_validated_record(
            &bytes,
            |record: &EncryptedTributeDayFractionsV1, chain_id| {
                record.emission_day == emission_day
                    && record.chain_id == chain_id
                    && record.version().is_some()
            },
        )
    }

    pub(crate) fn store_day_fractions_record(
        &self,
        emission_day: u32,
        record: &EncryptedTributeDayFractionsV1,
    ) -> Result<()> {
        if record.emission_day != emission_day
            || record.chain_id != self.storage_handle().chain_id()?
        {
            return Err(crate::errors::TributeError::DayResponseScopeMismatch.into());
        }
        let bytes = postcard::to_allocvec(record).map_err(crate::errors::TributeError::from)?;
        self.day_fraction_map_ct.bytes(&emission_day).write(&bytes)
    }

    /// The emission day whose Lysis covers the Tributes of `day`, once it is known.
    pub(crate) fn lysis_emission_day(&self, day: WorldwideDay) -> Result<Option<u32>> {
        Ok(Some(self.day_emission_day.read(&day)?).filter(|&emission_day| emission_day != 0))
    }

    /// When the Lysis of the Tributes of `day` completed, or 0 before it did.
    pub fn lysis_completed_at(&self, day: WorldwideDay) -> Result<u64> {
        match self.lysis_emission_day(day)? {
            Some(emission_day) => self.day_metadosis_completed_at.read(&emission_day),
            None => Ok(0),
        }
    }
}
