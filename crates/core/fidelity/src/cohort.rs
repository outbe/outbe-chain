//! Plaintext Fidelity cohort ledger.
//!
//! `active` is a LIFO stack of acquisitions `(size, acquired_at)`. `sold` is an
//! append-only log `(size, acquired_at, sold_at)`. The ciphertext length leaks
//! only the combined-count padding bucket.

use alloy_primitives::U256;
use outbe_fidelity_math::{league_from_rcfi, t_dec, RcfiAccumulator};

use crate::errors::CohortError;

/// Interior header: `qualified_start(8) || active_count(4) || sold_count(4)`.
pub const HEADER_LEN: usize = 16;
/// `size(32) || acquired_at(8)`.
const ACTIVE_ENTRY_LEN: usize = 40;
/// `size(32) || acquired_at(8) || sold_at(8)`.
const SOLD_ENTRY_LEN: usize = 48;
/// Smallest padding bucket, in combined cohort count.
const MIN_BUCKET: usize = 8;

/// Plaintext cohort ledger of one account.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CohortState {
    /// UTC day of the account's first qualified acquisition. Zero if none.
    pub qualified_start: u64,
    /// Unsold acquisitions, oldest first. The last entry is consumed first.
    pub active: Vec<(U256, u64)>,
    /// Sold slices. Same-day acquisitions are not recorded here.
    pub sold: Vec<(U256, u64, u64)>,
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
    /// Decode a blob interior. An empty interior is a fresh account.
    /// Trailing bytes beyond the declared entries are padding and ignored.
    pub fn decode(interior: &[u8]) -> Result<Self, CohortError> {
        if interior.is_empty() {
            return Ok(Self::default());
        }
        if interior.len() < HEADER_LEN {
            return Err(CohortError::CohortInteriorShorterThanHeader);
        }
        let qualified_start = read_u64_be(&interior[..8]);
        let active_count = read_u32_be(&interior[8..12]);
        let sold_count = read_u32_be(&interior[12..16]);
        let active_bytes = active_count
            .checked_mul(ACTIVE_ENTRY_LEN)
            .ok_or(CohortError::CohortCountsOverflow)?;
        let sold_bytes = sold_count
            .checked_mul(SOLD_ENTRY_LEN)
            .ok_or(CohortError::CohortCountsOverflow)?;
        let sold_start = HEADER_LEN
            .checked_add(active_bytes)
            .ok_or(CohortError::CohortCountsOverflow)?;
        let need = sold_start
            .checked_add(sold_bytes)
            .ok_or(CohortError::CohortCountsOverflow)?;
        if interior.len() < need {
            return Err(CohortError::CohortInteriorTruncated);
        }
        let active = interior[HEADER_LEN..sold_start]
            .as_chunks::<ACTIVE_ENTRY_LEN>()
            .0
            .iter()
            .map(|chunk| {
                (
                    U256::from_be_slice(&chunk[..32]),
                    read_u64_be(&chunk[32..40]),
                )
            })
            .collect();
        let sold = interior[sold_start..need]
            .as_chunks::<SOLD_ENTRY_LEN>()
            .0
            .iter()
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

    /// Encode, zero-padded to the next combined-count bucket (8, 16, 32, ...).
    pub fn encode_padded(&self) -> Result<Vec<u8>, CohortError> {
        let active_count =
            u32::try_from(self.active.len()).map_err(|_| CohortError::ActiveCohortCountOverflow)?;
        let sold_count =
            u32::try_from(self.sold.len()).map_err(|_| CohortError::SoldCohortCountOverflow)?;
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

    /// Push or coalesce an active cohort for `utc_day`.
    ///
    /// Returns `Some(utc_day)` when this is the account's first qualified acquisition.
    pub fn cohort_in(&mut self, amount: U256, utc_day: u64) -> Option<u64> {
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

    /// Consume active cohorts LIFO at `utc_day`.
    ///
    /// Same-day slices are annihilated. Older slices keep their original
    /// acquisition day and coalesce into `sold`. An empty ledger clears
    /// `qualified_start`.
    pub fn cohort_out(&mut self, amount: U256, utc_day: u64) {
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

    fn consume_top_active(&mut self, remaining: &mut U256) -> Option<(U256, u64)> {
        let (size, acquired_day) = *self.active.last()?;
        if size <= *remaining {
            self.active.pop();
            *remaining -= size;
            return Some((size, acquired_day));
        }
        let consumed = *remaining;
        *remaining = U256::ZERO;
        if let Some(top) = self.active.last_mut() {
            top.0 = size - consumed;
        }
        Some((consumed, acquired_day))
    }

    fn record_sold_slice(&mut self, slice: U256, acquired_day: u64, utc_day: u64) {
        if acquired_day >= utc_day {
            return;
        }
        if let Some((last_size, _, _)) = self
            .sold
            .last_mut()
            .filter(|(_, acquired, sold)| *acquired == acquired_day && *sold == utc_day)
        {
            *last_size = last_size.saturating_add(slice);
        } else {
            self.sold.push((slice, acquired_day, utc_day));
        }
    }

    /// `(rcfi, efficiency, d_dec_age)` at `eval_day`.
    pub fn rcfi_triple(&self, eval_day: u64) -> Result<(U256, U256, U256), CohortError> {
        let mut acc = RcfiAccumulator::default();
        for (size, acquired_day) in &self.active {
            acc.add_active(*size, *acquired_day, eval_day)
                .ok_or(CohortError::RcfiArithmeticOverflow)?;
        }
        for (size, acquired_day, sold_day) in &self.sold {
            if *sold_day <= eval_day {
                acc.add_sold(*size, *acquired_day, *sold_day, eval_day)
                    .ok_or(CohortError::RcfiArithmeticOverflow)?;
            } else if *acquired_day < eval_day {
                acc.add_active(*size, *acquired_day, eval_day)
                    .ok_or(CohortError::RcfiArithmeticOverflow)?;
            }
        }
        acc.finish(self.qualified_start, eval_day)
            .ok_or(CohortError::RcfiArithmeticOverflow)
    }

    /// `(rcfi, efficiency, league)` at `eval_day`.
    ///
    /// `first_qualified_start = 0` means no account has qualified, so the league
    /// is the floor.
    pub fn evaluate(
        &self,
        eval_day: u64,
        first_qualified_start: u64,
    ) -> Result<(U256, U256, u16), CohortError> {
        let (rcfi, efficiency, _) = self.rcfi_triple(eval_day)?;
        let max = if first_qualified_start != 0 {
            t_dec(eval_day.saturating_sub(first_qualified_start))
        } else {
            U256::ZERO
        };
        let league = league_from_rcfi(rcfi, max).ok_or(CohortError::LeagueArithmeticOverflow)?;
        Ok((rcfi, efficiency, league))
    }
}
