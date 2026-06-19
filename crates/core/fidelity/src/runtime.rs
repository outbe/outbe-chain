use crate::errors::FidelityError;
use crate::math::{t_dec, SCALE};
use crate::schema::{
    active_cohort_key, sold_cohort_key, ActiveCohort, FidelityContract, SoldCohort,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

const DEFAULT_FIDELITY_INDEX: u64 = 1;

impl FidelityContract<'_> {
    // --- Legacy mock index (consumed by lysis/credisfactory) --------------

    pub fn get_fidelity_index(&self, address: Address) -> Result<u64> {
        let val = self.fidelity_indices.read(&address)?;
        if val == 0 {
            Ok(DEFAULT_FIDELITY_INDEX)
        } else {
            Ok(val)
        }
    }

    // reject values exceeding u32::MAX — downstream lysis casts the
    // index to u32 for `league_id`; a larger value would truncate silently.
    pub fn set_fidelity_index(&mut self, address: Address, index: u64) -> Result<()> {
        if index > u32::MAX as u64 {
            return Err(FidelityError::IndexOutOfRange { address, index }.into());
        }
        self.fidelity_indices.write(&address, index)
    }

    // --- RCFI cohort engine -----------------------------------------------

    /// ACQUISITION hook ("mine Gratis from nod"): records a new active cohort.
    ///
    /// `now` is the block timestamp in seconds, passed by the caller. No-op on a
    /// zero amount. Never reverts for cohort-accounting reasons (it will later
    /// run inside the gratis mint flow); only fatal storage errors propagate.
    pub fn on_gratis_mined(&mut self, account: Address, amount: U256, now: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        // First acquisition establishes the qualified start date (proof-of-life
        // v1). A real chain timestamp is never 0, so 0 is a safe "unset" sentinel.
        if self.qualified_start.read(&account)? == 0 {
            self.qualified_start.write(&account, now)?;
        }
        let idx = self.active_count.read(&account)?;
        self.active_cohorts.create(&ActiveCohort {
            slot_key: active_cohort_key(account, idx),
            size: amount,
            acquired_at: now,
        })?;
        self.active_count.write(&account, idx + 1)?;
        Ok(())
    }

    /// SALE hook ("mine COEN from Gratis"): destroys active cohorts by LIFO.
    ///
    /// Youngest active cohort (the stack tail) is sold first; the boundary cohort
    /// is split proportionally — the sold slice keeps the ORIGINAL `acquired_at`
    /// and the remainder stays active. If the sale exceeds the recorded active
    /// cohorts the excess is clamped (silently ignored): the ledger can legitimately
    /// under-count true gratis balance, and this hook must never revert mine_coen.
    pub fn on_coen_mined(&mut self, account: Address, amount: U256, now: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let mut remaining = amount;
        let mut count = self.active_count.read(&account)?;
        while !remaining.is_zero() && count > 0 {
            let idx = count - 1;
            let key = active_cohort_key(account, idx);
            let cohort = match self.active_cohorts.get(key)? {
                Some(c) => c,
                None => break, // defensive: missing tail slot → stop (clamp)
            };
            if cohort.size <= remaining {
                // Full consume: move the whole cohort to the sold log, pop the stack.
                self.push_sold(account, cohort.size, cohort.acquired_at, now)?;
                self.active_cohorts.delete(key)?;
                count = idx;
                self.active_count.write(&account, count)?;
                remaining -= cohort.size;
            } else {
                // Partial: record the sold slice, shrink the active remainder in
                // place (same index/acquired_at → stays the youngest tail).
                self.push_sold(account, remaining, cohort.acquired_at, now)?;
                self.active_cohorts.update(&ActiveCohort {
                    slot_key: key,
                    size: cohort.size - remaining,
                    acquired_at: cohort.acquired_at,
                })?;
                remaining = U256::ZERO;
            }
        }
        Ok(())
    }

    /// Appends a sold slice to the per-owner append-only sold log.
    fn push_sold(
        &mut self,
        account: Address,
        size: U256,
        acquired_at: u64,
        sold_at: u64,
    ) -> Result<()> {
        let sidx = self.sold_count.read(&account)?;
        self.sold_cohorts.create(&SoldCohort {
            slot_key: sold_cohort_key(account, sidx),
            size,
            acquired_at,
            sold_at,
        })?;
        self.sold_count.write(&account, sidx + 1)?;
        Ok(())
    }

    /// RCFI for `account` at block time `now` (seconds), in decayed days (0..L).
    /// Floor of the fixed-point result from [`Self::compute_rcfi_fp`].
    pub fn compute_rcfi(&self, account: Address, now: u64) -> Result<u64> {
        let (rcfi_fp, _, _) = self.compute_rcfi_fp(account, now)?;
        Ok((rcfi_fp / SCALE).to::<u64>())
    }

    /// Fixed-point RCFI as `(rcfi, efficiency, d_dec_age)`, all 10^18-scaled.
    /// `rcfi = d_dec_age · efficiency`; `efficiency ∈ [0, 10^18]`. Pure given `now`.
    ///
    /// Overflow safety: each term `size · T_dec(..)` has `T_dec ≤ L_FP (~2^70)`;
    /// for any realistic supply and cohort count the sums and `num·SCALE` stay
    /// well under `2^256` (see the crate plan's overflow analysis).
    pub fn compute_rcfi_fp(&self, account: Address, now: u64) -> Result<(U256, U256, U256)> {
        let qs = self.qualified_start.read(&account)?;
        if qs == 0 {
            return Ok((U256::ZERO, U256::ZERO, U256::ZERO));
        }
        let d_dec_age = t_dec(now.saturating_sub(qs));

        let mut num = U256::ZERO;
        let mut den = U256::ZERO;

        // Active cohorts: full decayed age, counted in numerator and denominator.
        let active = self.active_count.read(&account)?;
        for i in 0..active {
            if let Some(c) = self.active_cohorts.get(active_cohort_key(account, i))? {
                let contribution = c.size * t_dec(now.saturating_sub(c.acquired_at));
                num += contribution;
                den += contribution;
            }
        }

        // Sold cohorts: decayed holding duration, denominator only.
        let sold = self.sold_count.read(&account)?;
        for i in 0..sold {
            if let Some(c) = self.sold_cohorts.get(sold_cohort_key(account, i))? {
                let buy = t_dec(now.saturating_sub(c.acquired_at));
                let sell = t_dec(now.saturating_sub(c.sold_at));
                // buy ≥ sell since acquired_at ≤ sold_at; saturating guards skew.
                den += c.size * buy.saturating_sub(sell);
            }
        }

        let efficiency = if den.is_zero() {
            U256::ZERO
        } else {
            num * SCALE / den
        };
        let rcfi = d_dec_age * efficiency / SCALE;
        Ok((rcfi, efficiency, d_dec_age))
    }

    /// RCFI for `account` at the current block time, in decayed days (0..L).
    pub fn get_rcfi(&self, account: Address) -> Result<u64> {
        let now = self.storage.timestamp()?.to::<u64>();
        self.compute_rcfi(account, now)
    }
}
