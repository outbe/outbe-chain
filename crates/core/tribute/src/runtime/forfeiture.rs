use super::*;

fn sealed_emission_day(contract: &TributeContract<'_>, day: WorldwideDay) -> Result<EmissionDate> {
    Ok(contract
        .lysis_emission_day(day)?
        .ok_or(TributeError::MetadosisNotCompleted)?)
}

/// Reference currencies priced for the emission day, with their entry price.
pub type PricedCurrencies = std::collections::BTreeMap<u16, U256>;

/// What the Lysis seal of a day gives back, in protocol minor units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LysisOutcome {
    /// Nominal of the priced Tributes.
    pub nominal_minor: U256,
    pub gratis_demand_minor: U256,
    pub lysis_limit_minor: U256,
    pub allocated_minor: U256,
}

type SeriesMetaMap = std::collections::BTreeMap<
    alloy_primitives::B256,
    (outbe_primitives::series_id::SeriesId, u16, u16, U256),
>;

fn collect_lysis_tributes(
    contract: &TributeContract<'_>,
    storage: &StorageHandle<'_>,
    day: WorldwideDay,
    price_day: EmissionDate,
    priced: &PricedCurrencies,
) -> Result<(
    Vec<outbe_tee::tribute_day::LysisTributeInputV1>,
    SeriesMetaMap,
)> {
    let mut tributes = Vec::new();
    let mut series_meta = SeriesMetaMap::new();
    let mut tribute_ids = contract.get_tribute_ids_by_day(day)?;
    tribute_ids.sort();
    for tribute_id in tribute_ids {
        let Some(record) = crate::state_api::get_retained_record(storage, tribute_id)? else {
            continue;
        };
        let encrypted = record
            .encrypted()
            .ok_or(TributeError::InvalidEncryptedEncoding)?
            .clone();
        let league = contract.tribute_leagues.read(&tribute_id)?.max(1);
        let series_scope =
            resolve_tribute_series_scope(&record, price_day, priced, &mut series_meta)?;
        tributes.push(outbe_tee::tribute_day::LysisTributeInputV1 {
            tribute: encrypted,
            league,
            series_scope,
        });
    }
    Ok((tributes, series_meta))
}

/// The series of a priced Tribute. A Tribute whose reference currency is not priced has none.
fn resolve_tribute_series_scope(
    record: &crate::TributeRecord,
    price_day: EmissionDate,
    priced: &PricedCurrencies,
    series_meta: &mut SeriesMetaMap,
) -> Result<Option<alloy_primitives::B256>> {
    let Some(&vwap) = priced.get(&record.reference_currency) else {
        return Ok(None);
    };
    let series_id = series_of(
        price_day,
        record.issuance_currency,
        record.reference_currency,
    )?;
    let scope = alloy_primitives::B256::right_padding_from(series_id.as_bytes());
    series_meta.entry(scope).or_insert((
        series_id,
        record.issuance_currency,
        record.reference_currency,
        vwap,
    ));
    Ok(Some(scope))
}

pub(super) fn compute_and_record_metadosis_fractions(
    contract: &mut TributeContract<'_>,
    (day, emission_day): (WorldwideDay, EmissionDate),
    priced: &PricedCurrencies,
    metadosis_limit_minor: U256,
    completed_at: u64,
) -> Result<LysisOutcome> {
    let storage = contract.storage_handle();
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    if contract.day_amount_record(day, true)?.is_none()
        && contract.day_amount_record(day, false)?.is_some()
    {
        let totals = contract.get_day_totals(day)?;
        contract.freeze_day_nominal(&totals)?;
    }
    match contract.lysis_emission_day(day)? {
        Some(linked) if linked != emission_day => {
            return Err(TributeError::LysisEmissionDayMismatch.into())
        }
        Some(_) => {}
        None => contract.day_emission_day.write(&day, emission_day)?,
    }
    let day_nominal = contract
        .day_amount_record(day, true)?
        .ok_or(TributeError::InvalidEncryptedDayIdentity)?;
    let (tributes, series_meta) =
        collect_lysis_tributes(contract, &storage, day, emission_day, priced)?;
    let result = crate::enclave_client::execute_lysis_fractions(
        outbe_tee::tribute_day::LysisFractionsRequestV1 {
            chain_id: storage.chain_id()?,
            worldwide_day: day,
            emission_day,
            metadosis_limit_minor,
            completed_at,
            day_nominal,
            tributes,
        },
    )
    .map_err(TributeError::DayEnclave)?;
    if result.day_fractions.worldwide_day != day {
        return Err(TributeError::DayResponseScopeMismatch.into());
    }
    contract
        .day_metadosis_completed_at
        .write(&emission_day, completed_at)?;
    contract.store_day_fractions_record(emission_day, &result.day_fractions)?;
    for zero_id in &result.zero_load_tribute_ids {
        contract.tribute_lysis_zero_load.write(zero_id, true)?;
    }
    for series_output in result.series_loads {
        let Some(&(series_id, issuance_currency, reference_currency, vwap)) =
            series_meta.get(&series_output.scope)
        else {
            return Err(TributeError::DayResponseScopeMismatch.into());
        };
        outbe_nodfactory::api::seal_lysis_series(
            &storage,
            outbe_nodfactory::api::LysisSeriesInput {
                series_id,
                sealed_at: completed_at,
                issuance_currency,
                reference_currency,
                entry_price_minor: vwap,
                unclaimed_tribute_count: series_output.unclaimed_tribute_count,
                unclaimed_tribute_load: series_output.unclaimed_tribute_load,
            },
        )?;
    }
    Ok(LysisOutcome {
        nominal_minor: result.total_nominal_minor,
        gratis_demand_minor: result.gratis_demand_minor,
        lysis_limit_minor: result.lysis_limit_minor,
        allocated_minor: result.allocated_minor,
    })
}

/// The Nod series of a Tribute's emission day and currencies.
fn series_of(
    price_day: EmissionDate,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<outbe_primitives::series_id::SeriesId> {
    outbe_primitives::series_id::SeriesId::for_pair(
        price_day.value(),
        issuance_currency,
        reference_currency,
    )
    .map_err(|_| TributeError::TributeSeriesInvalid.into())
}

fn require_issuable_tribute(
    contract: &TributeContract<'_>,
    storage: &StorageHandle<'_>,
    caller: alloy_primitives::Address,
    tribute_id: WwdEntityId,
) -> Result<crate::TributeRecord> {
    let day = tribute_id.worldwide_day();
    if contract.retired_days.read(&day)? {
        return Err(TributeError::TributeNotFound.into());
    }
    let completed_at = contract.lysis_completed_at(day)?;
    if completed_at == 0 {
        return Err(TributeError::MetadosisNotCompleted.into());
    }
    if storage.now_secs()? < completed_at {
        return Err(TributeError::NodIssueBeforeCompletion.into());
    }
    let record = crate::state_api::get_retained_record(storage, tribute_id)?
        .ok_or(TributeError::TributeNotFound)?;
    if record.owner != caller {
        return Err(TributeError::CallerNotTributeOwner.into());
    }
    if contract.tribute_lysis_zero_load.read(&tribute_id)? {
        return Err(TributeError::LysisArithmetic(
            outbe_lysis::errors::ProgramErrorV1::ZeroGratisLoad { ordinal: 0 }.to_string(),
        )
        .into());
    }
    Ok(record)
}

fn unwrap_issued_nod(
    outcome: outbe_tee::nod_mine::IssueNodFromTributeOutcomeV1,
) -> Result<outbe_primitives::nod_encryption::EncryptedNodV1> {
    match outcome {
        outbe_tee::nod_mine::IssueNodFromTributeOutcomeV1::Issued(nod) => Ok(nod),
        outbe_tee::nod_mine::IssueNodFromTributeOutcomeV1::LysisArithmetic(msg) => {
            Err(TributeError::LysisArithmetic(msg).into())
        }
        outbe_tee::nod_mine::IssueNodFromTributeOutcomeV1::ZeroSettlementCost => {
            Err(outbe_nodfactory::errors::NodFactoryError::EntryPriceOutOfBounds.into())
        }
    }
}

pub(super) fn issue_nod_from_tribute(
    contract: &mut TributeContract<'_>,
    caller: alloy_primitives::Address,
    tribute_id: WwdEntityId,
) -> Result<U256> {
    let storage = contract.storage_handle();
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    storage.with_checkpoint(|| {
        let day = tribute_id.worldwide_day();
        let record = require_issuable_tribute(contract, &storage, caller, tribute_id)?;
        let raw_league = contract.tribute_leagues.read(&tribute_id)?;
        let emission_day = sealed_emission_day(contract, day)?;
        let day_fractions = contract
            .day_fractions_record(emission_day)?
            .ok_or(TributeError::MetadosisNotCompleted)?;
        let (series_id, gratis_day_price) =
            resolve_series_and_price(&storage, &record, emission_day)?;
        let encrypted_tribute = record
            .encrypted()
            .ok_or(TributeError::InvalidEncryptedEncoding)?;
        let outcome = crate::enclave_client::issue_nod_from_tribute(
            outbe_tee::nod_mine::IssueNodFromTributeRequestV1 {
                tribute: encrypted_tribute.clone(),
                day_fractions,
                terms: outbe_primitives::nod_encryption::NodTermsV1 {
                    chain_id: storage.chain_id()?,
                    nod_id: outbe_core_common::nod_rules::derive_nod_id(series_id, record.owner),
                    owner: record.owner,
                    emission_day: series_id.day(),
                    league_id: raw_league.max(1),
                    issuance_currency: record.issuance_currency,
                    reference_currency: record.reference_currency,
                    entry_price_minor: gratis_day_price,
                },
            },
        )
        .map_err(TributeError::private_amount_read_failed)?;
        let encrypted_nod = unwrap_issued_nod(outcome)?;
        let nod_id = outbe_nodfactory::api::issue_tribute_nod_in_series(
            &storage,
            &encrypted_nod,
            series_id,
        )?;
        burn_live_tribute_record(contract, &record, raw_league)?;
        contract.emit(ITribute::TributeBurned {
            tributeId: tribute_id.to_u256(),
            owner: record.owner,
        })?;
        Ok(nod_id)
    })
}

/// The Tribute's series and the entry price Lysis sealed on it. A series Lysis left unsealed gives no Nod.
fn resolve_series_and_price(
    storage: &StorageHandle<'_>,
    record: &crate::TributeRecord,
    price_day: EmissionDate,
) -> Result<(outbe_primitives::series_id::SeriesId, U256)> {
    let series_id = series_of(
        price_day,
        record.issuance_currency,
        record.reference_currency,
    )?;
    let group_key =
        outbe_nod::api::series_group(storage, series_id)?.ok_or(TributeError::TributeForfeited)?;
    if outbe_nod::api::is_group_forfeited(storage, group_key)? {
        return Err(TributeError::TributeForfeited.into());
    }
    let sealed_price = outbe_nod::api::series_entry_price_minor(storage, series_id)?;
    Ok((series_id, sealed_price))
}

fn burn_live_tribute_record(
    contract: &mut TributeContract<'_>,
    record: &crate::TributeRecord,
    raw_league: u16,
) -> Result<WwdEntityId> {
    let tribute_id = record.tribute_id;
    contract.bump_day_bucket_record(record, false)?;
    if raw_league != 0 {
        contract.tribute_leagues.clear(&tribute_id)?;
    }
    contract.tribute_lysis_zero_load.clear(&tribute_id)?;
    contract.delete_loaded_state_record(record)?;
    Ok(tribute_id)
}

pub(super) fn clean_forfeited_tribute(
    contract: &mut TributeContract<'_>,
    tribute_id: WwdEntityId,
) -> Result<()> {
    let storage = contract.storage_handle();
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    storage.with_checkpoint(|| {
        let record = crate::state_api::get_retained_record(&storage, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        let day = record.worldwide_day;
        let retired = contract.retired_days.read(&day)?;
        let raw_league = contract.tribute_leagues.read(&tribute_id)?;
        let (is_forfeited, series_id) =
            check_tribute_forfeited(contract, &storage, &record, retired)?;
        if !is_forfeited {
            return Err(TributeError::TributeNotForfeited.into());
        }
        if let Some(series_id) = series_id {
            outbe_nod::api::record_unclaimed_tribute_cleaned(&storage, series_id)?;
        }
        if !retired {
            burn_live_tribute_record(contract, &record, raw_league)?;
            return Ok(());
        }
        if raw_league != 0 {
            contract.tribute_leagues.clear(&tribute_id)?;
        }
        contract.tribute_lysis_zero_load.clear(&tribute_id)?;
        contract.delete_loaded_state_record(&record)
    })
}

fn check_tribute_forfeited(
    contract: &TributeContract<'_>,
    storage: &StorageHandle<'_>,
    record: &crate::TributeRecord,
    retired: bool,
) -> Result<(bool, Option<outbe_primitives::series_id::SeriesId>)> {
    if retired {
        return Ok((true, None));
    }
    let day = record.worldwide_day;
    let completed_at = contract.lysis_completed_at(day)?;
    if completed_at == 0 {
        let Some(recorded) = contract.lysis_emission_day(day)? else {
            return Ok((false, None));
        };
        let is_aborted =
            outbe_emission::api::get_emission_day_record(storage.clone(), recorded.value())?
                .is_some_and(|e| {
                    matches!(
                        e.metadosis_state,
                        outbe_emission::schema::metadosis_state::CANCELLED
                            | outbe_emission::schema::metadosis_state::FAILED
                    )
                });
        return Ok((is_aborted, None));
    }
    if contract.tribute_lysis_zero_load.read(&record.tribute_id)? {
        return Ok((true, None));
    }
    let series_id = series_of(
        sealed_emission_day(contract, day)?,
        record.issuance_currency,
        record.reference_currency,
    )?;
    // A series Lysis left unsealed gives no Nod, so its Tributes are forfeited at once.
    match outbe_nod::api::series_group(storage, series_id)? {
        Some(group_key) => Ok((
            outbe_nod::api::is_group_forfeited(storage, group_key)?,
            Some(series_id),
        )),
        None => Ok((true, None)),
    }
}
