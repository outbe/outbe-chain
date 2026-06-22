use crate::math::{t_dec, SCALE};
use crate::schema::{
    active_cohort_key, sold_cohort_key, ActiveCohort, FidelityContract, SoldCohort,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;

/// Number of league tiers the `[0, synthetic_max]` RCFI range is split into.
pub(crate) const LEAGUE_COUNT: u16 = 4096;
/// Lowest league id (1-based). Assigned to the bottom slot and to accounts with
/// no retention / before any account has qualified.
pub(crate) const MIN_LEAGUE: u16 = 1;
/// Highest league id, assigned to the top slot. `4096` leagues span `1..=4096`.
pub(crate) const MAX_LEAGUE: u16 = MIN_LEAGUE + LEAGUE_COUNT - 1;

impl FidelityContract<'_> {
    /// ACQUISITION hook ("mine Gratis from nod"): records a new active cohort.
    ///
    /// `timestamp` is the block timestamp in seconds, passed by the caller. No-op on a
    /// zero amount.
    pub fn cohort_in(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        // First acquisition establishes the qualified start date (proof-of-life
        // v1). A real chain timestamp is never 0, so 0 is a safe "unset" sentinel.
        if self.qualified_start.read(&account)? == 0 {
            self.qualified_start.write(&account, timestamp)?;
        }
        // First-ever acquisition across all accounts anchors the global synthetic
        // RCFI ceiling for leagues. Block timestamps are monotonic, so the first
        // write is the chain-wide minimum qualified_start.
        if self.first_qualified_start.read()? == 0 {
            self.first_qualified_start.write(timestamp)?;
        }
        self.push_active(account, amount, timestamp)?;
        Ok(())
    }

    /// SALE hook ("mine COEN from Gratis"): destroys active cohorts by LIFO.
    ///
    /// Youngest active cohort (the stack tail) is sold first; the boundary cohort
    /// is split proportionally — the sold slice keeps the ORIGINAL `acquired_at`
    /// and the remainder stays active.
    pub fn cohort_out(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
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
                self.push_sold(account, cohort.size, cohort.acquired_at, timestamp)?;
                self.active_cohorts.delete(key)?;
                count = idx;
                self.active_count.write(&account, count)?;
                remaining -= cohort.size;
            } else {
                // Partial: record the sold slice, shrink the active remainder in
                // place (same index/acquired_at → stays the youngest tail).
                self.push_sold(account, remaining, cohort.acquired_at, timestamp)?;
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

    fn push_active(&mut self, account: Address, amount: U256, acquired_at: u64) -> Result<()> {
        let idx = self.active_count.read(&account)?;
        self.active_cohorts.create(&ActiveCohort {
            slot_key: active_cohort_key(account, idx),
            size: amount,
            acquired_at,
        })?;
        self.active_count.write(&account, idx + 1)?;
        Ok(())
    }

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

    /// RCFI for `account` at block time `timestamp` (seconds) as the fixed-point
    /// value the precompile exposes: decayed days scaled by `10^DECIMALS`
    /// (`SCALE`), so `SCALE` is one decayed day — the full `rcfi` from
    /// [`Self::compute_rcfi_fp`] without flooring.
    pub fn compute_rcfi_scaled(&self, account: Address, timestamp: u64) -> Result<U256> {
        let (rcfi_fp, _, _) = self.compute_rcfi_fp(account, timestamp)?;
        Ok(rcfi_fp)
    }

    /// Fixed-point RCFI as `(rcfi, efficiency, d_dec_age)`, all 10^18-scaled.
    /// `rcfi = d_dec_age · efficiency`; `efficiency ∈ [0, 10^18]`. Pure given `now`.
    ///
    /// Overflow safety: each term `size · T_dec(..)` has `T_dec ≤ L_FP (~2^70)`;
    /// for any realistic supply and cohort count the sums and `num·SCALE` stay
    /// well under `2^256` (see the crate plan's overflow analysis).
    pub fn compute_rcfi_fp(&self, account: Address, timestamp: u64) -> Result<(U256, U256, U256)> {
        let qs = self.qualified_start.read(&account)?;
        if qs == 0 {
            return Ok((U256::ZERO, U256::ZERO, U256::ZERO));
        }
        let d_dec_age = t_dec(timestamp.saturating_sub(qs));

        let mut num = U256::ZERO;
        let mut den = U256::ZERO;

        // Active cohorts: full decayed age, counted in numerator and denominator.
        let active = self.active_count.read(&account)?;
        for i in 0..active {
            if let Some(c) = self.active_cohorts.get(active_cohort_key(account, i))? {
                let contribution = c.size * t_dec(timestamp.saturating_sub(c.acquired_at));
                num += contribution;
                den += contribution;
            }
        }

        // Sold cohorts: decayed holding duration, denominator only.
        let sold = self.sold_count.read(&account)?;
        for i in 0..sold {
            if let Some(c) = self.sold_cohorts.get(sold_cohort_key(account, i))? {
                let buy = t_dec(timestamp.saturating_sub(c.acquired_at));
                let sell = t_dec(timestamp.saturating_sub(c.sold_at));
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

    /// RCFI for `account` at the current block time as the fixed-point value the
    /// precompile exposes (`10^DECIMALS`-scaled decayed days).
    pub fn get_rcfi_scaled(&self, account: Address) -> Result<U256> {
        let now = self.storage.timestamp()?.to::<u64>();
        self.compute_rcfi_scaled(account, now)
    }

    /// Synthetic maximum (saturating) RCFI at `timestamp`: the decayed age of
    /// the earliest-qualified account on the chain,
    /// `t_dec(timestamp − first_qualified_start)`, as a `10^DECIMALS`-scaled
    /// value. Since no account can have an earlier `qualified_start` and `t_dec`
    /// is monotonic, this is an upper bound on every account's RCFI at that time
    /// (and itself saturates toward `L`). Zero before any account has qualified.
    pub fn max_rcfi_at(&self, timestamp: u64) -> Result<U256> {
        let first = self.first_qualified_start.read()?;
        if first == 0 {
            return Ok(U256::ZERO);
        }
        Ok(t_dec(timestamp.saturating_sub(first)))
    }

    /// League for `account` at `timestamp`: the `[0, max_rcfi_at(timestamp)]`
    /// range split into [`LEAGUE_COUNT`] equal slots, returning the 1-based slot
    /// (`[MIN_LEAGUE, MAX_LEAGUE]`) the account's RCFI lands in. The account's
    /// RCFI never exceeds the synthetic max, so the top slot is reached only by
    /// the global-oldest 100%-holder; the clamp guards equality/skew. Returns
    /// [`MIN_LEAGUE`] when no account has qualified yet (max is zero).
    pub fn league_at(&self, account: Address, timestamp: u64) -> Result<u16> {
        let max = self.max_rcfi_at(timestamp)?;
        if max.is_zero() {
            return Ok(MIN_LEAGUE);
        }
        let rcfi = self.compute_rcfi_scaled(account, timestamp)?;
        // slot = floor(rcfi / (max / LEAGUE_COUNT)) = floor(rcfi · LEAGUE_COUNT / max).
        // The 10^18 scale cancels, so this is an exact floor. `rcfi ≤ max` keeps
        // the result in 0..=LEAGUE_COUNT; clamp the rcfi == max boundary to the
        // last slot (LEAGUE_COUNT - 1).
        let slot = rcfi * U256::from(LEAGUE_COUNT) / max;
        let slot = slot.min(U256::from(LEAGUE_COUNT - 1)).to::<u16>();
        Ok(MIN_LEAGUE + slot)
    }

    /// League for `account` at the current block time. See [`Self::league_at`].
    pub fn league(&self, account: Address) -> Result<u16> {
        let now = self.storage.timestamp()?.to::<u64>();
        self.league_at(account, now)
    }
}
