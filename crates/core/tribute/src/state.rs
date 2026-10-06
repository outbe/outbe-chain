use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_compressed_entities::{
    derive_poseidon_entity_id, list, read, EntityRef, ExecutionScope, IdPageRequest,
    ParentBodySource, QueryRef, VerifiedBody, WwdEntityId, MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

use crate::errors::TributeError;
use crate::schema::{DayPreAdmission, DayTotals, TributeContract, TributeData};
use crate::TributeRecord;

/// Immutable read projection consumed by the OCOMP terminal request path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributePreAdmissionProjection {
    pub worldwide_day: WorldwideDay,
    pub source_generation: u64,
    pub profile_ready: bool,
    pub is_sealed: bool,
    pub sealed_collection_root: B256,
    pub tribute_count: u32,
    pub tribute_nominal_total_minor: U256,
    pub canonical_body_bytes: u64,
    pub distinct_owner_count: u32,
    pub distinct_reference_currency_count: u16,
}

impl TributeContract<'_> {
    /// Initializes the OCOMP accumulator only on an empty fresh-devnet
    /// Tribute state. The genesis-bound OCOMP lifecycle owns the production
    /// call site.
    pub fn initialize_fresh_ocomp_profile(&mut self) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| {
            if self.ocomp_profile_ready.read()? {
                return Ok(());
            }
            if self.total_supply.read()? != 0 {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "Tribute OCOMP profile requires empty live state".into(),
                ));
            }
            self.ocomp_profile_ready.write(true)
        })
    }

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn owner_of(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<Address> {
        let tribute = self
            .get_record(scope, parent, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        Ok(tribute.owner)
    }

    pub fn balance_of(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<u64> {
        let count = self.get_tribute_ids_by_owner(scope, parent, owner)?.len();
        let count: u64 = count
            .try_into()
            .map_err(|_| TributeError::OwnerBalanceOverflow)?;
        Ok(count)
    }

    pub fn token_uri(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<String> {
        let tribute = self
            .get_record(scope, parent, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        let (issuance, nominal, encryption) = tribute.public_amount_attributes()?;
        let attributes = serde_json::json!([
            {"trait_type":"owner", "value": tribute.owner.to_string()},
            {"trait_type":"worldwide_day", "value": tribute.worldwide_day.to_string()},
            {"trait_type":"issuance_currency", "value": tribute.issuance_currency.to_string()},
            {"trait_type":"issuance_amount_minor", "value": issuance},
            {"trait_type":"nominal_amount_minor", "value": nominal},
            {"trait_type":"reference_currency", "value": tribute.reference_currency.to_string()},
            {"trait_type":"exclude_from_intex_issuance", "value": tribute.exclude_from_intex_issuance}
        ]);
        let metadata = serde_json::json!({
            "name": format!("Tribute 0x{}", tribute.tribute_id),
            "description": "Outbe Tribute", "attributes": attributes,
            "encryption": encryption
        });
        Ok(format!("data:application/json;utf8,{metadata}"))
    }

    /// Public authenticated record; no amount decryption occurs on this path.
    pub fn get_record(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<Option<TributeRecord>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::Tribute(tribute_id),
        )?
        .map(|current| record_from_verified(&current))
        .transpose()
    }

    pub fn get_tribute(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute_id: WwdEntityId,
    ) -> Result<Option<TributeData>> {
        read(
            self.storage_handle(),
            scope,
            parent,
            EntityRef::Tribute(tribute_id),
        )?
        .map(|current| tribute_from_verified(&current))
        .transpose()
    }

    pub fn get_day_totals(&self, day: WorldwideDay) -> Result<DayTotals> {
        let stored = self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| crate::day_schema::StoredDayTotals::with_key(day));
        let mut view = DayTotals::with_key(day);
        view.initialized = stored.initialized;
        view.tribute_count = stored.tribute_count;
        view.is_sealed = stored.is_sealed;
        view.tribute_nominal_total_minor = self.day_nominal_amount(day, false)?;
        Ok(view)
    }

    pub fn is_day_sealed(&self, day: WorldwideDay) -> Result<bool> {
        Ok(self
            .day_totals
            .get(day)?
            .map(|totals| totals.is_sealed)
            .unwrap_or(false))
    }

    pub fn pre_admission_projection(
        &self,
        day: WorldwideDay,
    ) -> Result<TributePreAdmissionProjection> {
        let totals = self.get_day_totals(day)?;
        let admission = self.read_day_pre_admission(day)?;
        let (tribute_count, tribute_nominal_total_minor) = if admission.is_sealed {
            (
                admission.sealed_tribute_count,
                admission.sealed_tribute_nominal_total_minor,
            )
        } else {
            (totals.tribute_count, totals.tribute_nominal_total_minor)
        };
        Ok(TributePreAdmissionProjection {
            worldwide_day: day,
            source_generation: admission.source_generation,
            profile_ready: self.ocomp_profile_ready.read()?,
            is_sealed: admission.is_sealed,
            sealed_collection_root: admission.sealed_collection_root,
            tribute_count,
            tribute_nominal_total_minor,
            canonical_body_bytes: admission.canonical_body_bytes,
            distinct_owner_count: admission.distinct_owner_count,
            distinct_reference_currency_count: admission.distinct_reference_currency_count,
        })
    }

    pub fn get_tribute_ids_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<WwdEntityId>> {
        Ok(self
            .read_records(scope, parent, QueryRef::TributeByOwner(owner))?
            .into_iter()
            .map(|tribute| tribute.tribute_id)
            .collect())
    }

    pub fn get_tribute_ids_by_day(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<Vec<WwdEntityId>> {
        Ok(self
            .read_records(scope, parent, QueryRef::TributeByDay(day))?
            .into_iter()
            .map(|tribute| tribute.tribute_id)
            .collect())
    }

    pub(crate) fn validate_tribute_for_issue(&self, tribute: &TributeData) -> Result<()> {
        if tribute.owner.is_zero() {
            return Err(TributeError::InvalidOwner.into());
        }
        if tribute.issuance_amount_minor.is_zero() {
            return Err(TributeError::SettlementAmountMustBePositive.into());
        }
        let expected = derive_poseidon_entity_id(tribute.owner, tribute.worldwide_day)
            .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        if tribute.tribute_id != expected {
            return Err(outbe_primitives::error::PrecompileError::Fatal(format!(
                "Tribute canonical identity mismatch: expected {expected}, found {}",
                tribute.tribute_id
            )));
        }
        Ok(())
    }

    pub(crate) fn ensure_day_accepts_tributes(&self, day: WorldwideDay) -> Result<()> {
        let totals = self.day_totals.get(day)?;
        if !totals.is_some_and(|totals| totals.initialized && !totals.is_sealed) {
            return Err(TributeError::WorldwideDaySealed.into());
        }
        Ok(())
    }

    pub(crate) fn store_day_totals(&mut self, totals: &DayTotals) -> Result<()> {
        if totals.tribute_nominal_total_minor
            != self.day_nominal_amount(totals.worldwide_day, false)?
        {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "day metadata write differs from encrypted amount".into(),
                ),
            );
        }
        self.store_day_metadata(totals)
    }

    pub(crate) fn store_day_pre_admission(&mut self, admission: &DayPreAdmission) -> Result<()> {
        if admission.is_sealed
            && self
                .day_amount_record(admission.worldwide_day, true)?
                .is_none()
        {
            self.freeze_day_nominal(admission)?;
        }
        if admission.sealed_tribute_nominal_total_minor
            != self.day_nominal_amount(admission.worldwide_day, true)?
        {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "sealed day metadata write differs from encrypted amount".into(),
                ),
            );
        }
        self.store_pre_admission_metadata(admission)
    }

    pub(crate) fn update_pre_admission_for_tribute(
        &mut self,
        tribute: &crate::TributeRecord,
        is_add: bool,
    ) -> Result<()> {
        if !self.ocomp_profile_ready.read()? {
            return Ok(());
        }
        let day = tribute.worldwide_day;
        let mut admission = self.read_day_pre_admission(day)?;
        if admission.is_sealed {
            return Err(TributeError::PreAdmissionSealed.into());
        }

        let body_bytes = tribute
            .stored_body()
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(error.to_string())
            })?
            .payload()
            .len();
        let body_bytes = u64::try_from(body_bytes).map_err(|_| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute canonical body length exceeds u64".into(),
            )
        })?;
        let currency_key = reference_currency_refcount_key(day, tribute.reference_currency);
        let currency_refcount = self.day_reference_currency_refcount.read(&currency_key)?;

        let next_currency_refcount = if is_add {
            admission.canonical_body_bytes = admission
                .canonical_body_bytes
                .checked_add(body_bytes)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(
                        "Tribute canonical body byte total overflow".into(),
                    )
                })?;
            if currency_refcount == 0 {
                admission.distinct_reference_currency_count = admission
                    .distinct_reference_currency_count
                    .checked_add(1)
                    .ok_or_else(|| {
                        outbe_primitives::error::PrecompileError::BodyReadCorruption(
                            "Tribute distinct reference currency count overflow".into(),
                        )
                    })?;
            }
            currency_refcount.checked_add(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "Tribute reference currency membership refcount overflow".into(),
                )
            })?
        } else {
            admission.canonical_body_bytes = admission
                .canonical_body_bytes
                .checked_sub(body_bytes)
                .ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(
                        "Tribute canonical body byte total underflow".into(),
                    )
                })?;
            let next_currency = currency_refcount.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "Tribute reference currency membership refcount underflow".into(),
                )
            })?;
            if next_currency == 0 {
                admission.distinct_reference_currency_count = admission
                    .distinct_reference_currency_count
                    .checked_sub(1)
                    .ok_or_else(|| {
                        outbe_primitives::error::PrecompileError::BodyReadCorruption(
                            "Tribute distinct reference currency count underflow".into(),
                        )
                    })?;
            }
            next_currency
        };

        admission.initialized = true;
        admission.distinct_owner_count = self
            .day_totals
            .get(day)?
            .map_or(0, |totals| totals.tribute_count);
        self.day_reference_currency_refcount
            .write(&currency_key, next_currency_refcount)?;
        self.store_day_pre_admission(&admission)
    }

    pub(crate) fn bump_day_bucket_record(
        &mut self,
        record: &TributeRecord,
        add: bool,
    ) -> Result<()> {
        let day = record.worldwide_day;
        let mut totals = self
            .day_totals
            .get(day)?
            .unwrap_or_else(|| crate::day_schema::StoredDayTotals::with_key(day));
        totals.initialized = true;
        totals.tribute_count = if add {
            totals.tribute_count.checked_add(1)
        } else {
            totals.tribute_count.checked_sub(1)
        }
        .ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute day count overflow or underflow".into(),
            )
        })?;
        let operation = match record.encrypted() {
            Some(tribute) => outbe_tee::tribute_day::TributeDayOperationV2::Adjust {
                tribute: Box::new(tribute.clone()),
                add,
            },
            None => outbe_tee::tribute_day::TributeDayOperationV2::AdjustTransient {
                nominal_amount_minor: calculation_view(record)?.nominal_amount_minor,
                add,
            },
        };
        let mut context = b"outbe/tribute/day-count/v2".to_vec();
        context.extend_from_slice(&day.value().to_be_bytes());
        context.extend_from_slice(&totals.tribute_count.to_be_bytes());
        context.push(u8::from(totals.is_sealed));
        self.apply_day_amount(day, operation, keccak256(context))?;
        self.day_totals.update(&totals)
    }

    pub(crate) fn read_all_by_owner(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        owner: Address,
    ) -> Result<Vec<TributeData>> {
        self.read_all(scope, parent, QueryRef::TributeByOwner(owner))
    }

    pub(crate) fn read_all_by_day(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        day: WorldwideDay,
    ) -> Result<Vec<TributeData>> {
        self.read_all(scope, parent, QueryRef::TributeByDay(day))
    }

    fn read_all(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        query: QueryRef,
    ) -> Result<Vec<TributeData>> {
        self.read_records(scope, parent, query)?
            .iter()
            .map(calculation_view)
            .collect()
    }

    fn read_records(
        &self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        query: QueryRef,
    ) -> Result<Vec<TributeRecord>> {
        let mut records = Vec::new();
        let mut after = None;
        loop {
            let page = list(
                self.storage_handle(),
                scope,
                parent,
                query,
                IdPageRequest {
                    after,
                    limit: MAX_ID_PAGE_LIMIT,
                },
            )?;
            let next_after = page.next_after();
            let bodies = page.into_bodies();
            records.extend(
                bodies
                    .iter()
                    .map(record_from_verified)
                    .collect::<Result<Vec<_>>>()?,
            );
            let Some(next) = next_after else {
                return Ok(records);
            };
            after = Some(next);
        }
    }
}

fn reference_currency_refcount_key(day: WorldwideDay, currency: u16) -> B256 {
    let mut preimage = [0_u8; 6];
    preimage[..4].copy_from_slice(&day.value().to_be_bytes());
    preimage[4..].copy_from_slice(&currency.to_be_bytes());
    keccak256(preimage)
}

pub(crate) fn record_from_verified(body: &VerifiedBody) -> Result<TributeRecord> {
    if let Some(encrypted) = body.payload().as_encrypted_tribute() {
        return Ok(TributeRecord::from_encrypted(encrypted.clone()));
    }
    let payload = body.payload().as_tribute().ok_or_else(|| {
        outbe_primitives::error::PrecompileError::BodyReadCorruption(
            "compressed-entity read returned a non-Tribute payload".into(),
        )
    })?;
    Ok(TributeRecord::from_legacy(
        crate::repository::from_canonical_body(payload.clone()),
    ))
}

pub(crate) fn tribute_from_verified(body: &VerifiedBody) -> Result<TributeData> {
    calculation_view(&record_from_verified(body)?)
}

fn calculation_view(record: &TributeRecord) -> Result<TributeData> {
    record.calculation_view().map_err(|error| {
        outbe_primitives::error::PrecompileError::Fatal(format!(
            "Tribute private amount read failed: {error}"
        ))
    })
}
