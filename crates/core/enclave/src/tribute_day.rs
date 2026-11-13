//! Stateless encrypted day arithmetic. Each write binds the exact predecessor.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_core_common::tribute_day_encryption::{
    EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1,
};
use outbe_system_common::time::{EmissionDate, WorldwideDay};
use ring::hmac;
use zeroize::Zeroizing;

use crate::{
    confidential::SlotCipherDomain,
    errors::{Result, TeeError, TributeDayRejectReason},
    host::tribute_day::{
        day_operation_inputs_hash, lysis_fractions_inputs_hash, LysisFractionsRequestV1,
        LysisFractionsResultV1, LysisSeriesLoadOutputV1, LysisTributeInputV1,
        TributeDayOpRequestV2, TributeDayOperationV2,
    },
    tribute_encryption::decrypt_tribute,
};

const DAY_AMOUNT: SlotCipherDomain = SlotCipherDomain {
    nonce_info: b"outbe/tribute/day/nonce/v2",
};
const DAY_FRACTIONS: SlotCipherDomain = SlotCipherDomain {
    nonce_info: b"outbe/tribute/day-fractions/nonce/v1",
};

fn sign_operation_hash(secret: &[u8; 32], domain: &[u8], inputs_hash: B256) -> B256 {
    let mut operation_context = domain.to_vec();
    operation_context.extend_from_slice(inputs_hash.as_slice());
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, secret),
        &operation_context,
    );
    B256::from_slice(tag.as_ref())
}

pub fn read_day_amount(
    network_secret: &[u8; 32],
    record: &EncryptedTributeDayAmountV2,
) -> Result<U256> {
    DAY_AMOUNT
        .slot(network_secret, record.crypto_slot(), 0)
        .read_u256(record.version(), &record.encrypted_amount)
}

pub fn apply_day_operation(
    network_secret: &[u8; 32],
    tribute_state_key: &[u8; 32],
    request: &TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2> {
    let (previous_version, amount) = match &request.previous {
        None => (0, U256::ZERO),
        Some(previous) => {
            if previous.chain_id != request.chain_id
                || previous.worldwide_day != request.worldwide_day
                || previous.frozen
            {
                return Err(TributeDayRejectReason::InvalidDayPredecessor.into());
            }
            (
                previous.version().ok_or(TeeError::DecryptFailed)?,
                read_day_amount(network_secret, previous)?,
            )
        }
    };
    let next = apply_amount(tribute_state_key, request, amount)?;
    let inputs_hash = day_operation_inputs_hash(request).map_err(|_| TeeError::EncryptFailed)?;
    let mut record = EncryptedTributeDayAmountV2 {
        chain_id: request.chain_id,
        worldwide_day: request.worldwide_day,
        frozen: matches!(request.operation, TributeDayOperationV2::Freeze),
        operation_hash: sign_operation_hash(
            network_secret,
            b"outbe/tribute/day-operation/v2",
            inputs_hash,
        ),
        encrypted_amount: Vec::new(),
    };
    record.encrypted_amount = DAY_AMOUNT
        .slot(network_secret, record.crypto_slot(), 0)
        .write_u256(previous_version, next)?;
    Ok(record)
}

fn apply_amount(
    tribute_state_key: &[u8; 32],
    request: &TributeDayOpRequestV2,
    amount: U256,
) -> Result<U256> {
    let (delta, add) = match &request.operation {
        TributeDayOperationV2::Adjust { tribute, add } => {
            if tribute.context.chain_id != request.chain_id
                || tribute.context.worldwide_day != request.worldwide_day
            {
                return Err(TributeDayRejectReason::TributeWrongDayOrChain.into());
            }
            let amounts = decrypt_tribute(tribute_state_key, tribute)?;
            if amounts.issuance_amount_minor.is_zero() {
                return Err(TributeDayRejectReason::IssuanceAmountMustBePositive.into());
            }
            (amounts.nominal_amount_minor, *add)
        }
        TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor,
            add,
        } => (*nominal_amount_minor, *add),
        TributeDayOperationV2::Freeze => return Ok(amount),
    };
    if add {
        amount.checked_add(delta)
    } else {
        amount.checked_sub(delta)
    }
    .ok_or_else(|| TributeDayRejectReason::DayNominalAmountOverflowOrUnderflow.into())
}

pub fn seal_day_fractions(
    network_secret: &[u8; 32],
    (chain_id, worldwide_day, emission_day, completed_at): (u64, WorldwideDay, EmissionDate, u64),
    inputs_hash: B256,
    fractions: &BTreeMap<u16, u64>,
) -> Result<EncryptedTributeDayFractionsV1> {
    let mut record = EncryptedTributeDayFractionsV1 {
        chain_id,
        worldwide_day,
        emission_day,
        completed_at,
        operation_hash: sign_operation_hash(
            network_secret,
            b"outbe/tribute/day-fractions-operation/v1",
            inputs_hash,
        ),
        encrypted_fractions: Vec::new(),
    };
    let plaintext =
        Zeroizing::new(postcard::to_allocvec(fractions).map_err(|_| TeeError::EncryptFailed)?);
    record.encrypted_fractions = DAY_FRACTIONS
        .slot(network_secret, record.crypto_slot(), 0)
        .write_blob(0, plaintext.as_ref())?;
    Ok(record)
}

pub fn read_day_fractions(
    network_secret: &[u8; 32],
    record: &EncryptedTributeDayFractionsV1,
) -> Result<BTreeMap<u16, u64>> {
    if record.version() != Some(1) {
        return Err(TributeDayRejectReason::InvalidDayFractionsRecord.into());
    }
    let (_, bytes) = DAY_FRACTIONS
        .slot(network_secret, record.crypto_slot(), 0)
        .read_blob(&record.encrypted_fractions)?;
    let bytes = Zeroizing::new(bytes);
    postcard::from_bytes(bytes.as_slice()).map_err(|_| TeeError::DecryptFailed)
}

fn sum_allocated_gratis_from_groups(
    fi_groups: &BTreeMap<u16, (u32, U256)>,
    u256_fractions: &BTreeMap<u16, U256>,
    lysis_limit_minor: U256,
) -> U256 {
    let scale = U256::from(1_000_000u64);
    let mut total_allocated = U256::ZERO;
    for (&league, &(count, nominal_sum)) in fi_groups {
        let fraction = u256_fractions.get(&league).copied().unwrap_or(U256::ZERO);
        if count == 0 || nominal_sum.is_zero() || fraction.is_zero() {
            continue;
        }
        let league_raw = outbe_system_common::math::scaled_math::checked_mul_div_floor(
            nominal_sum,
            fraction,
            scale,
        )
        .unwrap_or(lysis_limit_minor);
        total_allocated = total_allocated.saturating_add(league_raw);
    }
    total_allocated.min(lysis_limit_minor)
}

type LeagueGroupMap = BTreeMap<u16, (u32, U256)>;
type LysisEvaluation = (
    Vec<outbe_core_common::wwd_entity_id::WwdEntityId>,
    Vec<LysisSeriesLoadOutputV1>,
    Option<U256>,
);

struct PricedTributes<'a> {
    tributes: Vec<(&'a LysisTributeInputV1, B256, U256)>,
    groups: LeagueGroupMap,
    count: u32,
    nominal: U256,
}

fn collect_priced_tributes<'a>(
    network_secret: &[u8; 32],
    tribute_state_key: &[u8; 32],
    request: &'a LysisFractionsRequestV1,
) -> Result<PricedTributes<'a>> {
    let day_nominal = &request.day_nominal;
    if day_nominal.chain_id != request.chain_id
        || day_nominal.worldwide_day != request.worldwide_day
        || !day_nominal.frozen
    {
        return Err(TributeDayRejectReason::InvalidDayPredecessor.into());
    }
    let frozen_nominal = read_day_amount(network_secret, day_nominal)?;
    let mut day_total = U256::ZERO;
    let mut previous_id = None;
    let mut priced = PricedTributes {
        tributes: Vec::new(),
        groups: BTreeMap::new(),
        count: 0,
        nominal: U256::ZERO,
    };
    let overflow = || TributeDayRejectReason::LysisFractionComputationFailed;
    for input in &request.tributes {
        if input.tribute.context.chain_id != request.chain_id
            || input.tribute.context.worldwide_day != request.worldwide_day
        {
            return Err(TributeDayRejectReason::TributeWrongDayOrChain.into());
        }
        let tribute_id = input.tribute.context.tribute_id;
        if previous_id.is_some_and(|previous| previous >= tribute_id) {
            return Err(TributeDayRejectReason::LysisTributesDifferFromDayNominal.into());
        }
        previous_id = Some(tribute_id);
        let nominal = decrypt_tribute(tribute_state_key, &input.tribute)?.nominal_amount_minor;
        day_total = day_total.checked_add(nominal).ok_or_else(overflow)?;
        let Some(scope) = input.series_scope else {
            continue;
        };
        let group = priced
            .groups
            .entry(input.league.max(1))
            .or_insert((0, U256::ZERO));
        group.0 = group.0.checked_add(1).ok_or_else(overflow)?;
        group.1 = group.1.checked_add(nominal).ok_or_else(overflow)?;
        priced.count = priced.count.checked_add(1).ok_or_else(overflow)?;
        priced.nominal = priced.nominal.checked_add(nominal).ok_or_else(overflow)?;
        priced.tributes.push((input, scope, nominal));
    }
    if day_total != frozen_nominal {
        return Err(TributeDayRejectReason::LysisTributesDifferFromDayNominal.into());
    }
    Ok(priced)
}

fn evaluate_tributes_for_lysis(
    nod_state_key: &[u8; 32],
    request: &LysisFractionsRequestV1,
    priced: &PricedTributes<'_>,
    u256_fractions: &BTreeMap<u16, U256>,
    (lysis_limit_minor, inputs_hash): (U256, B256),
) -> Result<LysisEvaluation> {
    if priced.tributes.is_empty() {
        return Ok((Vec::new(), Vec::new(), None));
    }
    let scale = U256::from(1_000_000u64);
    let mut zero_load_tribute_ids = Vec::new();
    let mut series_totals: BTreeMap<B256, (u64, U256)> = BTreeMap::new();
    for (input, scope, nominal) in &priced.tributes {
        let fraction = u256_fractions
            .get(&input.league.max(1))
            .copied()
            .unwrap_or(U256::ZERO);
        let load = outbe_system_common::math::scaled_math::checked_mul_div_floor(
            *nominal, fraction, scale,
        )
        .map_err(|_| TributeDayRejectReason::LysisFractionComputationFailed)?;
        if load.is_zero() {
            zero_load_tribute_ids.push(input.tribute.context.tribute_id);
        } else if !lysis_limit_minor.is_zero() {
            let entry = series_totals.entry(*scope).or_insert((0, U256::ZERO));
            entry.0 = entry
                .0
                .checked_add(1)
                .ok_or(TributeDayRejectReason::LysisFractionComputationFailed)?;
            entry.1 = entry
                .1
                .checked_add(load)
                .ok_or(TributeDayRejectReason::LysisFractionComputationFailed)?;
        }
    }
    if lysis_limit_minor.is_zero() {
        return Ok((zero_load_tribute_ids, Vec::new(), None));
    }
    let (series_loads, exact_total) =
        seal_series_loads(nod_state_key, request.chain_id, inputs_hash, series_totals)?;
    Ok((
        zero_load_tribute_ids,
        series_loads,
        Some(exact_total.min(lysis_limit_minor)),
    ))
}

fn seal_series_loads(
    nod_state_key: &[u8; 32],
    chain_id: u64,
    inputs_hash: B256,
    series_totals: BTreeMap<B256, (u64, U256)>,
) -> Result<(Vec<LysisSeriesLoadOutputV1>, U256)> {
    let mut series_loads = Vec::with_capacity(series_totals.len());
    let mut exact_total = U256::ZERO;
    for (scope, (count, load)) in series_totals {
        exact_total = exact_total
            .checked_add(load)
            .ok_or(TributeDayRejectReason::LysisFractionComputationFailed)?;
        let sealed_load = crate::nod_encryption::seal_group_load(
            nod_state_key,
            (chain_id, scope),
            (0, inputs_hash),
            load,
        )?;
        series_loads.push(LysisSeriesLoadOutputV1 {
            scope,
            unclaimed_tribute_count: count,
            unclaimed_tribute_load: sealed_load,
        });
    }
    Ok((series_loads, exact_total))
}

pub fn execute_lysis_fractions(
    network_secret: &[u8; 32],
    tribute_state_key: &[u8; 32],
    nod_state_key: &[u8; 32],
    request: &LysisFractionsRequestV1,
) -> Result<LysisFractionsResultV1> {
    let priced = collect_priced_tributes(network_secret, tribute_state_key, request)?;
    let (gratis_demand_minor, lysis_limit_minor) =
        outbe_lysis::day_limit::lysis_limit_of_day(priced.nominal, request.metadosis_limit_minor)
            .ok_or(TributeDayRejectReason::LysisFractionComputationFailed)?;
    let u256_fractions = if priced.count == 0 || priced.nominal.is_zero() {
        BTreeMap::new()
    } else {
        outbe_lysis::program_v1::compute_fraction_map_from_groups(
            &priced.groups,
            priced.count,
            priced.nominal,
            lysis_limit_minor,
        )
        .map_err(|_| TributeDayRejectReason::LysisFractionComputationFailed)?
    };
    let fractions: BTreeMap<u16, u64> = u256_fractions
        .iter()
        .map(|(&league, &frac)| (league, u64::try_from(frac).unwrap_or(u64::MAX)))
        .collect();
    let inputs_hash = lysis_fractions_inputs_hash(request).map_err(|_| TeeError::EncryptFailed)?;
    let day_fractions = seal_day_fractions(
        network_secret,
        (
            request.chain_id,
            request.worldwide_day,
            request.emission_day,
            request.completed_at,
        ),
        inputs_hash,
        &fractions,
    )?;
    let default_allocated =
        sum_allocated_gratis_from_groups(&priced.groups, &u256_fractions, lysis_limit_minor);
    let (zero_load_tribute_ids, series_loads, exact_allocated) = evaluate_tributes_for_lysis(
        nod_state_key,
        request,
        &priced,
        &u256_fractions,
        (lysis_limit_minor, inputs_hash),
    )?;
    Ok(LysisFractionsResultV1 {
        day_fractions,
        total_nominal_minor: priced.nominal,
        gratis_demand_minor,
        lysis_limit_minor,
        allocated_minor: exact_allocated.unwrap_or(default_allocated),
        zero_load_tribute_ids,
        series_loads,
    })
}
