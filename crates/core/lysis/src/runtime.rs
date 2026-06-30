use crate::algorithm::calc_fraction_distribution_fp;
use crate::constants::calc_floor_price;
use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::units::SCALE_1E18;
use outbe_primitives::{error::Result, storage::StorageHandle};

/// FI tree height constant used in the distribution algorithm.
const FI_TREE_HEIGHT: usize = 10;

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
/// 4. Creates NODs for each fundable tribute
/// 5. Leaves gratis unminted until a later NOD mine step
/// 6. Consumes the whole day: burns **every** tribute for the day and clears the
///    day index, even ones skipped for zero gratis-load or a missing reference
///    price. The day retires terminal exactly once and is never re-lysed, so a
///    tribute left in the index would strand there forever.
pub fn lysis(
    storage: StorageHandle,
    wwd: WorldwideDay,
    gratis_allocation: U256,
) -> Result<LysisResult> {
    let mut tribute_contract = outbe_tribute::TributeContract::new(storage.clone());

    // 1. Load all tributes for the day
    let tributes = tribute_contract.get_all_day_tributes(wwd)?;
    if tributes.is_empty() {
        return Ok(LysisResult {
            nod_ids: vec![],
            tribute_ids: vec![],
            remaining_gratis: gratis_allocation,
        });
    }

    // 2. Collect each owner's RCFI (the fidelity index) and total nominal interest
    let mut tribute_fis: Vec<u16> = Vec::with_capacity(tributes.len());
    let mut total_interest = U256::ZERO;

    for tribute in &tributes {
        let fi = outbe_fidelity::api::league(storage.clone(), tribute.owner)?;
        tribute_fis.push(fi);
        total_interest += tribute.nominal_amount_minor;
    }

    if total_interest.is_zero() {
        let tribute_ids = tributes.iter().map(|t| t.token_id).collect();
        // No interest to distribute against, but the day still retires terminal:
        // drain its tributes so none strand in the day index (consume-all).
        tribute_contract.burn_all_by_wwd(wwd)?;
        return Ok(LysisResult {
            nod_ids: vec![],
            tribute_ids,
            remaining_gratis: gratis_allocation,
        });
    }

    // 3-7. Compute the FI → gratis-fraction map from the tribute amounts.
    let nominal_amounts: Vec<U256> = tributes.iter().map(|t| t.nominal_amount_minor).collect();
    let fi_fraction_map = compute_fi_fraction_map(
        &nominal_amounts,
        &tribute_fis,
        total_interest,
        gratis_allocation,
    )?;

    // 9. Issue NODs for each tribute. Entry prices are resolved lazily per
    // reference currency (memoized below) — there is no single unconditional
    // resolve, so a missing reference price degrades to skipping that one tribute
    // instead of reverting the whole day.
    let mut nod_ids = Vec::with_capacity(tributes.len());
    let mut tribute_ids = Vec::with_capacity(tributes.len());
    let mut remaining = gratis_allocation;
    // Per-currency entry-price cache for this call. `Some(None)` records "resolved,
    // but no price" so a repeated unpriced currency is not re-read. `BTreeMap`
    // (not `HashMap`) keeps this lookup-only structure off the HashMap-on-consensus
    // rule even though it is never iterated.
    let mut price_cache: std::collections::BTreeMap<u16, Option<U256>> =
        std::collections::BTreeMap::new();

    for (i, tribute) in tributes.iter().enumerate() {
        tribute_ids.push(tribute.token_id);

        let fi = tribute_fis[i];
        let fraction_fp = fi_fraction_map.get(&fi).copied().unwrap_or(U256::ZERO);

        // gratis_load = fraction * nominal / SCALE (integer math)
        let gratis_load = tribute.nominal_amount_minor * fraction_fp / SCALE_1E18;

        if gratis_load.is_zero() || gratis_load > remaining {
            // Cannot cover this tribute — skip NOD issuance. The tribute is still
            // burned with the rest of the day below (consume-all).
            continue;
        }

        // Resolve the reference price (memoized). A missing price is routine
        // missing oracle data, not corruption: skip issuance, leave the gratis
        // unspent (it flows back as `remaining`), and let the tribute be consumed
        // below. Reverting here would roll back the begin-zone settlement tx and
        // halt the chain at this height. Genuine storage faults still propagate.
        let ccy = tribute.reference_currency;
        let entry_price_minor = match price_cache.get(&ccy) {
            Some(cached) => *cached,
            None => {
                let resolved = outbe_oracle::api::entry_price_minor(storage.clone(), wwd, ccy)?;
                price_cache.insert(ccy, resolved);
                resolved
            }
        };
        let Some(entry_price_minor) = entry_price_minor else {
            tracing::warn!(
                worldwide_day = u32::from(wwd),
                token_id = %tribute.token_id,
                reference_currency = ccy,
                "lysis: no reference price (VWAP/S-curve) for currency — tribute skipped and consumed"
            );
            continue;
        };

        remaining -= gratis_load;

        // floor_price = max(tribute_price, entry_price) * (1 + floor_rate 8%)
        let floor_price_minor =
            calc_floor_price(tribute.tribute_price_minor.max(entry_price_minor));

        // League tier (in `[minLeague, maxLeague]`) from the Fidelity module:
        // the owner's RCFI bucketed against the global synthetic-max ceiling at
        // the current block time. Replaces the former floor(RCFI-in-days) value.
        let league_id = outbe_fidelity::api::league(storage.clone(), tribute.owner)?;

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
    }
    // Bucket qualification is NOT written here. Buckets become qualified when
    // the COEN/0xUSD oracle exchange rate reaches bucket.floor_price_minor —
    // see `outbe_nod::runtime::NodContract::mine_gratis` for the price check
    // and `outbe_nod::hooks::NodLifecycle` (if present) for eager bulk scan.

    // Consume-all: burn every tribute for the day and clear the day index — both
    // the ones that issued a NOD and the ones skipped (zero gratis-load or a
    // missing reference price). The day retires terminal exactly once and is
    // never re-lysed, so any tribute left in the index would strand there
    // permanently. Each burn emits `TributeBurned`, so the consumption is
    // observable on-chain.
    tribute_contract.burn_all_by_wwd(wwd)?;

    Ok(LysisResult {
        nod_ids,
        tribute_ids,
        remaining_gratis: remaining,
    })
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
    let fractions = calc_fraction_distribution_fp(&y_fp, &p, FI_TREE_HEIGHT, nt, f_fp, fmax_fp)?;

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
