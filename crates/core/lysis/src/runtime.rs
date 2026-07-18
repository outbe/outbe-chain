use crate::algorithm::calc_fraction_distribution_fp;
use crate::constants::calc_floor_price;
use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    list, EntityId36, ExecutionScope, IdPageRequest, ParentBodySource, QueryRef, MAX_ID_PAGE_LIMIT,
};
use outbe_primitives::units::SCALE_1E18;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
    time::timestamp_to_date_key,
};

/// Result of a lysis execution.
pub struct LysisResult {
    pub nod_ids: Vec<EntityId36>,
    pub tribute_ids: Vec<EntityId36>,
    pub remaining_gratis: U256,
}

/// Executes lysis for a given worldwide day with the specified gratis allocation.
///
/// All arithmetic uses integer fixed-point math (no f32/f64).
///
/// 1. Loads all tributes for the day
/// 2. Groups by fidelity index
/// 3. Runs the distribution algorithm (fixed-point)
/// 4. Creates NODs for each tribute
/// 5. Leaves gratis unminted until a later NOD mine step
/// 6. Deletes processed tributes and clears the day index
pub fn lysis(
    storage: StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    wwd: WorldwideDay,
    auction_timestamp: u64,
    gratis_allocation: U256,
) -> Result<LysisResult> {
    storage.clone().with_checkpoint(|| {
        lysis_inner(
            storage,
            scope,
            parent,
            wwd,
            auction_timestamp,
            gratis_allocation,
        )
    })
}

fn lysis_inner(
    storage: StorageHandle,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    wwd: WorldwideDay,
    auction_timestamp: u64,
    gratis_allocation: U256,
) -> Result<LysisResult> {
    let mut tribute_contract = outbe_tribute::TributeContract::new(storage.clone());
    let tributes = load_day_tributes(storage.clone(), scope, parent, wwd)?;
    if tributes.is_empty() {
        return Ok(LysisResult {
            nod_ids: vec![],
            tribute_ids: vec![],
            remaining_gratis: gratis_allocation,
        });
    }

    let mut tribute_fis = Vec::with_capacity(tributes.len());
    let mut total_interest = U256::ZERO;
    for loaded in &tributes {
        let tribute = loaded.body();
        tribute_fis.push(outbe_fidelity::api::league(storage.clone(), tribute.owner)?);
        total_interest = total_interest
            .checked_add(tribute.nominal_amount_minor)
            .ok_or_else(|| {
                PrecompileError::BodyReadCorruption(
                    "Tribute nominal total overflow during Lysis".into(),
                )
            })?;
    }
    if total_interest.is_zero() {
        return Err(PrecompileError::BodyReadCorruption(
            "non-empty Tribute partition has zero nominal total".into(),
        ));
    }

    let nominal_amounts: Vec<U256> = tributes
        .iter()
        .map(|loaded| loaded.body().nominal_amount_minor)
        .collect();
    let fi_fraction_map = compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )?;
    let entry_price_minor_840 = resolve_entry_price_minor(storage.clone(), wwd, 840)?;

    let mut nod_ids = Vec::with_capacity(tributes.len());
    let mut tribute_ids = Vec::with_capacity(tributes.len());
    let mut remaining = gratis_allocation;
    let mut contributors = std::collections::BTreeMap::<Address, U256>::new();

    for (i, loaded) in tributes.iter().enumerate() {
        let tribute = loaded.body();
        tribute_ids.push(tribute.tribute_id);

        let fraction_fp = fi_fraction_map
            .get(&tribute_fis[i])
            .copied()
            .unwrap_or(U256::ZERO);
        let gratis_load = tribute.nominal_amount_minor * fraction_fp / SCALE_1E18;
        consume_required_gratis(&mut remaining, gratis_load)?;

        let entry_price_minor = match tribute.reference_currency {
            840 => entry_price_minor_840,
            currency => resolve_entry_price_minor(storage.clone(), wwd, currency)?,
        };
        let floor_price_minor =
            calc_floor_price(tribute.tribute_price_minor.max(entry_price_minor));
        let league_id = outbe_fidelity::api::league(storage.clone(), tribute.owner)?;
        let cost_amount_minor = entry_price_minor * gratis_load / SCALE_1E18;
        let nod_id = outbe_nodfactory::api::issue_nod(
            &storage,
            scope,
            parent,
            &outbe_nod::NodIssueParams {
                owner: tribute.owner,
                worldwide_day: wwd,
                league_id,
                floor_price_minor,
                gratis_load_minor: gratis_load,
                entry_price_minor,
                cost_amount_minor,
                issuance_currency: tribute.issuance_currency,
                reference_currency: tribute.reference_currency,
            },
        )?;
        nod_ids.push(nod_id);
        let entry = contributors.entry(tribute.owner).or_insert(U256::ZERO);
        *entry = entry
            .checked_add(tribute.nominal_amount_minor)
            .ok_or_else(|| {
                PrecompileError::BodyReadCorruption(
                    "Tribute contributor nominal overflow during Lysis".into(),
                )
            })?;
    }

    if nod_ids.len() != tributes.len() {
        return Err(PrecompileError::BodyReadCorruption(
            "Lysis Nod count does not match Tribute count".into(),
        ));
    }

    let list: Vec<(Address, U256)> = contributors.into_iter().collect();
    outbe_intex::api::record_contributors(
        &storage,
        timestamp_to_date_key(auction_timestamp),
        &list,
    )?;
    tribute_contract.consume_lysis_partition(
        wwd,
        u32::try_from(tributes.len()).map_err(|_| {
            PrecompileError::BodyReadCorruption("Tribute count exceeds u32 during Lysis".into())
        })?,
        total_interest,
    )?;

    Ok(LysisResult {
        nod_ids,
        tribute_ids,
        remaining_gratis: remaining,
    })
}

pub(crate) fn consume_required_gratis(remaining: &mut U256, gratis_load: U256) -> Result<()> {
    if gratis_load.is_zero() || gratis_load > *remaining {
        return Err(PrecompileError::BodyReadCorruption(
            "Lysis must issue exactly one non-zero Nod per Tribute".into(),
        ));
    }
    *remaining -= gratis_load;
    Ok(())
}

fn load_day_tributes(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    wwd: WorldwideDay,
) -> Result<Vec<outbe_tribute::LoadedTribute>> {
    let mut records = Vec::new();
    let mut after = None;
    loop {
        let page = list(
            storage.clone(),
            scope,
            parent,
            QueryRef::TributeByDay(wwd),
            IdPageRequest {
                after,
                limit: MAX_ID_PAGE_LIMIT,
            },
        )?;
        let next_after = page.next_after();
        let bodies = page.into_bodies();
        records.extend(
            bodies
                .into_iter()
                .map(outbe_tribute::LoadedTribute::from_verified)
                .collect::<Result<Vec<_>>>()?,
        );
        let Some(next) = next_after else {
            return Ok(records);
        };
        after = Some(next);
    }
}

/// Computes the FI → gratis-fraction map (fixed-point, SCALE = 10^18) from each
/// tribute's nominal amount and fidelity index. Pure integer math; deterministic
/// across nodes.
///
/// `nominal_amounts` and `tribute_fis` are index-aligned: entry `i` is the
/// nominal interest and fidelity index of the same tribute. `total_interest` is
/// the sum of all `nominal_amounts` (precomputed by the caller).
pub(crate) fn compute_fi_fraction_map(
    nominal_amounts: &[U256],
    tribute_fis: &[u16],
    total_interest: U256,
    gratis_allocation: U256,
) -> Result<std::collections::HashMap<u16, U256>> {
    // 3. Group tributes by fidelity index (sorted ascending for algorithm stability)
    let mut fi_groups: std::collections::BTreeMap<u16, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, &fi) in tribute_fis.iter().enumerate() {
        fi_groups.entry(fi).or_default().push(i);
    }

    // 4. Prepare distribution parameters in fixed-point (SCALE = 10^18)
    let sorted_fis: Vec<u16> = fi_groups.keys().copied().collect();
    let mut y_fp: Vec<U256> = Vec::with_capacity(sorted_fis.len());
    let mut p: Vec<u64> = Vec::with_capacity(sorted_fis.len());

    for &fi in &sorted_fis {
        let indices = &fi_groups[&fi];
        let group_interest: U256 = indices
            .iter()
            .map(|&i| nominal_amounts[i])
            .fold(U256::ZERO, |acc, v| acc + v);
        let share = group_interest * SCALE_1E18 / total_interest;
        y_fp.push(share);
        p.push(indices.len() as u64);
    }

    // normalize y_fp so sum == SCALE exactly. Integer division in the
    // loop above truncates each share; the missing delta is absorbed into the
    // last share. Deterministic because `sorted_fis` is BTreeMap-ordered on all
    // nodes. Guarantees the downstream `calc_fraction_distribution_fp` invariant
    // `sum(y_fp) == SCALE`.
    let y_sum: U256 = y_fp.iter().copied().sum();
    if let Some(last) = y_fp.last_mut() {
        if y_sum < SCALE_1E18 {
            *last += SCALE_1E18 - y_sum;
        }
        // y_sum > SCALE is unreachable: each share is ≤ group/total ≤ 1.
    }

    let nt = nominal_amounts.len();

    // 5. Deficit coefficient in fixed-point → derive per-FI floor and ceiling.
    // `f_fp` is bounded by `SCALE_1E18 ≈ 2^60` in normalized scenarios, so
    // doubling for `fmax_fp` is safe in U256 (no saturating needed).
    let f_fp = gratis_allocation * SCALE_1E18 / total_interest;
    let fmax_fp = f_fp * U256::from(2u64);

    // 6. Run distribution algorithm (pure integer)
    let fractions = calc_fraction_distribution_fp(&y_fp, &p, nt, f_fp, fmax_fp)?;

    // 7. Build FI → fraction map (fixed-point)
    let mut fi_fraction_map: std::collections::HashMap<u16, U256> =
        std::collections::HashMap::with_capacity(sorted_fis.len());
    for (i, &fi) in sorted_fis.iter().enumerate() {
        if let Some(&frac) = fractions.get(i) {
            fi_fraction_map.insert(fi, frac);
        }
    }

    Ok(fi_fraction_map)
}

fn resolve_entry_price_minor(
    storage: StorageHandle,
    worldwide_day: WorldwideDay,
    iso_code: u16,
) -> Result<U256> {
    let pair_id = outbe_oracle::api::get_pair_id(storage.clone(), iso_code)?;
    let vwap = outbe_oracle::api::get_worldwide_day_vwap_for_pair_id(
        storage.clone(),
        worldwide_day,
        pair_id,
    )?
    .unwrap_or(U256::ZERO);
    let max_scurve =
        outbe_oracle::api::get_max_active_scurve_value(storage, worldwide_day, pair_id)?;
    let nominal = vwap.max(max_scurve);
    if nominal.is_zero() {
        return Err(PrecompileError::Revert(
            "nominal price is zero: no VWAP or S-curve data available for this WorldwideDay".into(),
        ));
    }
    Ok(nominal)
}
