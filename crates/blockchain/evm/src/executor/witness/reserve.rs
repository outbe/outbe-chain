//! Tape room the end-block suffix keeps against user transactions.
use super::{
    payload_len_fits_extra_data, tape_fits_budget, worst_case_base_extra_len,
    MAX_DECOMPRESSED_WITNESS_BYTES,
};
use outbe_tee::witness::{compressed_witness_bound, EnclaveWitnessTapeV1};

/// Raw tape bytes user transactions leave for the end-block suffix, which runs after them and
/// cannot be deferred: two passes of `SWEEP_WRITES_PER_BLOCK` Nod load reads of 168 bytes each.
pub const END_BLOCK_WITNESS_RESERVE_BYTES: usize = 22 * 1024;

/// A user candidate must leave the reserve. Compression does not add up, so only the
/// worst-case bound proves the reserve still fits.
pub(super) fn candidate_fits(
    tape: &EnclaveWitnessTapeV1,
    base_extra_data: &[u8],
    is_user_candidate: bool,
) -> bool {
    if !is_user_candidate {
        return tape_fits_budget(tape, base_extra_data);
    }
    let Ok(raw) = postcard::to_allocvec(tape) else {
        return false;
    };
    let reserved = raw.len().saturating_add(END_BLOCK_WITNESS_RESERVE_BYTES);
    if reserved > MAX_DECOMPRESSED_WITNESS_BYTES {
        return false;
    }
    let Some(base_len) = worst_case_base_extra_len(base_extra_data) else {
        return false;
    };
    payload_len_fits_extra_data(compressed_witness_bound(reserved), base_len)
}
