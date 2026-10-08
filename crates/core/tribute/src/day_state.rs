//! Storage boundary for encrypted daily amounts and transient calculation views.

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    time::WorldwideDay,
    tribute_day_encryption::EncryptedTributeDayAmountV2,
};
use outbe_tee::tribute_day::{TributeDayOpRequestV2, TributeDayOperationV2};

use crate::{
    day_schema::{StoredDayPreAdmission, StoredDayTotals},
    DayPreAdmission, DayTotals, TributeContract,
};

impl TributeContract<'_> {
    pub(crate) fn freeze_day_nominal(&self, admission: &DayPreAdmission) -> Result<()> {
        let mut context = b"outbe/tribute/day-freeze/v2".to_vec();
        context.extend_from_slice(admission.sealed_collection_root.as_slice());
        context.extend_from_slice(&admission.sealed_tribute_count.to_be_bytes());
        context.extend_from_slice(&admission.canonical_body_bytes.to_be_bytes());
        context.extend_from_slice(&admission.distinct_owner_count.to_be_bytes());
        context.extend_from_slice(&admission.distinct_reference_currency_count.to_be_bytes());
        context.extend_from_slice(&admission.source_generation.to_be_bytes());
        self.apply_day_amount(
            admission.worldwide_day,
            TributeDayOperationV2::Freeze,
            keccak256(context),
        )
    }

    pub fn encrypted_day_nominal(&self, day: WorldwideDay, frozen: bool) -> Result<Vec<u8>> {
        if frozen {
            self.frozen_day_nominal_ct.get_bytes(&day).read()
        } else {
            self.day_nominal_ct.get_bytes(&day).read()
        }
    }

    pub(crate) fn day_amount_record(
        &self,
        day: WorldwideDay,
        frozen: bool,
    ) -> Result<Option<EncryptedTributeDayAmountV2>> {
        let bytes = self.encrypted_day_nominal(day, frozen)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let record: EncryptedTributeDayAmountV2 = postcard::from_bytes(&bytes).map_err(corrupt)?;
        if record.worldwide_day != day
            || record.frozen != frozen
            || record.chain_id != self.storage_handle().chain_id()?
            || record.version().is_none()
        {
            return Err(corrupt("invalid encrypted Tribute day identity"));
        }
        Ok(Some(record))
    }

    pub(crate) fn day_nominal_amount(&self, day: WorldwideDay, frozen: bool) -> Result<U256> {
        match self.day_amount_record(day, frozen)? {
            None => Ok(U256::ZERO),
            Some(record) => crate::enclave_client::read_day_amount(&record).map_err(enclave_error),
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
        let record = crate::enclave_client::apply_day_operation(request).map_err(enclave_error)?;
        if record.frozen != frozen {
            return Err(enclave_error("Tribute day response scope mismatch"));
        }
        let bytes = postcard::to_allocvec(&record).map_err(corrupt)?;
        if frozen {
            self.frozen_day_nominal_ct.get_bytes(&day).write(&bytes)
        } else {
            self.day_nominal_ct.get_bytes(&day).write(&bytes)
        }
    }

    pub(crate) fn reset_day_nominal(&self, totals: &DayTotals) -> Result<()> {
        let mut context = b"outbe/tribute/day-retirement/v2".to_vec();
        context.extend_from_slice(&totals.worldwide_day.value().to_be_bytes());
        context.extend_from_slice(&totals.tribute_count.to_be_bytes());
        context.push(u8::from(totals.is_sealed));
        self.apply_day_amount(
            totals.worldwide_day,
            TributeDayOperationV2::Reset {
                expected_total: totals.tribute_nominal_total_minor,
            },
            keccak256(context),
        )
    }

    pub(crate) fn read_day_pre_admission(&self, day: WorldwideDay) -> Result<DayPreAdmission> {
        let stored = self
            .day_pre_admission
            .get(day)?
            .unwrap_or_else(|| StoredDayPreAdmission::with_key(day));
        let mut view = DayPreAdmission::with_key(day);
        view.initialized = stored.initialized;
        view.is_sealed = stored.is_sealed;
        view.sealed_collection_root = stored.sealed_collection_root;
        view.sealed_tribute_count = stored.sealed_tribute_count;
        view.sealed_tribute_nominal_total_minor = self.day_nominal_amount(day, true)?;
        view.canonical_body_bytes = stored.canonical_body_bytes;
        view.distinct_owner_count = stored.distinct_owner_count;
        view.distinct_reference_currency_count = stored.distinct_reference_currency_count;
        view.source_generation = stored.source_generation;
        Ok(view)
    }

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

    pub(crate) fn store_pre_admission_metadata(&mut self, view: &DayPreAdmission) -> Result<()> {
        let mut stored = StoredDayPreAdmission::with_key(view.worldwide_day);
        stored.initialized = view.initialized;
        stored.is_sealed = view.is_sealed;
        stored.sealed_collection_root = view.sealed_collection_root;
        stored.sealed_tribute_count = view.sealed_tribute_count;
        stored.canonical_body_bytes = view.canonical_body_bytes;
        stored.distinct_owner_count = view.distinct_owner_count;
        stored.distinct_reference_currency_count = view.distinct_reference_currency_count;
        stored.source_generation = view.source_generation;
        if self.day_pre_admission.exists(view.worldwide_day)? {
            self.day_pre_admission.update(&stored)
        } else {
            self.day_pre_admission.create(&stored)
        }
    }
}

fn corrupt(error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::BodyReadCorruption(error.to_string())
}
fn enclave_error(error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(format!("Tribute day enclave: {error}"))
}
