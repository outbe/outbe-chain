use crate::program_v1;
use alloy_primitives::U256;
use outbe_primitives::error::Result;

pub(crate) fn consume_required_gratis(remaining: &mut U256, gratis_load: U256) -> Result<()> {
    *remaining = program_v1::validate_required_gratis(*remaining, gratis_load, 0)?;
    Ok(())
}

/// Computes the FI -> gratis-fraction map (fixed-point, SCALE = 10^6) from each
/// tribute's nominal amount and fidelity index. It uses only integer math. The result is
/// deterministic across nodes.
///
/// `nominal_amounts` and `tribute_fis` are index-aligned: entry `i` is the
/// nominal interest and fidelity index of the same tribute. `total_interest` is
/// the sum of all `nominal_amounts` (the caller precomputes it).
pub(crate) fn compute_fi_fraction_map(
    nominal_amounts: &[U256],
    tribute_fis: &[u16],
    total_interest: U256,
    lysis_limit_minor: U256,
) -> Result<std::collections::HashMap<u16, U256>> {
    Ok(program_v1::compute_fraction_hash_map(
        nominal_amounts,
        tribute_fis,
        total_interest,
        lysis_limit_minor,
    )?)
}
