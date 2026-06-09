use crate::algorithm::{calc_fraction_distribution_fp, LYSIS_LIMIT_MAX, LYSIS_LIMIT_MIN};
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_oracle::{contract::OracleContract, scurve};
use outbe_primitives::units::{SCALE_1E18, SCALE_1E18_U128};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

/// FI tree height constant used in the distribution algorithm.
const FI_TREE_HEIGHT: usize = 10;

/// Fixed-point scale for algorithm interface (10^18).
const _SCALE: u128 = 1_000_000_000_000_000_000;

/// Result of a lysis execution.
pub struct LysisResult {
    pub nod_ids: Vec<U256>,
    pub tribute_ids: Vec<U256>,
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
    wwd: WorldwideDay,
    gratis_allocation: U256,
) -> Result<LysisResult> {
    let mut tribute_contract = outbe_tribute::TributeContract::new(storage.clone());
    let fidelity = outbe_fidelity::FidelityContract::new(storage.clone());

    // 1. Load all tributes for the day
    let tributes = tribute_contract.get_all_day_tributes(wwd)?;
    if tributes.is_empty() {
        return Ok(LysisResult {
            nod_ids: vec![],
            tribute_ids: vec![],
            remaining_gratis: gratis_allocation,
        });
    }

    // 2. Collect fidelity indices and compute total nominal interest
    let mut tribute_fis: Vec<u64> = Vec::with_capacity(tributes.len());
    let mut total_interest = U256::ZERO;

    for tribute in &tributes {
        let fi = fidelity.get_fidelity_index(tribute.owner)?;
        tribute_fis.push(fi);
        total_interest += tribute.nominal_amount_minor;
    }

    if total_interest.is_zero() {
        let tribute_ids = tributes.iter().map(|t| t.token_id).collect();
        return Ok(LysisResult {
            nod_ids: vec![],
            tribute_ids,
            remaining_gratis: gratis_allocation,
        });
    }

    // 3. Group tributes by fidelity index (sorted ascending for algorithm stability)
    let mut fi_groups: std::collections::BTreeMap<u64, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, &fi) in tribute_fis.iter().enumerate() {
        fi_groups.entry(fi).or_default().push(i);
    }

    // 4. Prepare distribution parameters in fixed-point (SCALE = 10^18)
    let sorted_fis: Vec<u64> = fi_groups.keys().copied().collect();
    let mut y_fp: Vec<u128> = Vec::with_capacity(sorted_fis.len());
    let mut p: Vec<u64> = Vec::with_capacity(sorted_fis.len());

    for &fi in &sorted_fis {
        let indices = &fi_groups[&fi];
        let group_interest: U256 = indices
            .iter()
            .map(|&i| tributes[i].nominal_amount_minor)
            .fold(U256::ZERO, |acc, v| acc + v);
        // y_fp = group_interest * SCALE / total_interest (integer division truncates)
        let share = (group_interest * SCALE_1E18 / total_interest).to::<u128>();
        y_fp.push(share);
        p.push(indices.len() as u64);
    }

    // normalize y_fp so sum == SCALE exactly. Integer division in the
    // loop above truncates each share; the missing delta is absorbed into the
    // last share. Deterministic because `sorted_fis` is BTreeMap-ordered on all
    // nodes. Guarantees the downstream `calc_fraction_distribution_fp` invariant
    // `sum(y_fp) == SCALE`.
    let y_sum: u128 = y_fp.iter().sum();
    if let Some(last) = y_fp.last_mut() {
        if y_sum < SCALE_1E18_U128 {
            *last += SCALE_1E18_U128 - y_sum;
        }
        // y_sum > SCALE is unreachable: each share is ≤ group/total ≤ 1.
    }

    let nt = tributes.len();

    // 5. Deficit coefficient in fixed-point → clamp to [LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX/2]
    // A-25: Clamp at U256 level before downcast to avoid silent u128 truncation.
    // A-27: .clamp(MIN, MAX/2) — MIN is the floor guarantee, MAX/2 is the ceiling.
    let deficit_u256 = gratis_allocation * SCALE_1E18 / total_interest;
    let deficit_fp = deficit_u256.min(U256::from(u128::MAX)).to::<u128>();
    let f_fp = deficit_fp.clamp(LYSIS_LIMIT_MIN, LYSIS_LIMIT_MAX / 2);
    let fmax_fp = LYSIS_LIMIT_MAX;

    // 6. Run distribution algorithm (pure integer)
    let fractions = calc_fraction_distribution_fp(&y_fp, &p, FI_TREE_HEIGHT, nt, f_fp, fmax_fp)?;

    // 7. Build FI → fraction map (fixed-point)
    let mut fi_fraction_map: std::collections::HashMap<u64, u128> =
        std::collections::HashMap::with_capacity(sorted_fis.len());
    for (i, &fi) in sorted_fis.iter().enumerate() {
        if let Some(&frac) = fractions.get(i) {
            fi_fraction_map.insert(fi, frac);
        }
    }

    // 8. Resolve entry_price_minor
    let entry_price_minor = resolve_entry_price_minor(storage.clone(), wwd)?;

    // 9. Issue NODs for each tribute
    let mut nod_ids = Vec::with_capacity(tributes.len());
    let mut tribute_ids = Vec::with_capacity(tributes.len());
    // A-39: Track which tribute token_ids were successfully processed.
    let mut processed_tribute_ids: Vec<U256> = Vec::with_capacity(tributes.len());
    let mut remaining = gratis_allocation;

    for (i, tribute) in tributes.iter().enumerate() {
        tribute_ids.push(tribute.token_id);

        let fi = tribute_fis[i];
        let fraction_fp = fi_fraction_map.get(&fi).copied().unwrap_or(0);

        // gratis_load = fraction * nominal / SCALE (integer math)
        let gratis_load = tribute.nominal_amount_minor * U256::from(fraction_fp) / SCALE_1E18;

        if gratis_load.is_zero() || gratis_load > remaining {
            // Cannot cover this tribute — skip NOD issuance
            continue;
        }

        remaining -= gratis_load;

        // floor_price = max(tribute_price, entry_price) * (1 + floor_rate 8%)
        let floor_basis = tribute.tribute_price_minor.max(entry_price_minor);
        let floor_price_minor = floor_basis * U256::from(108u64) / U256::from(100u64);

        // fidelity is capped to u32::MAX on write (see
        // `outbe_fidelity::FidelityContract::set_fidelity_index`), so the
        // conversion cannot truncate. Guard remains for defense in depth.
        let league_id = u32::try_from(fi).map_err(|_| {
            PrecompileError::Revert(format!("lysis: fidelity index {fi} exceeds u32::MAX"))
        })?;

        // cost_amount = cost_of_gratis * gratis_load / SCALE — both inputs are
        // 10^18-scaled (oracle price × minor units); divide once to land in
        // minor units.
        let cost_amount_minor = entry_price_minor * gratis_load / SCALE_1E18;
        let nod_id = outbe_nodfactory::api::issue_nod(
            &storage,
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
        processed_tribute_ids.push(tribute.token_id);
    }
    // Bucket qualification is NOT written here. Buckets become qualified when
    // the COEN/0xUSD oracle exchange rate reaches bucket.floor_price_minor —
    // see `outbe_nod::runtime::NodContract::mine_gratis` for the price check
    // and `outbe_nod::hooks::NodLifecycle` (if present) for eager bulk scan.

    // A-39: Only delete tributes that were successfully processed (NOD issued).
    // Skipped tributes are preserved for potential reprocessing.
    for token_id in &processed_tribute_ids {
        tribute_contract.burn(*token_id)?;
    }
    // Only clear the day index if ALL tributes were processed.
    if processed_tribute_ids.len() == tributes.len() {
        tribute_contract.clear_day_index(wwd)?;
    }

    Ok(LysisResult {
        nod_ids,
        tribute_ids,
        remaining_gratis: remaining,
    })
}

fn resolve_entry_price_minor(storage: StorageHandle, worldwide_day: WorldwideDay) -> Result<U256> {
    let oracle = OracleContract::new(storage);
    let pair_id = oracle.get_pair_id("COEN", "0xUSD")?;
    if pair_id == 0 {
        return Err(PrecompileError::Revert(
            "oracle pair COEN/0xUSD not registered".into(),
        ));
    }

    let vwap = oracle
        .get_worldwide_day_vwap_for_pair_id(worldwide_day, pair_id)?
        .unwrap_or(U256::ZERO);
    let scurve_timestamp = worldwide_day_to_utc_timestamp(worldwide_day);
    let max_scurve = scurve::get_max_active_scurve_value(&oracle, pair_id, scurve_timestamp)?;
    let nominal = vwap.max(max_scurve);
    if nominal.is_zero() {
        return Err(PrecompileError::Revert(
            "nominal price is zero: no VWAP or S-curve data available for this WorldwideDay".into(),
        ));
    }
    Ok(nominal)
}

fn worldwide_day_to_utc_timestamp(worldwide_day: WorldwideDay) -> u64 {
    let worldwide_day: u32 = worldwide_day.into();
    let year = (worldwide_day / 10_000) as i64;
    let month = ((worldwide_day / 100) % 100) as i64;
    let day = (worldwide_day % 100) as i64;
    let days = days_from_civil(year, month, day);
    (days as u64) * 24 * 3600
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
