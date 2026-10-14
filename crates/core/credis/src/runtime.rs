//! Read-time derivations of a Credis: its deadline, effective state, outcome and the
//! interest accrued. CredisFactory applies the rules that change a Credis.
//!
//! A Credis lives on the COEN price path, not on a calendar. Nothing here is
//! scheduled off `issued_at`. The only time-driven quantity is the interest
//! day count. Even that is evaluated lazily at settlement, not accrued per
//! block.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::units::SCALE_1E6_U256;

use crate::constants::DAYS_PER_YEAR;
use crate::errors::CredisError;
use crate::schema::{Credis, CredisContract, CredisState};

/// Timestamp after which a called Credis's remainder may be forfeited.
///
/// Reads the notice period sealed onto the Credis at issuance, so switching the
/// profile cannot move the deadline of a Credis that is already live.
pub fn settlement_deadline(record: &Credis) -> u64 {
    record
        .called_at
        .saturating_add(u64::from(record.call_notice_period_seconds))
}

/// The state a reader sees at `now`: a Called Credis past its deadline reads Forfeited
/// before the forfeit sweep reaches it.
pub fn effective_state(record: &Credis, now: u64) -> Result<CredisState> {
    let state = record.lifecycle_state()?;
    if state == CredisState::Called && now > settlement_deadline(record) {
        return Ok(CredisState::Forfeited);
    }
    Ok(state)
}

/// Where a Credis's principal and Gratis went, as a reader sees it at `now`.
/// A Forfeited Credis keeps its remainders on the record: they are what was written
/// off and burned, and nothing is outstanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub outstanding_principal_minor: U256,
    pub principal_paid_minor: U256,
    pub principal_written_off_minor: U256,
    pub outstanding_gratis_minor: U256,
    pub gratis_returned_minor: U256,
    pub gratis_burned_minor: U256,
}

pub fn outcome(record: &Credis, now: u64) -> Result<Outcome> {
    let principal_paid_minor = record
        .principal_minor
        .checked_sub(record.outstanding_principal_minor)
        .ok_or(CredisError::ArithmeticOverflow)?;
    let gratis_returned_minor = record
        .gratis_minor
        .checked_sub(record.outstanding_gratis_minor)
        .ok_or(CredisError::ArithmeticOverflow)?;
    let forfeited = effective_state(record, now)? == CredisState::Forfeited;
    let (outstanding_principal, written_off) = match forfeited {
        true => (U256::ZERO, record.outstanding_principal_minor),
        false => (record.outstanding_principal_minor, U256::ZERO),
    };
    let (outstanding_gratis, burned) = match forfeited {
        true => (U256::ZERO, record.outstanding_gratis_minor),
        false => (record.outstanding_gratis_minor, U256::ZERO),
    };
    Ok(Outcome {
        outstanding_principal_minor: outstanding_principal,
        principal_paid_minor,
        principal_written_off_minor: written_off,
        outstanding_gratis_minor: outstanding_gratis,
        gratis_returned_minor,
        gratis_burned_minor: burned,
    })
}

impl CredisContract<'_> {
    /// Whole UTC days that a settlement at `now` charges. This is the day count
    /// for the interest and the amount by which the accrual anchor advances.
    /// The function deliberately does not consume the sub-day remainder. The
    /// remainder stays on the Credis, and a later settlement charges it.
    pub fn elapsed_days(record: &Credis, now: u64) -> u64 {
        now.saturating_sub(record.last_settled_at) / SECONDS_PER_DAY
    }

    /// Interest accrued on the outstanding principal since the accrual anchor.
    /// The interest is simple and non-compounding, ACT/365 over whole elapsed days.
    /// It is floored in the user's favor to the asset's minor unit (C34).
    /// A Forfeited Credis accrues nothing.
    pub fn accrued_interest(record: &Credis, now: u64) -> Result<U256> {
        let days = Self::elapsed_days(record, now);
        if days == 0
            || record.outstanding_principal_minor.is_zero()
            || record.policy_rate.is_zero()
            || effective_state(record, now)? == CredisState::Forfeited
        {
            return Ok(U256::ZERO);
        }
        let numerator = record
            .outstanding_principal_minor
            .checked_mul(record.policy_rate)
            .and_then(|v| v.checked_mul(U256::from(days)))
            .ok_or(CredisError::ArithmeticOverflow)?;
        // Non-zero by construction: both factors are compile-time constants.
        // The scale must match `policy_rate`, which the oracle publishes at 1e6.
        let denominator = U256::from(DAYS_PER_YEAR)
            .checked_mul(SCALE_1E6_U256)
            .ok_or(CredisError::ArithmeticOverflow)?;
        Ok(numerator / denominator)
    }

    // ---------------------------------------------------------------------
    // Reads
    // ---------------------------------------------------------------------

    /// Loads the Credis record. Reverts on missing.
    pub fn get_credis(&self, credis_id: U256) -> Result<Credis> {
        self.load_credis(credis_id)
    }

    /// Sum of `principal_minor` and of `outstanding_principal_minor` across all Credis for
    /// `account`, in one walk of the owner index.
    pub fn principal_and_outstanding_of(&self, account: Address, now: u64) -> Result<(U256, U256)> {
        let mut principal = U256::ZERO;
        let mut outstanding = U256::ZERO;
        for record in self.get_credis_by_owner(account)? {
            principal = principal
                .checked_add(record.principal_minor)
                .ok_or(CredisError::ArithmeticOverflow)?;
            outstanding = outstanding
                .checked_add(outcome(&record, now)?.outstanding_principal_minor)
                .ok_or(CredisError::ArithmeticOverflow)?;
        }
        Ok((principal, outstanding))
    }

    /// How many Credis `account` has ever been issued.
    pub fn credis_count_of(&self, account: Address) -> Result<u32> {
        self.read_owner_credis_count(account)
    }

    /// `account`'s `index`-th Credis, in insertion order.
    pub fn token_of_owner_by_index(&self, account: Address, index: u32) -> Result<U256> {
        if index >= self.read_owner_credis_count(account)? {
            return Err(CredisError::IndexOutOfBounds.into());
        }
        self.read_owner_credis_id(account, index)
    }

    /// The `index`-th Credis ever created, in creation order.
    pub fn token_by_index(&self, index: u64) -> Result<U256> {
        if index >= self.read_total_credis()? {
            return Err(CredisError::IndexOutOfBounds.into());
        }
        self.read_credis_id_at(index)
    }

    /// All Credis for `account`, in insertion order. Unbounded, so it stays
    /// internal: the ABI enumerates through `token_of_owner_by_index` instead.
    pub(crate) fn get_credis_by_owner(&self, account: Address) -> Result<Vec<Credis>> {
        let count = self.read_owner_credis_count(account)?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let credis_id = self.read_owner_credis_id(account, i)?;
            if let Some(record) = self.records.get(credis_id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Total Credis ever created, including closed Credis. This value backs `totalSupply`.
    pub fn total_credis(&self) -> Result<u64> {
        self.read_total_credis()
    }

    /// Credis id at global dense-index `index` (`index < total_credis()`).
    pub fn credis_id_at(&self, index: u64) -> Result<U256> {
        self.read_credis_id_at(index)
    }
}
