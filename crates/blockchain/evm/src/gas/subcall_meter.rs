//! `SubcallGasMeter` - a thin wrapper over [`evm2::interpreter::Gas`] used by
//! the outbe sub-call driver.
//!
//! Each method delegates 1:1 to the inner [`evm2::interpreter::Gas`] instance.
//! Thus byte-for-byte parity with upstream is guaranteed by construction.

use evm2::interpreter::Gas;

/// Sub-call gas meter mirroring [`evm2::interpreter::Gas`] byte-for-byte.
///
/// The driver owns one meter for each sub-call frame. All accounting semantics (regular
/// gas, reservoir, refunds, state gas spent) match upstream `evm2::interpreter::Gas`. Thus
/// the settlement triple produces identical results across proposer and validator
/// re-execution paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubcallGasMeter {
    inner: Gas,
}

impl SubcallGasMeter {
    /// Creates a new meter with the given regular-gas limit.
    ///
    /// Reservoir is zero. Outbe does not use EIP-8037 at this stage, so the
    /// meter behaves as a single-pool tracker. The meter keeps the field to
    /// keep the byte-for-byte mirror with upstream `Gas`.
    #[inline]
    pub const fn new(limit: u64) -> Self {
        Self {
            inner: Gas::new(limit),
        }
    }

    /// Returns the gas limit.
    #[inline]
    pub const fn limit(&self) -> u64 {
        self.inner.limit()
    }

    /// Returns the gas remaining (regular budget, exclusive of reservoir).
    #[inline]
    pub const fn remaining(&self) -> u64 {
        self.inner.remaining()
    }

    /// Returns the total amount of gas refunded.
    #[inline]
    pub const fn refunded(&self) -> i64 {
        self.inner.refunded()
    }

    /// Returns total state gas spent so far (EIP-8037).
    #[inline]
    pub const fn state_gas_spent(&self) -> i64 {
        self.inner.state_gas_spent()
    }

    /// Returns the state gas reservoir (EIP-8037).
    #[inline]
    pub const fn reservoir(&self) -> u64 {
        self.inner.reservoir()
    }

    /// Deducts `cost` from `remaining` only (used for child frame gas
    /// forwarding). Does not touch reservoir or regular gas budget.
    ///
    /// Returns `false` if total remaining gas is insufficient, leaving
    /// `remaining` unchanged on failure.
    #[inline]
    #[must_use = "On insufficient gas the caller must halt with an out-of-gas error"]
    pub fn record_regular_cost(&mut self, cost: u64) -> bool {
        self.inner.tracker_mut().try_spend(cost).is_ok()
    }

    /// Erases `returned` gas from the spent counter. That is, it returns unused
    /// gas from a child frame back into the meter's `remaining` budget.
    #[inline]
    pub fn erase_cost(&mut self, returned: u64) {
        self.inner.erase_cost(returned);
    }

    /// Records a refund. `refund` may be negative. The cumulative refund
    /// counter is expected to be non-negative at the end of execution.
    #[inline]
    pub fn record_refund(&mut self, refund: i64) {
        self.inner.record_refund(refund);
    }

    /// Sets the total state gas spent (used when propagating from a child
    /// frame's settlement).
    #[inline]
    pub fn set_state_gas_spent(&mut self, val: i64) {
        while self.inner.state_gas_spent() != val {
            let delta = val.saturating_sub(self.inner.state_gas_spent());
            self.inner.add_state_gas_spent(delta);
        }
    }

    /// Sets the state gas reservoir (used when propagating from a child
    /// frame's settlement).
    #[inline]
    pub fn set_reservoir(&mut self, val: u64) {
        self.inner.set_reservoir(val);
    }

    /// Returns the underlying evm2 [`Gas`] instance.
    #[inline]
    pub const fn inner(&self) -> &Gas {
        &self.inner
    }

    /// Returns the underlying evm2 [`Gas`] instance mutably. See [`Self::inner`].
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut Gas {
        &mut self.inner
    }
}
