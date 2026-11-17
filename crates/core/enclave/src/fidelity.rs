//! Enclave-side Fidelity cohort engine (secret-bearing).
//!
//! The Fidelity cohort ledger (the Gratis movement history) lives on-chain as
//! one AEAD blob per account under the [`crate::confidential::FIDELITY`]
//! domain. Thus its keys are cryptographically independent from Gratis's and
//! Promis's. This module is the only place cohorts exist in plaintext:
//!
//! - Cohort mutations ride inside the co-located Gratis op
//!   ([`apply_cohort_section`], from the `ApplyGratisOp` dispatch).
//! - The once-per-WWD metadosis league snapshot batch-decrypts the day's
//!   owners ([`snapshot_leagues`]).
//! - Owner-authorized `eth_call` queries evaluate RCFI in place
//!   ([`query_index`], gated by a signed, expiring authorization, never by a
//!   raw view key).
//!
//! The RCFI arithmetic is `outbe_fidelity_math`. This is the exact accumulator
//! that the chain historically ran over plaintext cohorts, so the two evaluation
//! paths cannot drift. Every function is a pure transform of its inputs + the
//! resident state key (consensus determinism). Business failures return
//! structured errors, never panics.

use alloy_primitives::{Address, B256, U256};
use outbe_tee::resident::{ResidentOfferMaterial, StateKeyHandle};

use crate::protocol::{
    eip191_hash, fidelity_cohort_canonical_hash, fidelity_query_auth_message,
    fidelity_query_canonical_hash, FidelityCohortOp, FidelityCohortRequest, FidelityCohortResult,
    FidelityLeagueEntry, FidelityOpOutcome, FidelityOpSection, FidelityQueryRequest,
    FidelityQueryResult, FidelitySnapshotRequest,
};
use outbe_fidelity_math::{league_from_rcfi, t_dec, RcfiAccumulator};

use crate::confidential::FIDELITY;
use crate::errors::{FidelityEnclaveError, Result};
use crate::fidelity_cipher::{self, CohortTransition};
use zeroize::Zeroizing;

/// Interior header: `qualified_start(8) || active_count(4) || sold_count(4)`.
const HEADER_LEN: usize = 16;
/// `size(32) || acquired_at(8)`.
const ACTIVE_ENTRY_LEN: usize = 40;
/// `size(32) || acquired_at(8) || sold_at(8)`.
const SOLD_ENTRY_LEN: usize = 48;
/// Smallest padding bucket (in combined cohort count).
const MIN_BUCKET: usize = 8;

/// Derive the resident Fidelity state key from the DKG group signature. See
/// [`crate::confidential::Domain::derive_state_key`].
pub fn derive_fidelity_state_key(
    material: &ResidentOfferMaterial,
    chain_id: B256,
    epoch: u64,
) -> Result<StateKeyHandle> {
    FIDELITY.derive_state_key(material, chain_id, epoch)
}

/// Plaintext cohort ledger of one account - the decrypted blob interior.
///
/// `active` is a LIFO stack of acquisitions `(size, acquired_at)`. `sold` is an
/// append-only log `(size, acquired_at, sold_at)`. Semantics are a 1:1 port of
/// the historical on-chain `FidelityContract::cohort_in/cohort_out`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct CohortState {
    qualified_start: u64,
    active: Vec<(U256, u64)>,
    sold: Vec<(U256, u64, u64)>,
}

fn read_u64_be(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(buf)
}

fn read_u32_be(bytes: &[u8]) -> usize {
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    u32::from_be_bytes(buf) as usize
}

impl CohortState {
    /// Decode a blob interior. Empty interior = fresh account. Trailing bytes
    /// beyond the declared entries are padding and ignored.
    fn decode(interior: &[u8]) -> Result<Self> {
        if interior.is_empty() {
            return Ok(Self::default());
        }
        if interior.len() < HEADER_LEN {
            return Err(FidelityEnclaveError::CohortInteriorShorterThanHeader.into());
        }
        let qualified_start = read_u64_be(&interior[..8]);
        let active_count = read_u32_be(&interior[8..12]);
        let sold_count = read_u32_be(&interior[12..16]);

        let active_bytes = active_count
            .checked_mul(ACTIVE_ENTRY_LEN)
            .ok_or(FidelityEnclaveError::CohortCountsOverflow)?;
        let sold_bytes = sold_count
            .checked_mul(SOLD_ENTRY_LEN)
            .ok_or(FidelityEnclaveError::CohortCountsOverflow)?;
        let sold_start = HEADER_LEN
            .checked_add(active_bytes)
            .ok_or(FidelityEnclaveError::CohortCountsOverflow)?;
        let need = sold_start
            .checked_add(sold_bytes)
            .ok_or(FidelityEnclaveError::CohortCountsOverflow)?;
        if interior.len() < need {
            return Err(FidelityEnclaveError::CohortInteriorTruncated.into());
        }

        let active = interior[HEADER_LEN..sold_start]
            .chunks_exact(ACTIVE_ENTRY_LEN)
            .map(|chunk| {
                (
                    U256::from_be_slice(&chunk[..32]),
                    read_u64_be(&chunk[32..40]),
                )
            })
            .collect();
        let sold = interior[sold_start..need]
            .chunks_exact(SOLD_ENTRY_LEN)
            .map(|chunk| {
                (
                    U256::from_be_slice(&chunk[..32]),
                    read_u64_be(&chunk[32..40]),
                    read_u64_be(&chunk[40..48]),
                )
            })
            .collect();
        Ok(Self {
            qualified_start,
            active,
            sold,
        })
    }

    /// Encode, zero-padded to the next combined-count bucket (8, 16, 32, ...) so
    /// the ciphertext length leaks only a coarse cohort-count bucket. The
    /// combined count never decreases (a full-consume moves active -> sold), so
    /// the blob length is monotone and changes only at bucket crossings.
    fn encode_padded(&self) -> Result<Vec<u8>> {
        let active_count = u32::try_from(self.active.len())
            .map_err(|_| FidelityEnclaveError::ActiveCohortCountOverflow)?;
        let sold_count = u32::try_from(self.sold.len())
            .map_err(|_| FidelityEnclaveError::SoldCohortCountOverflow)?;
        let total = self.active.len() + self.sold.len();
        let bucket = total.max(MIN_BUCKET).next_power_of_two();
        let target = HEADER_LEN + bucket * SOLD_ENTRY_LEN;

        let mut out = Vec::with_capacity(target);
        out.extend_from_slice(&self.qualified_start.to_be_bytes());
        out.extend_from_slice(&active_count.to_be_bytes());
        out.extend_from_slice(&sold_count.to_be_bytes());
        for (size, acquired_at) in &self.active {
            out.extend_from_slice(&size.to_be_bytes::<32>());
            out.extend_from_slice(&acquired_at.to_be_bytes());
        }
        for (size, acquired_at, sold_at) in &self.sold {
            out.extend_from_slice(&size.to_be_bytes::<32>());
            out.extend_from_slice(&acquired_at.to_be_bytes());
            out.extend_from_slice(&sold_at.to_be_bytes());
        }
        out.resize(target.max(out.len()), 0);
        Ok(out)
    }

    /// ACQUISITION: push or coalesce an active cohort for `utc_day`. Returns
    /// `Some(utc_day)` when this is the account's first qualified acquisition.
    fn cohort_in(&mut self, amount: U256, utc_day: u64) -> Option<u64> {
        if amount.is_zero() {
            return None;
        }
        let initialized = (self.qualified_start == 0).then(|| {
            self.qualified_start = utc_day;
            utc_day
        });
        if let Some((last_size, _)) = self.active.last_mut().filter(|(_, day)| *day == utc_day) {
            *last_size = last_size.saturating_add(amount);
        } else {
            self.active.push((amount, utc_day));
        }
        initialized
    }

    fn consume_top_active(&mut self, remaining: &mut U256) -> Option<(U256, u64)> {
        let (size, acquired_day) = *self.active.last()?;
        if size <= *remaining {
            self.active.pop();
            *remaining -= size;
            return Some((size, acquired_day));
        }
        self.active.last_mut().expect("non-empty active").0 = size - *remaining;
        let consumed = *remaining;
        *remaining = U256::ZERO;
        Some((consumed, acquired_day))
    }

    fn record_sold_slice(&mut self, slice: U256, acquired_day: u64, utc_day: u64) {
        if acquired_day >= utc_day {
            return;
        }
        if let Some((last_size, _, _)) = self
            .sold
            .last_mut()
            .filter(|(_, acq, sold)| *acq == acquired_day && *sold == utc_day)
        {
            *last_size = last_size.saturating_add(slice);
        } else {
            self.sold.push((slice, acquired_day, utc_day));
        }
    }

    /// SALE: consume active cohorts LIFO (youngest first) at `utc_day`. Splits
    /// the boundary cohort proportionally. Same-day acquisitions (`acquired_day == utc_day`)
    /// have zero held decay duration and are annihilated directly; older slices
    /// keep their original `acquired_day` and coalesce into `sold`.
    fn cohort_out(&mut self, amount: U256, utc_day: u64) {
        let mut remaining = amount;
        while !remaining.is_zero() {
            let Some((slice, acquired_day)) = self.consume_top_active(&mut remaining) else {
                break;
            };
            self.record_sold_slice(slice, acquired_day, utc_day);
        }
        if self.active.is_empty() && self.sold.is_empty() {
            self.qualified_start = 0;
        }
    }

    /// `(rcfi, efficiency, d_dec_age)` at `eval_day` (UTC day index).
    fn rcfi_triple(&self, eval_day: u64) -> Result<(U256, U256, U256)> {
        let mut acc = RcfiAccumulator::default();
        for (size, acquired_day) in &self.active {
            acc.add_active(*size, *acquired_day, eval_day)
                .ok_or(FidelityEnclaveError::RcfiArithmeticOverflow)?;
        }
        for (size, acquired_day, sold_day) in &self.sold {
            if *sold_day <= eval_day {
                acc.add_sold(*size, *acquired_day, *sold_day, eval_day)
                    .ok_or(FidelityEnclaveError::RcfiArithmeticOverflow)?;
            } else if *acquired_day < eval_day {
                acc.add_active(*size, *acquired_day, eval_day)
                    .ok_or(FidelityEnclaveError::RcfiArithmeticOverflow)?;
            }
        }
        acc.finish(self.qualified_start, eval_day)
            .ok_or_else(|| FidelityEnclaveError::RcfiArithmeticOverflow.into())
    }

    /// `(rcfi, efficiency, league)` at `eval_day` (UTC day index).
    /// `first_qualified_start = 0` means no account has qualified (league floor).
    fn evaluate(&self, eval_day: u64, first_qualified_start: u64) -> Result<(U256, U256, u16)> {
        let (rcfi, efficiency, _) = self.rcfi_triple(eval_day)?;
        let max = if first_qualified_start != 0 {
            t_dec(eval_day.saturating_sub(first_qualified_start))
        } else {
            U256::ZERO
        };
        let league =
            league_from_rcfi(rcfi, max).ok_or(FidelityEnclaveError::LeagueArithmeticOverflow)?;
        Ok((rcfi, efficiency, league))
    }
}

fn read_state(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<(u64, CohortState)> {
    let (version, interior) = fidelity_cipher::read_blob(view_key, account, blob)?;
    let interior = Zeroizing::new(interior);
    Ok((version, CohortState::decode(&interior)?))
}

/// Apply the Fidelity section of a Gratis op:
///
/// 1. Decrypt the account's cohort blob.
/// 2. Mutate it (or only probe it) at the UTC day of `section.timestamp`.
/// 3. Re-encrypt it.
/// 4. Report the plaintext receipt.
///
/// `amount` is the Gratis op's own amount. Errors reject the whole combined op.
pub fn apply_cohort_section(
    state_key: &StateKeyHandle,
    account: Address,
    amount: U256,
    section: &FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    let view_key = FIDELITY.account_keys.derive_view_key(state_key, account)?;
    let (version, mut state) = read_state(&view_key, account, &section.current_blob)?;
    let utc_day = outbe_system_common::time::utc_day_from_timestamp(section.timestamp);

    let mut qualified_start_initialized = None;
    match section.op {
        FidelityCohortOp::In => {
            qualified_start_initialized = state.cohort_in(amount, utc_day);
        }
        FidelityCohortOp::Out => state.cohort_out(amount, utc_day),
        FidelityCohortOp::Probe => {}
    }

    // If this very op qualified the account first on the whole chain, the host
    // will set the global scalar to the section UTC day. Evaluate the league
    // against that same anchor.
    let effective_first = if section.first_qualified_start != 0 {
        section.first_qualified_start
    } else {
        qualified_start_initialized.unwrap_or(0)
    };
    let (_, _, league) = state.evaluate(utc_day, effective_first)?;

    let new_blob = match section.op {
        // Probe never rewrites the ledger. An empty blob means "nothing to write".
        FidelityCohortOp::Probe => Vec::new(),
        FidelityCohortOp::In | FidelityCohortOp::Out => {
            let padded_state = Zeroizing::new(state.encode_padded()?);
            fidelity_cipher::write_blob(
                &view_key,
                CohortTransition {
                    account,
                    previous_version: version,
                    previous_blob: &section.current_blob,
                    padded_state: &padded_state,
                },
            )?
        }
    };
    Ok(FidelityOpOutcome {
        new_blob,
        qualified_start_initialized,
        league,
    })
}

/// Apply a STANDALONE cohort op (its own round-trip). Thin wrapper over
/// [`apply_cohort_section`] that sets the canonical inputs hash. The caller
/// (dispatch) signs `attestation_tag`. Errors surface as an enclave error ->
/// host `Fatal`.
pub fn apply_cohort_op(
    state_key: &StateKeyHandle,
    req: &FidelityCohortRequest,
) -> Result<FidelityCohortResult> {
    let outcome = apply_cohort_section(state_key, req.account, req.amount, &req.section)?;
    Ok(FidelityCohortResult {
        outcome,
        inputs_canonical_hash: fidelity_cohort_canonical_hash(req),
        attestation_tag: Vec::new(),
    })
}

/// Batch-decrypt the day's owners and return one plaintext league per owner, in
/// request order. This is metadosis's once-per-WWD snapshot. A single undecryptable
/// blob fails the whole batch (state corruption must be loud and deterministic,
/// not silently skipped).
pub fn snapshot_leagues(
    state_key: &StateKeyHandle,
    req: &FidelitySnapshotRequest,
) -> Result<Vec<FidelityLeagueEntry>> {
    let eval_day = outbe_system_common::time::utc_day_from_timestamp(req.timestamp);
    let mut leagues = Vec::with_capacity(req.entries.len());
    for entry in &req.entries {
        let view_key = FIDELITY
            .account_keys
            .derive_view_key(state_key, entry.owner)?;
        let (_, state) = read_state(&view_key, entry.owner, &entry.cohort_blob)?;
        let (rcfi, _, league) = state.evaluate(eval_day, req.first_qualified_start)?;
        leagues.push(FidelityLeagueEntry {
            owner: entry.owner,
            league,
            rcfi,
        });
    }
    Ok(leagues)
}

/// Owner-authorized RCFI/league read. Verifies the signed authorization INSIDE
/// the enclave, then decrypts and evaluates.
pub fn query_index(
    state_key: &StateKeyHandle,
    resident_chain_id: B256,
    req: &FidelityQueryRequest,
) -> Result<FidelityQueryResult> {
    if req.chain_id != resident_chain_id {
        return Err(FidelityEnclaveError::QueryAuthDifferentChain.into());
    }
    if req.expiry < req.block_timestamp {
        return Err(FidelityEnclaveError::QueryAuthExpired.into());
    }
    let Ok(sig65) = <[u8; 65]>::try_from(req.owner_sig.as_slice()) else {
        return Err(FidelityEnclaveError::OwnerSignatureLength.into());
    };
    let prehash = eip191_hash(&fidelity_query_auth_message(
        resident_chain_id,
        req.account,
        req.expiry,
    ));
    match outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65) {
        Ok(signer) if signer == req.account => {}
        _ => return Err(FidelityEnclaveError::OwnerSignatureAccountMismatch.into()),
    }

    let view_key = FIDELITY
        .account_keys
        .derive_view_key(state_key, req.account)?;
    let (_, state) = read_state(&view_key, req.account, &req.cohort_blob)?;
    let query_day = outbe_system_common::time::utc_day_from_timestamp(req.query_timestamp);
    let (rcfi, efficiency, league) = state.evaluate(query_day, req.first_qualified_start)?;
    Ok(FidelityQueryResult {
        rcfi,
        efficiency,
        league,
        inputs_canonical_hash: fidelity_query_canonical_hash(req),
        attestation_tag: Vec::new(),
    })
}

#[cfg(test)]
mod tests;
