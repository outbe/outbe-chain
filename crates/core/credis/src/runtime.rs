//! Business logic for the Credis contract.
//!
//! A Credis lives on the COEN price path, not on a calendar. Nothing here is
//! scheduled off `issued_at`. The only time-driven quantity is the interest
//! day count. Even that is evaluated lazily at settlement, not accrued per
//! block.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{timestamp_to_date_key, SECONDS_PER_DAY};
use outbe_primitives::units::SCALE_1E6_U256;

use crate::constants::{CALL_RATE_PCT, DAYS_PER_YEAR, PRICE_RATE_DEN};
use crate::errors::CredisError;
use crate::precompile::ICredis;
use crate::schema::{Credis, CredisContract, CredisState};

/// Current execution timestamp's UTC reward day key (YYYYMMDD).
fn reward_day(storage: &StorageHandle<'_>) -> Result<u32> {
    // Execution timestamps must fit Unix seconds in u64. Reject a value that does not fit
    // rather than truncate it.
    let timestamp =
        u64::try_from(storage.timestamp()?).map_err(|_| CredisError::ArithmeticOverflow)?;
    Ok(timestamp_to_date_key(timestamp))
}

/// Terms captured when a Credis is issued. Grouped rather than passed positionally
/// so a mis-ordered `U256` cannot silently swap principal for collateral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCredisParams {
    pub owner: Address,
    pub cca: Address,
    /// Main account whose pledged Gratis backs the Credis.
    pub source: Address,
    pub asset: Address,
    /// ISO 4217 numeric code of the disbursed `asset`.
    pub issuance_currency: u16,
    /// ISO 4217 numeric code of the reference currency elected at issuance and
    /// fixed for the Credis's life.
    pub reference_currency: u16,
    /// `r`, 1e6 scaled, already multiplied by the policy-rate factor.
    pub policy_rate: U256,
    /// `P` - stablecoin minor units disbursed.
    pub principal_minor: U256,
    /// Entry price in the issuance currency, scale 1e6, fixed by the reservation.
    pub entry_price_minor: U256,
    /// Call anchor price in the reference currency, scale 1e6, sealed at issuance.
    pub call_anchor_price_minor: U256,
    /// `G` - pledged Gratis collateral.
    pub gratis_minor: U256,
    pub issued_at: u64,
}

/// Outcome of [`CredisContract::settle`]. The caller moves the money. It pulls
/// `total_paid` from the payer into the vault. It releases `gratis_returned_minor` to
/// the pledger, never to the payer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    /// Interest collected, in full, before any principal.
    pub interest: U256,
    /// Principal covered by this payment.
    pub principal_paid: U256,
    /// `interest + principal_paid` - what the payer owes for this settlement.
    pub total_paid: U256,
    /// Collateral freed and owed back to the pledger.
    pub gratis_returned_minor: U256,
    pub asset: Address,
    /// True when this settlement drove the outstanding principal to zero.
    pub closed: bool,
}

/// What a forfeit writes off. `gratis_burned_minor` is the unpaid share of the collateral.
/// Principal written off is never collected. Unpaid interest is not booked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forfeit {
    pub gratis_burned_minor: U256,
    pub principal_written_off: U256,
}

/// `price x (100 + rate_pct) / 100`.
pub fn calc_call_price(price: U256) -> Result<U256> {
    price
        .checked_mul(U256::from(PRICE_RATE_DEN + CALL_RATE_PCT))
        .map(|v| v / U256::from(PRICE_RATE_DEN))
        .ok_or_else(|| CredisError::ArithmeticOverflow.into())
}

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
    fn elapsed_days(record: &Credis, now: u64) -> u64 {
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

    /// Issues a Credis and returns its derived
    /// `credis_id = keccak256(cca || owner || asset || block_number)`.
    ///
    /// This function seals everything the Credis will ever need:
    ///
    /// - The call price derives from `call_anchor_price_minor`.
    /// - `policy_rate` is pinned.
    /// - The call terms are snapshotted from the profile, so a later profile change
    ///   cannot re-term a live Credis.
    ///
    /// Collateral starts fully locked and the interest anchor starts at issuance.
    /// Entry price, call anchor, and call price are not written again.
    pub fn issue(&mut self, params: IssueCredisParams) -> Result<U256> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let terms = [
                params.principal_minor,
                params.gratis_minor,
                params.entry_price_minor,
                params.call_anchor_price_minor,
            ];
            if terms.iter().any(U256::is_zero) {
                return Err(CredisError::InvalidAmount.into());
            }
            if params.source.is_zero() {
                return Err(CredisError::InvalidSource.into());
            }

            let credis_id = CredisContract::credis_id(
                params.cca,
                params.owner,
                params.asset,
                storage.block_number()?,
            );
            // One Credis per CCA/account/asset tuple per execution block.
            if self.credis_exists(credis_id)? {
                return Err(CredisError::CredisAlreadyExists.into());
            }
            let call_terms = crate::config::read_from(self, storage.chain_id()?)?;

            let record = Credis {
                credis_id,
                owner: params.owner,
                cca: params.cca,
                asset: params.asset,
                issuance_currency: params.issuance_currency,
                reference_currency: params.reference_currency,
                source: params.source,
                principal_minor: params.principal_minor,
                outstanding_principal_minor: params.principal_minor,
                gratis_minor: params.gratis_minor,
                outstanding_gratis_minor: params.gratis_minor,
                policy_rate: params.policy_rate,
                entry_price_minor: params.entry_price_minor,
                call_price_minor: calc_call_price(params.call_anchor_price_minor)?,
                issued_at: params.issued_at,
                last_settled_at: params.issued_at,
                called_at: 0,
                state: CredisState::Issued as u8,
                call_notice_period_seconds: call_terms.call_notice_period_seconds,
                call_rate: CALL_RATE_PCT,
                call_window_seconds: call_terms.call_window_seconds,
                call_threshold_seconds: call_terms.call_threshold_seconds,
                call_anchor_price_minor: params.call_anchor_price_minor,
                interest_paid_minor: U256::ZERO,
            };
            outbe_ccaregistry::api::credis_issued(
                &self.storage,
                params.cca,
                reward_day(&self.storage)?,
                params.gratis_minor,
            )?;
            self.create_credis_record(&record)?;
            self.widen_scan_terms(&record)?;
            self.append_to_owner_index(params.owner, credis_id)?;
            self.append_to_global_index(credis_id)?;
            self.index_for_call(&record)?;

            self.emit(ICredis::Transfer {
                from: Address::ZERO,
                to: params.owner,
                tokenId: credis_id,
            })?;
            Ok(credis_id)
        })
    }

    /// Calls an Issued Credis, opening the settlement window. Settlement terms
    /// are unchanged throughout it. Idempotent: an already-called Credis
    /// returns `false` without moving its deadline.
    ///
    /// The caller must establish the sustained breach before this call.
    pub fn mark_called(&mut self, credis_id: U256, now: u64) -> Result<bool> {
        let mut record = self.load_credis(credis_id)?;
        if record.lifecycle_state()? != CredisState::Issued {
            return Ok(false);
        }
        record.state = CredisState::Called as u8;
        record.called_at = now;
        self.update_credis_record(&record)?;
        self.unindex_for_call(&record)?;
        self.queue_called(credis_id, settlement_deadline(&record))?;
        self.emit(ICredis::CredisCalled {
            credisId: credis_id,
            calledAt: now,
            settlementDeadline: settlement_deadline(&record),
        })?;
        self.emit(ICredis::MetadataUpdate {
            _tokenId: credis_id,
        })?;
        Ok(true)
    }

    /// Applies a settlement of `amount`: interest first, principal second.
    ///
    /// Any payer may settle any Issued or Called Credis. The released collateral
    /// is owed to the pledger recorded on the Credis. Thus a payer can never
    /// redirect value to themselves.
    ///
    /// - Called Credis accept repayment through deadline equality.
    /// - The function rejects a payment below the accrued interest outright.
    /// - The function consumes only what the Credis needs, so it does not
    ///   over-pull an over-payment. The caller charges `Settlement::total_paid`.
    /// - Collateral release is principal-proportional, rounded up and capped by
    ///   the locked remainder. Final settlement releases exactly what is left.
    pub fn settle(&mut self, credis_id: U256, amount: U256, now: u64) -> Result<Settlement> {
        let mut record = self.load_credis(credis_id)?;
        let state_before = record.lifecycle_state()?;
        match state_before {
            CredisState::Settled | CredisState::Forfeited => {
                return Err(CredisError::CredisClosed.into())
            }
            CredisState::Issued | CredisState::Called => {}
        }

        if state_before == CredisState::Called && now > settlement_deadline(&record) {
            return Err(CredisError::SettlementDeadlinePassed.into());
        }

        let days = Self::elapsed_days(&record, now);
        let interest = Self::accrued_interest(&record, now)?;
        if amount < interest {
            return Err(CredisError::PaymentBelowAccruedInterest.into());
        }
        let principal_paid = (amount - interest).min(record.outstanding_principal_minor);

        // C34 favors the user on each partial. Repeated ceilings can exhaust
        // collateral before principal, so cap every return at the remainder.
        let gratis_returned_minor = if principal_paid == record.outstanding_principal_minor {
            record.outstanding_gratis_minor
        } else {
            record
                .gratis_minor
                .checked_mul(principal_paid)
                .ok_or(CredisError::ArithmeticOverflow)?
                .div_ceil(record.principal_minor)
                .min(record.outstanding_gratis_minor)
        };

        record.outstanding_principal_minor = record
            .outstanding_principal_minor
            .checked_sub(principal_paid)
            .ok_or(CredisError::ArithmeticOverflow)?;
        record.outstanding_gratis_minor = record
            .outstanding_gratis_minor
            .checked_sub(gratis_returned_minor)
            .ok_or(CredisError::ArithmeticOverflow)?;
        // Accrual restarts on the reduced principal. No unpaid interest ever
        // carries between settlements. The anchor advances by the whole days
        // actually charged, never to `now`. Settling on a sub-day boundary must
        // not discard the remainder. Otherwise, repeated dust settlements just
        // under 24h apart would hold `days` at zero and evade the coupon entirely.
        record.last_settled_at = record
            .last_settled_at
            .saturating_add(days.saturating_mul(SECONDS_PER_DAY));
        // Sum of successful interest deltas. Every revert path returns above this write.
        record.interest_paid_minor = record
            .interest_paid_minor
            .checked_add(interest)
            .ok_or(CredisError::ArithmeticOverflow)?;

        let closed = record.outstanding_principal_minor.is_zero();
        if closed {
            record.state = CredisState::Settled as u8;
        }
        self.update_credis_record(&record)?;
        if closed {
            // Terminal: leave the call index and the settlement-deadline queue.
            self.unindex_for_call(&record)?;
            if state_before == CredisState::Called {
                self.unqueue_called(credis_id)?;
            }
        }

        self.emit(ICredis::SettlementApplied {
            credisId: credis_id,
            interestPaidMinor: interest,
            principalPaidMinor: principal_paid,
            gratisReturnedMinor: gratis_returned_minor,
            outstandingPrincipalMinor: record.outstanding_principal_minor,
        })?;
        if closed {
            self.emit(ICredis::CredisSettled {
                credisId: credis_id,
            })?;
        }
        self.emit(ICredis::MetadataUpdate {
            _tokenId: credis_id,
        })?;

        Ok(Settlement {
            interest,
            principal_paid,
            total_paid: interest
                .checked_add(principal_paid)
                .ok_or(CredisError::ArithmeticOverflow)?,
            gratis_returned_minor,
            asset: record.asset,
            closed,
        })
    }

    /// Forfeits the remainder of a called Credis whose settlement window has
    /// lapsed. Only the unpaid share is written off: every settlement already
    /// released its proportional share, so whatever the owner settled they have
    /// already reclaimed. The record keeps its remainders: they are what is
    /// written off and burned, read through [`outcome`]. Rounded-up partial returns
    /// may leave zero collateral even while principal remains outstanding.
    ///
    /// Returns what the caller must burn and credit. This function closes the
    /// Credis itself.
    pub fn forfeit(&mut self, credis_id: U256, now: u64) -> Result<Forfeit> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let mut record = self.load_credis(credis_id)?;
            if record.lifecycle_state()? != CredisState::Called {
                return Err(CredisError::NotCalled.into());
            }
            if now <= settlement_deadline(&record) {
                return Err(CredisError::SettlementDeadlineNotPassed.into());
            }
            if record.outstanding_principal_minor.is_zero() {
                return Err(CredisError::NothingOutstanding.into());
            }

            let gratis_burned_minor = record.outstanding_gratis_minor;
            let principal_written_off = record.outstanding_principal_minor;

            outbe_ccaregistry::api::credis_forfeited(
                &self.storage,
                record.cca,
                reward_day(&self.storage)?,
                gratis_burned_minor,
            )?;
            // The remainders stay on the record: they are what was written off and burned.
            record.state = CredisState::Forfeited as u8;
            self.update_credis_record(&record)?;
            self.unindex_for_call(&record)?;
            self.unqueue_called(credis_id)?;

            self.emit(ICredis::CredisForfeited {
                credisId: credis_id,
                cca: record.cca,
                gratisBurnedMinor: gratis_burned_minor,
                principalWrittenOffMinor: principal_written_off,
            })?;
            self.emit(ICredis::MetadataUpdate {
                _tokenId: credis_id,
            })?;

            Ok(Forfeit {
                gratis_burned_minor,
                principal_written_off,
            })
        })
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
