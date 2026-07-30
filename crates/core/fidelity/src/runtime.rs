use crate::math::{league_from_rcfi, t_dec, RcfiAccumulator};
pub(crate) use crate::math::{MAX_LEAGUE, MIN_LEAGUE};
use crate::schema::{
    active_cohort_key, sold_cohort_key, ActiveCohort, FidelityContract, SoldCohort,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};

/// Fork-fixed PoC ceiling for one owner's complete Fidelity opening.
pub const OCOMP_POC_MAX_COHORTS_PER_OWNER: u16 = 64;

/// Bounded readiness projection consumed by OCOMP pre-admission.
///
/// Leagues are snapshotted per owner by Metadosis at prepare time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FidelityOcompProjection {
    pub profile_ready: bool,
}

impl FidelityContract<'_> {
    /// Initializes the OCOMP profile only for the fresh-devnet empty Fidelity
    /// state. The later fork handler owns the sole production call site.
    pub fn initialize_fresh_ocomp_profile(&mut self) -> Result<FidelityOcompProjection> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if self.ocomp_profile_ready.read()? {
                if self.ocomp_max_cohorts_per_owner.read()? != OCOMP_POC_MAX_COHORTS_PER_OWNER {
                    return Err(PrecompileError::Fatal(
                        "Fidelity OCOMP profile cap mismatch".into(),
                    ));
                }
                return self.ocomp_projection();
            }
            if self.first_qualified_start.read()? != 0
                || self.ocomp_max_cohorts_per_owner.read()? != 0
            {
                return Err(PrecompileError::Fatal(
                    "Fidelity OCOMP fresh profile requires empty state".into(),
                ));
            }
            self.ocomp_max_cohorts_per_owner
                .write(OCOMP_POC_MAX_COHORTS_PER_OWNER)?;
            self.ocomp_profile_ready.write(true)?;
            self.ocomp_projection()
        })
    }

    pub fn ocomp_projection(&self) -> Result<FidelityOcompProjection> {
        Ok(FidelityOcompProjection {
            profile_ready: self.ocomp_profile_ready.read()?,
        })
    }

    pub fn owner_cohort_count(&self, account: Address) -> Result<u32> {
        self.active_count
            .read(&account)?
            .checked_add(self.sold_count.read(&account)?)
            .ok_or_else(|| {
                PrecompileError::BodyReadCorruption("Fidelity owner cohort count overflow".into())
            })
    }

    /// ACQUISITION hook ("mine Gratis from nod"): records a new active cohort.
    ///
    /// `timestamp` is the block timestamp in seconds, passed by the caller. No-op on a
    /// zero amount.
    pub fn cohort_in(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.cohort_in_inner(account, amount, timestamp))
    }

    fn cohort_in_inner(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        self.ensure_owner_capacity(account, 1)?;
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
        self.observe_owner_cohorts(account)?;
        Ok(())
    }

    /// SALE hook ("mine COEN from Gratis"): destroys active cohorts by LIFO.
    ///
    /// Youngest active cohort (the stack tail) is sold first; the boundary cohort
    /// is split proportionally — the sold slice keeps the ORIGINAL `acquired_at`
    /// and the remainder stays active.
    pub fn cohort_out(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.cohort_out_inner(account, amount, timestamp))
    }

    fn cohort_out_inner(&mut self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
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
                self.ensure_owner_capacity(account, 1)?;
                self.push_sold(account, remaining, cohort.acquired_at, timestamp)?;
                self.active_cohorts.update(&ActiveCohort {
                    slot_key: key,
                    size: cohort.size - remaining,
                    acquired_at: cohort.acquired_at,
                })?;
                remaining = U256::ZERO;
            }
        }
        self.observe_owner_cohorts(account)?;
        Ok(())
    }

    fn push_active(&mut self, account: Address, amount: U256, acquired_at: u64) -> Result<()> {
        let idx = self.active_count.read(&account)?;
        self.active_cohorts.create(&ActiveCohort {
            slot_key: active_cohort_key(account, idx),
            size: amount,
            acquired_at,
        })?;
        self.active_count.write(
            &account,
            idx.checked_add(1).ok_or_else(|| {
                PrecompileError::BodyReadCorruption("Fidelity active cohort count overflow".into())
            })?,
        )?;
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
        self.sold_count.write(
            &account,
            sidx.checked_add(1).ok_or_else(|| {
                PrecompileError::BodyReadCorruption("Fidelity sold cohort count overflow".into())
            })?,
        )?;
        Ok(())
    }

    fn ensure_owner_capacity(&self, account: Address, additional: u32) -> Result<()> {
        if !self.ocomp_profile_ready.read()? {
            return Ok(());
        }
        let next = self
            .owner_cohort_count(account)?
            .checked_add(additional)
            .ok_or_else(|| {
                PrecompileError::BodyReadCorruption("Fidelity owner cohort count overflow".into())
            })?;
        let cap = u32::from(self.ocomp_max_cohorts_per_owner.read()?);
        if cap == 0 || next > cap {
            return Err(PrecompileError::Revert(
                "Fidelity OCOMP cohort cap exceeded".into(),
            ));
        }
        Ok(())
    }

    /// Post-mutation corruption guard: an owner's total cohort count must never
    /// exceed the armed profile cap. `ensure_owner_capacity` enforces this before
    /// each add; this re-checks after the write as defence in depth. No longer
    /// tracks a high-water mark — OCOMP consumes per-owner league snapshots, not
    /// cohort counts.
    fn observe_owner_cohorts(&self, account: Address) -> Result<()> {
        if !self.ocomp_profile_ready.read()? {
            return Ok(());
        }
        let count = self.owner_cohort_count(account)?;
        let cap = self.ocomp_max_cohorts_per_owner.read()?;
        let count = u16::try_from(count).map_err(|_| {
            PrecompileError::BodyReadCorruption("Fidelity bounded cohort count exceeds u16".into())
        })?;
        if cap == 0 || count > cap {
            return Err(PrecompileError::BodyReadCorruption(
                "Fidelity bounded cohort count exceeds profile cap".into(),
            ));
        }
        Ok(())
    }

    pub fn compute_fidelity_index(&self, account: Address, timestamp: u64) -> Result<U256> {
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
        let mut accumulator = RcfiAccumulator::default();

        // Active cohorts: full decayed age, counted in numerator and denominator.
        let active = self.active_count.read(&account)?;
        for i in 0..active {
            if let Some(c) = self.active_cohorts.get(active_cohort_key(account, i))? {
                accumulator
                    .add_active(c.size, c.acquired_at, timestamp)
                    .ok_or_else(fidelity_arithmetic_error)?;
            }
        }

        // Sold cohorts: decayed holding duration, denominator only.
        let sold = self.sold_count.read(&account)?;
        for i in 0..sold {
            if let Some(c) = self.sold_cohorts.get(sold_cohort_key(account, i))? {
                accumulator
                    .add_sold(c.size, c.acquired_at, c.sold_at, timestamp)
                    .ok_or_else(fidelity_arithmetic_error)?;
            }
        }

        accumulator
            .finish(qs, timestamp)
            .ok_or_else(fidelity_arithmetic_error)
    }

    pub fn get_fidelity_index(&self, account: Address) -> Result<U256> {
        let now = self.storage.timestamp()?.to::<u64>();
        self.compute_fidelity_index(account, now)
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
    /// range split into equal slots, returning the 1-based slot the account's RCFI lands in.
    /// The account's RCFI never exceeds the synthetic max, so the top slot is reached only by
    /// the global-oldest 100%-holder.
    /// Returns [`MIN_LEAGUE`] when no account has qualified yet (max is zero).
    pub fn league_at(&self, account: Address, timestamp: u64) -> Result<u16> {
        let max = self.max_rcfi_at(timestamp)?;
        let rcfi = self.compute_fidelity_index(account, timestamp)?;
        league_from_rcfi(rcfi, max).ok_or_else(fidelity_arithmetic_error)
    }

    /// League for `account` at the current block time. See [`Self::league_at`].
    pub fn league(&self, account: Address) -> Result<u16> {
        let now = self.storage.timestamp()?.to::<u64>();
        self.league_at(account, now)
    }
}

fn fidelity_arithmetic_error() -> PrecompileError {
    PrecompileError::Fatal("Fidelity arithmetic overflow".into())
}
