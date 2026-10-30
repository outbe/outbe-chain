use super::*;

pub(super) fn apply(
    contract: &mut TributeContract<'_>,
    day: WorldwideDay,
) -> Result<TributeForfeitureReceipt> {
    let mut totals = contract.get_day_totals(day)?;
    if !totals.initialized || !totals.is_sealed {
        return Err(TributeError::CapacityForfeitureRequiresSealedPartition.into());
    }
    if contract.retired_days.read(&day)? {
        return Err(TributeError::CapacityForfeitureRequiresUnretiredPartition.into());
    }
    let forfeited_count = totals.tribute_count;
    let forfeited_nominal = totals.tribute_nominal_total_minor;
    contract.freeze_day_nominal(&totals)?;
    contract.reset_day_nominal(&totals)?;
    totals.tribute_count = 0;
    totals.tribute_nominal_total_minor = U256::ZERO;
    contract.store_day_totals(&totals)?;
    let retirement_outcome = contract.retire_completed_partition_inner(day)?;
    if retirement_outcome != RetirementOutcome::Requested {
        return Err(TributeError::CapacityForfeitureRequiresUnretiredPartition.into());
    }
    Ok(TributeForfeitureReceipt {
        worldwide_day: day,
        forfeited_count,
        forfeited_nominal,
        source_generation: 0,
        retired_generation: 1,
        retirement_outcome,
    })
}

pub(super) fn resolve_price_day(
    contract: &TributeContract<'_>,
    day: WorldwideDay,
    completed_at: u64,
) -> Result<u32> {
    let recorded = contract.day_emission_day.read(&day)?;
    if recorded != 0 {
        return Ok(recorded);
    }
    outbe_nod::openings::entry_price_day(completed_at).map_err(date_step_error)
}

pub(super) fn seal_series_and_sum_exact_loads(
    contract: &TributeContract<'_>,
    day: WorldwideDay,
    lysis_limit_minor: U256,
    completed_at: u64,
    u256_fractions: &std::collections::BTreeMap<u16, U256>,
) -> Result<Option<U256>> {
    let ids = contract.get_tribute_ids_by_day(day)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let storage = contract.storage_handle();
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    let price_day = resolve_price_day(contract, day, completed_at)?;
    let nod_max: U256 = lysis_limit_minor >> 10usize;
    let scale = U256::from(1_000_000u64);
    let mut series_totals: std::collections::BTreeMap<(u16, u16), (u64, U256)> =
        std::collections::BTreeMap::new();
    for &tribute_id in &ids {
        let Some(record) = crate::state_api::get_retained_record(&storage, tribute_id)? else {
            continue;
        };
        let league = contract.tribute_leagues.read(&tribute_id)?.max(1);
        let fraction = u256_fractions.get(&league).copied().unwrap_or(U256::ZERO);
        let nominal = record
            .calculation_view()
            .map_err(TributeError::private_amount_read_failed)?
            .nominal_amount_minor;
        let load =
            outbe_primitives::math::scaled_math::checked_mul_div_floor(nominal, fraction, scale)?
                .min(nod_max);
        let entry = series_totals
            .entry((record.issuance_currency, record.reference_currency))
            .or_insert((0, U256::ZERO));
        entry.0 = entry
            .0
            .checked_add(1)
            .ok_or(TributeError::DayCountOverflowOrUnderflow)?;
        entry.1 = entry
            .1
            .checked_add(load)
            .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
    }
    let mut exact_total = U256::ZERO;
    for ((issuance_currency, reference_currency), (count, load)) in series_totals {
        let Some(vwap) = issuable_day_vwap(&storage, price_day, reference_currency)? else {
            continue;
        };
        exact_total = exact_total
            .checked_add(load)
            .ok_or(outbe_emission::errors::EmissionError::AmountOverflow)?;
        let group_key =
            outbe_nod::identity::series_group_key(price_day, issuance_currency, reference_currency);
        outbe_nodfactory::api::seal_lysis_series(
            &storage,
            outbe_nodfactory::api::LysisSeriesInput {
                group_key,
                emission_day: price_day,
                reference_currency,
                entry_price_minor: Some(vwap),
                unclaimed_tribute_count: count,
                unclaimed_tribute_load: load,
            },
        )?;
    }
    Ok(Some(exact_total.min(lysis_limit_minor)))
}

fn issuable_day_vwap(
    storage: &StorageHandle<'_>,
    price_day: u32,
    currency: u16,
) -> Result<Option<U256>> {
    if currency == 0 {
        return Ok(None);
    }
    Ok(
        outbe_oracle::api::get_utc_day_vwap_for_iso(storage.clone(), price_day, currency)?
            .filter(|&p| outbe_nod::pricing::is_issuable_entry(p)),
    )
}

pub(super) fn create_nod_from_tribute(
    contract: &mut TributeContract<'_>,
    caller: alloy_primitives::Address,
    tribute_id: WwdEntityId,
) -> Result<U256> {
    let storage = contract.storage_handle();
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    storage.with_checkpoint(|| {
        let day = tribute_id.worldwide_day();
        if contract.retired_days.read(&day)? {
            return Err(TributeError::TributeNotFound.into());
        }
        let completed_at = contract.day_metadosis_completed_at.read(&day)?;
        if completed_at == 0 {
            return Err(TributeError::MetadosisNotCompleted.into());
        }
        if storage.now_secs()? < completed_at {
            return Err(TributeError::NodCreationBeforeCompletion.into());
        }
        let record = crate::state_api::get_retained_record(&storage, tribute_id)?
            .ok_or(TributeError::TributeNotFound)?;
        if record.owner != caller {
            return Err(TributeError::CallerNotTributeOwner.into());
        }
        let raw_league = contract.tribute_leagues.read(&tribute_id)?;
        let league = raw_league.max(1);
        let fraction = U256::from(
            contract
                .day_fractions
                .read(&crate::schema::day_league_key(day, league))?,
        );
        let lysis_limit = contract.day_lysis_limit_minor.read(&day)?;
        let nominal = record
            .calculation_view()
            .map_err(TributeError::private_amount_read_failed)?
            .nominal_amount_minor;
        let gratis_load_minor = outbe_lysis::program_v1::calculate_gratis_load(
            nominal,
            fraction,
            lysis_limit >> 10usize,
            0,
        )
        .map_err(|e| TributeError::LysisArithmetic(e.to_string()))?;

        let (group_key, gratis_day_price) =
            resolve_series_key_and_price(contract, &storage, &record, completed_at)?;
        let encrypted_tribute = record
            .encrypted()
            .ok_or(TributeError::InvalidEncryptedEncoding)?;
        let encrypted_nod = crate::enclave_client::encrypt_nod_for_tribute(
            encrypted_tribute,
            outbe_primitives::nod_encryption::NodTermsV1 {
                chain_id: storage.chain_id()?,
                nod_id: outbe_primitives::nod_rules::derive_nod_id(
                    record.owner,
                    record.worldwide_day,
                ),
                owner: record.owner,
                worldwide_day: day,
                league_id: league,
                issuance_currency: record.issuance_currency,
                reference_currency: record.reference_currency,
                entry_price_minor: gratis_day_price,
            },
            gratis_load_minor,
        )
        .map_err(TributeError::private_amount_read_failed)?;
        let nod_id = outbe_nodfactory::api::issue_tribute_nod_in_series(
            &storage,
            &encrypted_nod,
            gratis_load_minor,
            group_key,
        )?;

        burn_live_tribute_record(contract, &record, raw_league, nominal)?;
        Ok(nod_id)
    })
}

fn resolve_series_key_and_price(
    contract: &TributeContract<'_>,
    storage: &StorageHandle<'_>,
    record: &crate::TributeRecord,
    completed_at: u64,
) -> Result<(alloy_primitives::B256, U256)> {
    let price_day = resolve_price_day(contract, record.worldwide_day, completed_at)?;
    let group_key = outbe_nod::identity::series_group_key(
        price_day,
        record.issuance_currency,
        record.reference_currency,
    );
    if outbe_nod::api::is_group_forfeited(storage, group_key)? {
        return Err(TributeError::TributeForfeited.into());
    }
    let sealed_price = outbe_nod::api::group_entry_price_minor(storage, group_key)?;
    let gratis_day_price = if !sealed_price.is_zero() {
        sealed_price
    } else {
        outbe_oracle::api::get_utc_day_vwap_for_iso(
            storage.clone(),
            price_day,
            record.reference_currency,
        )?
        .ok_or(TributeError::EntryPriceUnavailable {
            price_day,
            currency: record.reference_currency,
        })?
    };
    Ok((group_key, gratis_day_price))
}

fn burn_live_tribute_record(
    contract: &mut TributeContract<'_>,
    record: &crate::TributeRecord,
    raw_league: u16,
    nominal: U256,
) -> Result<WwdEntityId> {
    let tribute_id = record.tribute_id;
    let day = record.worldwide_day;
    contract.bump_day_bucket_record(record, false)?;
    if raw_league != 0 {
        contract.adjust_fi_group(day, raw_league, nominal, false)?;
        contract.tribute_leagues.clear(&tribute_id)?;
    }
    contract.delete_loaded_state_record(record)?;
    contract.emit(ITribute::TributeBurned {
        tributeId: tribute_id.to_u256(),
        owner: record.owner,
        worldwideDay: day.into(),
    })?;
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
        let (is_forfeited, series_key) =
            check_tribute_forfeited(contract, &storage, &record, retired)?;
        if !is_forfeited {
            return Err(TributeError::TributeNotForfeited.into());
        }
        if let Some(group_key) = series_key {
            outbe_nod::api::record_unclaimed_tribute_cleaned(&storage, group_key)?;
        }
        let raw_league = contract.tribute_leagues.read(&tribute_id)?;
        let nominal = record
            .calculation_view()
            .map_err(TributeError::private_amount_read_failed)?
            .nominal_amount_minor;
        if !retired {
            burn_live_tribute_record(contract, &record, raw_league, nominal)?;
            return Ok(());
        }
        if raw_league != 0 {
            contract.adjust_fi_group(day, raw_league, nominal, false)?;
            contract.tribute_leagues.clear(&tribute_id)?;
        }
        contract.delete_loaded_state_record(&record)?;
        contract.emit(ITribute::TributeBurned {
            tributeId: tribute_id.to_u256(),
            owner: record.owner,
            worldwideDay: day.into(),
        })
    })
}

fn check_tribute_forfeited(
    contract: &TributeContract<'_>,
    storage: &StorageHandle<'_>,
    record: &crate::TributeRecord,
    retired: bool,
) -> Result<(bool, Option<alloy_primitives::B256>)> {
    if retired {
        return Ok((true, None));
    }
    let day = record.worldwide_day;
    let completed_at = contract.day_metadosis_completed_at.read(&day)?;
    if completed_at == 0 {
        let recorded = contract.day_emission_day.read(&day)?;
        let candidate = if recorded != 0 {
            recorded
        } else {
            derive_wwd_emission_day(day)
        };
        let is_aborted = outbe_emission::api::get_emission_day_record(storage.clone(), candidate)?
            .is_some_and(|e| {
                matches!(
                    e.metadosis_state,
                    outbe_emission::schema::metadosis_state::CANCELLED
                        | outbe_emission::schema::metadosis_state::FAILED
                )
            });
        return Ok((is_aborted, None));
    }
    let price_day = resolve_price_day(contract, day, completed_at)?;
    let group_key = outbe_nod::identity::series_group_key(
        price_day,
        record.issuance_currency,
        record.reference_currency,
    );
    if outbe_nod::api::is_group_forfeited(storage, group_key)? {
        return Ok((true, Some(group_key)));
    }
    let unquoted = outbe_nod::api::group_entry_price_minor(storage, group_key)?.is_zero()
        && issuable_day_vwap(storage, price_day, record.reference_currency)?.is_none();
    Ok((unquoted, None))
}

fn derive_wwd_emission_day(day: WorldwideDay) -> u32 {
    let offering_end = day
        .start_timestamp()
        .saturating_add(outbe_chain_constants::get_metadosis_forming_period_seconds())
        .saturating_add(outbe_chain_constants::get_metadosis_lookback_delay_seconds())
        .saturating_add(outbe_chain_constants::get_metadosis_offering_period_seconds());
    outbe_primitives::time::timestamp_to_date_key(offering_end)
}
