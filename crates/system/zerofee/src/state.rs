//! Local storage helpers for the zero-fee paymaster counter.
//!
//! All access goes through `ZeroFeeContract::counter` which is a
//! `Map<Address, u64>` of packed `(date_key, count)`. The day reset is
//! lazy. The helpers treat a stored day that no longer matches the
//! current UTC day as count = 0, without an explicit overwrite. The next
//! `record_use` will rewrite the slot with the new day.

use alloy_primitives::Address;
use outbe_primitives::error::Result;

use crate::schema::{pack_counter, unpack_counter, ZeroFeeContract};

impl ZeroFeeContract<'_> {
    /// Returns the raw packed `(date_key, count)` for `signer`.
    ///
    /// An unset slot reads as `(0, 0)` (zero-init storage), which the
    /// lazy-reset rule maps to "no usage today".
    pub fn read_packed(&self, signer: Address) -> Result<u64> {
        self.counter.read(&signer)
    }

    /// Effective count for `signer` on `current_day`, applying the lazy
    /// day reset. If the stored day differs from `current_day` (or the
    /// slot has never been written), this returns 0.
    pub fn effective_count(&self, signer: Address, current_day: u32) -> Result<u32> {
        let packed = self.read_packed(signer)?;
        let (stored_day, stored_count) = unpack_counter(packed);
        if stored_day == current_day {
            Ok(stored_count)
        } else {
            Ok(0)
        }
    }

    /// Increments the sponsored-tx counter for `signer` on
    /// `current_day`, applying the lazy reset if the stored day no
    /// longer matches. The add saturates at `u32::MAX`. Callers are
    /// expected to gate on `effective_count < FREE_TX_DAILY_LIMIT`
    /// before they call this.
    ///
    /// Crate-private: the only legitimate caller is
    /// [`crate::runtime::record_sponsorship_use`]. That caller pairs the write
    /// with the [`crate::precompile::IZeroFee::SponsorshipAuthorized`] log emission.
    /// [`crate::runtime::authorize_sponsorship`] gates that caller.
    /// Broadening this surface would let a future caller burn quota
    /// without observability or authorization checks.
    pub(crate) fn record_use(&mut self, signer: Address, current_day: u32) -> Result<u32> {
        let effective = self.effective_count(signer, current_day)?;
        let next = effective.saturating_add(1);
        self.counter
            .write(&signer, pack_counter(current_day, next))?;
        Ok(next)
    }
}
