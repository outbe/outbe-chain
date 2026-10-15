use std::collections::BTreeMap;

use num_bigint::{BigInt, BigUint};
use num_traits::{Signed, Zero};
use serde_json::{json, Value};

use crate::{
    arithmetic::{decimal, fp_root, scale, to_i256, trunc_signed_div, wrap_u256},
    corpus::CorpusTribute,
    failure::ReferenceFailure,
};

fn fallback_tau(group_count: usize, tribute_count: usize) -> Result<BigUint, ReferenceFailure> {
    let unit = scale();
    let group_scaled = wrap_u256(BigUint::from(group_count) * &unit);
    let tribute_scaled = wrap_u256(BigUint::from(tribute_count) * &unit);
    let group_root = fp_root(&group_scaled, 5)?;
    let tribute_root = fp_root(&tribute_scaled, 10)?;
    Ok(wrap_u256(group_root * tribute_root) / unit)
}

fn policy_tau(
    populations: &[usize],
    tribute_count: usize,
) -> Result<Vec<BigUint>, ReferenceFailure> {
    let unit = scale();
    let group_count = populations.len();
    let mut tau = vec![BigUint::zero(); group_count + 1];
    for index in 1..group_count {
        let current = populations[index];
        let previous = populations[index - 1];
        tau[index] = if current != 0 && previous != 0 {
            let half_index = wrap_u256(BigUint::from(2 * index - 1) * &unit) / BigUint::from(2_u8);
            let numerator = fp_root(&half_index, 5)?;
            let current_root = fp_root(&wrap_u256(BigUint::from(current) * &unit), 10)?;
            let previous_root = fp_root(&wrap_u256(BigUint::from(previous) * &unit), 10)?;
            let divisor = current_root.min(previous_root);
            if divisor.is_zero() {
                fallback_tau(group_count, tribute_count)?
            } else {
                wrap_u256(numerator * &unit) / divisor
            }
        } else {
            fallback_tau(group_count, tribute_count)?
        };
    }
    let mut middle_sum = BigUint::zero();
    for value in &tau[1..group_count] {
        middle_sum = wrap_u256(middle_sum + value);
    }
    let policy_c = &unit * 2_u8 / 10_u8;
    tau[0] = wrap_u256(&policy_c * &middle_sum) / &unit;
    tau[group_count] = wrap_u256((&unit - policy_c) * middle_sum) / unit;
    Ok(tau)
}

fn masses(tau: &[BigUint], unit: &BigUint) -> Vec<BigUint> {
    let mut tau_total = BigUint::zero();
    for value in tau {
        tau_total = wrap_u256(tau_total + value);
    }
    tau.iter()
        .map(|value| {
            if tau_total.is_zero() {
                BigUint::zero()
            } else {
                wrap_u256(value * unit) / &tau_total
            }
        })
        .collect()
}

fn cumulative_shares(shares: &[BigUint], unit: &BigUint) -> Vec<BigUint> {
    let mut cumulative = vec![BigUint::zero(); shares.len() + 1];
    let mut running = BigUint::zero();
    for (index, value) in shares.iter().enumerate() {
        running = wrap_u256(running + value);
        cumulative[index + 1] = running.clone();
    }
    *cumulative.last_mut().expect("cumulative is non-empty") = unit.clone();
    cumulative
}

/// Returns the expected cumulative share and its variance.
fn moments(masses: &[BigUint], cumulative: &[BigUint], unit: &BigUint) -> (BigUint, BigUint) {
    let mut expected = BigUint::zero();
    let mut expected_square = BigUint::zero();
    for (mass, value) in masses.iter().zip(cumulative) {
        expected = wrap_u256(expected + wrap_u256(mass * value) / unit);
        let squared = wrap_u256(wrap_u256(mass * value) * value);
        let scale_square = wrap_u256(unit * unit);
        expected_square = wrap_u256(expected_square + squared / scale_square);
    }
    let expected_term = wrap_u256(&expected * &expected) / unit;
    let variance = if expected_square > expected_term {
        &expected_square - &expected_term
    } else {
        BigUint::zero()
    };
    (expected, variance)
}

struct Tilt<'a> {
    masses: &'a [BigUint],
    cumulative: &'a [BigUint],
    expected: BigInt,
    beta_numerator: BigInt,
    beta_denominator: BigInt,
    maximum: BigInt,
    scale: BigInt,
}

impl Tilt<'_> {
    fn tail_sum(&self, index: usize) -> Result<BigInt, ReferenceFailure> {
        let mut tail_sum = BigInt::zero();
        for tail in index..self.masses.len() {
            let difference = to_i256(&self.cumulative[tail])? - &self.expected;
            let beta_term = if self.beta_denominator.is_positive() {
                trunc_signed_div(&(&self.beta_numerator * difference), &self.beta_denominator)
            } else {
                BigInt::zero()
            };
            let factor = &self.scale + beta_term;
            tail_sum += trunc_signed_div(&(to_i256(&self.masses[tail])? * factor), &self.scale);
        }
        Ok(tail_sum)
    }

    fn fraction(&self, index: usize) -> Result<BigUint, ReferenceFailure> {
        let result = trunc_signed_div(&(&self.maximum * self.tail_sum(index)?), &self.scale);
        Ok(result.to_biguint().unwrap_or_default())
    }
}

fn cap_to_target(
    fractions: &mut [BigUint],
    shares: &[BigUint],
    target_fraction: &BigUint,
    unit: &BigUint,
) {
    let mut weighted_total = BigUint::zero();
    for (fraction, share) in fractions.iter().zip(shares) {
        weighted_total = wrap_u256(weighted_total + wrap_u256(fraction * share) / unit);
    }
    if weighted_total > *target_fraction {
        for value in fractions {
            *value = wrap_u256(&*value * target_fraction) / &weighted_total;
        }
    }
}

pub(crate) fn distribution(
    shares: &[BigUint],
    populations: &[usize],
    tribute_count: usize,
    target_fraction: &BigUint,
    maximum_fraction: &BigUint,
) -> Result<Vec<BigUint>, ReferenceFailure> {
    let unit = scale();
    if populations.is_empty() {
        return Ok(vec![BigUint::zero()]);
    }
    if populations.len() == 1 {
        return Ok(vec![target_fraction.clone()]);
    }

    let masses = masses(&policy_tau(populations, tribute_count)?, &unit);
    let cumulative = cumulative_shares(shares, &unit);
    let (expected, variance) = moments(&masses, &cumulative, &unit);
    let fraction_ratio = if maximum_fraction.is_zero() {
        BigUint::zero()
    } else {
        wrap_u256(target_fraction * &unit) / maximum_fraction
    };
    let expected = to_i256(&expected)?;
    let tilt = Tilt {
        masses: &masses,
        cumulative: &cumulative,
        beta_numerator: to_i256(&fraction_ratio)? - &expected,
        expected,
        beta_denominator: to_i256(&variance)?,
        maximum: to_i256(maximum_fraction)?,
        scale: BigInt::from(unit.clone()),
    };

    let mut fractions = (1..=shares.len())
        .map(|index| tilt.fraction(index))
        .collect::<Result<Vec<_>, _>>()?;
    cap_to_target(&mut fractions, shares, target_fraction, &unit);
    Ok(fractions)
}

pub(crate) fn fraction_table(
    tributes: &[CorpusTribute],
    total_nominal: &BigUint,
    allocation: &BigUint,
) -> Result<Vec<Value>, ReferenceFailure> {
    Ok(fraction_table_with_projection(tributes, total_nominal, allocation)?.0)
}

pub(crate) fn fraction_table_with_projection(
    tributes: &[CorpusTribute],
    total_nominal: &BigUint,
    allocation: &BigUint,
) -> Result<(Vec<Value>, BigUint, BigUint), ReferenceFailure> {
    let unit = scale();
    let mut groups = BTreeMap::<u16, Vec<usize>>::new();
    for (ordinal, tribute) in tributes.iter().enumerate() {
        groups
            .entry(
                tribute.f1.ok_or_else(|| {
                    ReferenceFailure::ordinal("FIDELITY_FIRST_UNAVAILABLE", ordinal)
                })?,
            )
            .or_default()
            .push(ordinal);
    }
    let mut shares = Vec::with_capacity(groups.len());
    let mut populations = Vec::with_capacity(groups.len());
    let mut group_nominals = Vec::with_capacity(groups.len());
    for ordinals in groups.values() {
        let mut group_nominal = BigUint::zero();
        for ordinal in ordinals {
            group_nominal = wrap_u256(group_nominal + decimal(&tributes[*ordinal].nominal)?);
        }
        shares.push(wrap_u256(&group_nominal * &unit) / total_nominal);
        populations.push(ordinals.len());
        group_nominals.push(group_nominal);
    }
    let mut share_sum = BigUint::zero();
    for value in &shares {
        share_sum = wrap_u256(share_sum + value);
    }
    if share_sum < unit {
        if let Some(last) = shares.last_mut() {
            *last = wrap_u256(&*last + (&unit - share_sum));
        }
    }
    let target = wrap_u256(allocation * &unit) / total_nominal;
    let maximum = wrap_u256(&target * 2_u8);
    // The distribution prioritizes its first group: highest league first.
    shares.reverse();
    populations.reverse();
    let mut fractions = distribution(&shares, &populations, tributes.len(), &target, &maximum)?;
    // Restore ascending league order for projection and the public table.
    fractions.reverse();
    shares.reverse();
    populations.reverse();
    let raw_projected = group_nominals
        .iter()
        .zip(&fractions)
        .fold(BigUint::zero(), |sum, (nominal, fraction)| {
            sum + wrap_u256(nominal * fraction) / &unit
        });
    if raw_projected > *allocation {
        for fraction in &mut fractions {
            *fraction = wrap_u256(&*fraction * allocation) / &raw_projected;
        }
    }
    let normalized_projected = group_nominals
        .iter()
        .zip(&fractions)
        .fold(BigUint::zero(), |sum, (nominal, fraction)| {
            sum + wrap_u256(nominal * fraction) / &unit
        });
    let table = groups
        .keys()
        .zip(shares)
        .zip(populations)
        .zip(fractions)
        .map(|(((league, share), population), fraction)| {
            json!({
                "league": league,
                "share": share.to_string(),
                "population": population,
                "fraction": fraction.to_string(),
            })
        })
        .collect();
    Ok((table, raw_projected, normalized_projected))
}
