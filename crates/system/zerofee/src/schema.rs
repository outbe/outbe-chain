//! On-chain storage layout for the zero-fee paymaster precompile.
//!
//! Tracks per-signer daily quota usage for EIP-7702 sponsored
//! transactions. A single `Map<Address, u64>` packs `(date_key, count)`:
//!
//! ```text
//! packed: [ date_key u32 (high 32 bits) | count u32 (low 32 bits) ]
//! ```
//!
//! Slot 0 carries the schema version (currently `1`).
//! `outbe-genesis-cli` writes it at genesis. The
//! macro-allocated `counter` Map uses slot 0 as its keccak base. But Map
//! entries are stored at `keccak256(addr || base_slot)`, which never
//! collides with slot 0 itself. A future layout migration would bump
//! the version marker and base its transformation on it. Today the
//! only consumer is the README rule "All precompiles ... slot 0 =
//! version" plus future migration logic.

use alloy_primitives::{Address, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::ZEROFEE_ADDRESS;
use outbe_primitives::error::Result;

use crate::runtime::SponsorshipAuthorization;

/// EVM storage layout for the zero-fee paymaster contract.
///
/// `counter` is the single per-signer record. The packed encoding keeps
/// reads and writes to one storage slot per signer regardless of how
/// many txs they have ever sponsored. Lazy day reset: when
/// `unpack(stored).date_key != current_day`, the effective count is 0.
#[storage_schema]
#[contract(addr = ZEROFEE_ADDRESS)]
pub struct ZeroFeeContract {
    /// Slot N (macro-allocated): per-signer packed `(date_key u32, count u32)`.
    ///
    /// One slot per signer is the minimal representation of the per-signer
    /// daily quota. The lazy day reset overwrites the slot in place and
    /// never deletes it. A signer that uses sponsorship on one day keeps
    /// its slot after that day. Retention is permanent by design. The cost
    /// to create each signer address limits the growth: a funding transfer,
    /// or an EIP-7702 authorization in a paid transaction.
    #[attribute(order = 0)]
    counter: outbe_primitives::storage::dsl::Map<Address, u64>,
}

impl ZeroFeeContract<'_> {
    /// Returns the raw packed `(date_key, count)` for `signer`.
    ///
    /// An unset slot reads as `(0, 0)`, which the lazy-reset rule maps to
    /// "no usage today".
    pub fn read_packed(&self, signer: Address) -> Result<u64> {
        self.counter.read(&signer)
    }

    /// Effective count for `signer` on `current_day`, applying the lazy
    /// day reset. A stored day that differs from `current_day` returns 0.
    pub fn effective_count(&self, signer: Address, current_day: u32) -> Result<u32> {
        let packed = self.read_packed(signer)?;
        let (stored_day, stored_count) = unpack_counter(packed);
        if stored_day == current_day {
            Ok(stored_count)
        } else {
            Ok(0)
        }
    }

    /// Storage slot id for `signer`'s packed counter. This is not a writable slot.
    pub fn counter_slot(&self, signer: Address) -> U256 {
        self.counter.slot(&signer).slot()
    }

    /// Increments the sponsored-tx counter for the grant's signer and day.
    /// The add saturates at `u32::MAX`. Callers gate on
    /// `effective_count < FREE_TX_DAILY_LIMIT` before they call this.
    ///
    /// Crate-private. The only production caller is
    /// [`crate::runtime::record_sponsorship_use`], which also emits
    /// [`crate::precompile::IZeroFee::SponsorshipAuthorized`].
    pub(crate) fn record_use(&mut self, auth: &SponsorshipAuthorization) -> Result<u32> {
        let signer = auth.signer();
        let current_day = auth.current_day();
        let effective = self.effective_count(signer, current_day)?;
        let next = effective.saturating_add(1);
        self.counter
            .write(&signer, pack_counter(current_day, next))?;
        Ok(next)
    }
}

/// Packs `(date_key, count)` into a single `u64` for storage.
#[inline]
pub const fn pack_counter(date_key: u32, count: u32) -> u64 {
    ((date_key as u64) << 32) | (count as u64)
}

/// Unpacks `(date_key, count)` from the stored `u64`.
#[inline]
pub const fn unpack_counter(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrip() {
        for (d, c) in [(0, 0), (1, 1), (20251231, 7), (u32::MAX, u32::MAX)] {
            let packed = pack_counter(d, c);
            assert_eq!(unpack_counter(packed), (d, c));
        }
    }

    #[test]
    fn zero_packed_means_zero_day_zero_count() {
        assert_eq!(unpack_counter(0), (0, 0));
    }
}
