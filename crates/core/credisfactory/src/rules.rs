//! The rules that change a Credis: issue on a reservation, settle interest first, delete
//! a forfeited one. Its group is called and forfeited by the sweeps. Credis keeps the
//! record and its reads.

use alloy_primitives::{Address, U256};
use outbe_credis::constants::CALL_RATE_PCT;
use outbe_credis::errors::CredisError;
use outbe_credis::runtime::{effective_state, settlement_deadline};
use outbe_credis::{Credis, CredisContract, CredisState};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{timestamp_to_date_key, SECONDS_PER_DAY};

use crate::called::emit;
use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;

/// Current execution timestamp's UTC reward day key (YYYYMMDD).
fn reward_day(storage: &StorageHandle<'_>) -> Result<u32> {
    Ok(timestamp_to_date_key(block_time(storage)?))
}

/// Execution timestamps must fit Unix seconds in u64. Reject a value that does not fit
/// rather than truncate it.
fn block_time(storage: &StorageHandle<'_>) -> Result<u64> {
    u64::try_from(storage.timestamp()?).map_err(|_| CredisError::ArithmeticOverflow.into())
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
    /// ISO 4217 numeric code of the reference currency elected at the reservation and
    /// fixed for the Credis's life.
    pub reference_currency: u16,
    /// `r`, the issuance currency's policy rate, 1e6 scaled.
    pub policy_rate: U256,
    /// `P` - stablecoin minor units disbursed.
    pub principal_minor: U256,
    /// Entry price in the issuance currency, scale 1e6, fixed by the reservation.
    pub entry_price_minor: U256,
    /// Call anchor price in the reference currency, scale 1e6, fixed by the reservation.
    pub call_anchor_price_minor: U256,
    /// `G` - pledged Gratis collateral.
    pub gratis_minor: U256,
    pub issued_at: u64,
}

/// Outcome of [`CredisContract::settle`]. The caller moves the money. It pulls
/// `total_paid` from the payer into the vault. It releases `gratis_returned_minor` to
/// the pledger, never to the payer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repayment {
    /// Interest collected, in full, before any principal.
    pub interest: U256,
    /// Principal covered by this payment.
    pub principal_paid: U256,
    /// `interest + principal_paid` - what the payer owes for this repayment.
    pub total_paid: U256,
    /// Collateral freed and owed back to the pledger.
    pub gratis_returned_minor: U256,
    pub asset: Address,
    /// True when this repayment drove the outstanding principal to zero.
    pub closed: bool,
}

/// What a forfeit writes off. `gratis_burned_minor` is the unpaid share of the collateral.
/// Principal written off is never collected. Unpaid interest is not booked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forfeit {
    pub gratis_burned_minor: U256,
    pub principal_written_off: U256,
}

/// The anchor marked up by the call rate.
pub fn calc_call_price(price: U256) -> Result<U256> {
    outbe_primitives::markup::marked_up(price, CALL_RATE_PCT)
        .ok_or_else(|| CredisError::ArithmeticOverflow.into())
}

/// Issues a Credis and returns its derived
/// `credis_id = keccak256(cca || owner || asset || block_number)`.
///
/// This function seals everything the Credis will ever need:
///
/// - The call price derives from `call_anchor_price_minor`.
/// - `policy_rate` is pinned.
/// - The call terms are snapshotted from the profile. The Credis's group seals its
///   first member's, so a later profile change cannot re-term a live Credis.
///
/// Collateral starts fully locked and the interest anchor starts at issuance.
/// Entry price, call anchor, and call price are not written again.
pub fn issue(credis: &mut CredisContract<'_>, params: IssueCredisParams) -> Result<U256> {
    let storage = credis.storage.clone();
    storage.with_checkpoint(|| {
        let terms = [
            params.principal_minor,
            params.gratis_minor,
            params.entry_price_minor,
            params.call_anchor_price_minor,
        ];
        if terms.iter().any(U256::is_zero) {
            return Err(CredisFactoryError::ZeroIssueTerm.into());
        }
        if params.source.is_zero() {
            return Err(CredisFactoryError::InvalidSource.into());
        }
        if params.owner.is_zero() {
            return Err(CredisFactoryError::InvalidOwner.into());
        }

        let credis_id = CredisContract::credis_id(
            params.cca,
            params.owner,
            params.asset,
            storage.block_number()?,
        );
        // One Credis per CCA/owner/asset tuple per execution block.
        if credis.credis_exists(credis_id)? {
            return Err(CredisFactoryError::CredisAlreadyExists.into());
        }
        let call_terms = outbe_credis::config::read(&storage)?;

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
            &credis.storage,
            params.cca,
            reward_day(&credis.storage)?,
            params.gratis_minor,
        )?;
        credis.create_credis_record(&record)?;
        credis.widen_scan_terms(&record)?;

        credis.emit(outbe_credis::precompile::ICredis::Transfer {
            from: Address::ZERO,
            to: params.owner,
            tokenId: credis_id,
        })?;
        Ok(credis_id)
    })
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
///   over-pull an over-payment. The caller charges `Repayment::total_paid`.
/// - Collateral release is principal-proportional, rounded up and capped by
///   the locked remainder. Final settlement releases exactly what is left.
pub fn settle(
    credis: &mut CredisContract<'_>,
    credis_id: U256,
    amount: U256,
    now: u64,
) -> Result<Repayment> {
    let mut record = credis.load_credis(credis_id)?;
    let state_before = record.lifecycle_state()?;
    match state_before {
        CredisState::Settled | CredisState::Forfeited => {
            return Err(CredisFactoryError::CredisClosed.into())
        }
        CredisState::Issued | CredisState::Called => {}
    }

    if state_before == CredisState::Called && now > settlement_deadline(&record) {
        return Err(CredisFactoryError::SettlementDeadlinePassed.into());
    }

    let days = CredisContract::elapsed_days(&record, now);
    let interest = CredisContract::accrued_interest(&record, now)?;
    if amount < interest {
        return Err(CredisFactoryError::PaymentBelowAccruedInterest.into());
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
    let group = record.group_key();
    // A settled Credis keeps the call and terms it read from its group.
    credis.update_credis_record(&record)?;
    if closed {
        credis.leave_group(group, credis_id)?;
    }

    emit(
        &credis.storage,
        &ICredisFactory::SettlementApplied {
            credisId: credis_id,
            interestPaidMinor: interest,
            principalPaidMinor: principal_paid,
            gratisReturnedMinor: gratis_returned_minor,
            outstandingPrincipalMinor: record.outstanding_principal_minor,
        },
    )?;
    if closed {
        emit(
            &credis.storage,
            &ICredisFactory::CredisSettled {
                credisId: credis_id,
            },
        )?;
    }
    credis.emit(outbe_credis::precompile::ICredis::MetadataUpdate {
        _tokenId: credis_id,
    })?;

    Ok(Repayment {
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

/// The owner gives up an Issued or Called Credis before its deadline, with the same
/// write-off and burn as a lapsed call.
pub fn forfeit_by_owner(
    credis: &mut CredisContract<'_>,
    credis_id: U256,
    caller: Address,
) -> Result<Forfeit> {
    let storage = credis.storage.clone();
    storage.with_checkpoint(|| {
        let record = credis.load_credis(credis_id)?;
        if caller != record.owner {
            return Err(CredisFactoryError::NotCredisOwner.into());
        }
        match effective_state(&record, block_time(&storage)?)? {
            CredisState::Issued | CredisState::Called => {}
            CredisState::Settled | CredisState::Forfeited => {
                return Err(CredisFactoryError::CredisClosed.into())
            }
        }
        delete_forfeited(credis, record)
    })
}

/// Deletes a forfeited Credis. Only the unpaid share is written off: every settlement
/// already released its proportional share. Rounded-up partial returns may leave zero
/// collateral even while principal remains outstanding.
///
/// Returns what the caller must burn.
pub fn delete_forfeited(credis: &mut CredisContract<'_>, record: Credis) -> Result<Forfeit> {
    if record.outstanding_principal_minor.is_zero() {
        return Err(CredisFactoryError::NothingOutstanding.into());
    }
    let credis_id = record.credis_id;
    let forfeit = Forfeit {
        gratis_burned_minor: record.outstanding_gratis_minor,
        principal_written_off: record.outstanding_principal_minor,
    };
    outbe_ccaregistry::api::credis_forfeited(
        &credis.storage,
        record.cca,
        reward_day(&credis.storage)?,
        forfeit.gratis_burned_minor,
    )?;
    credis.leave_group(record.group_key(), credis_id)?;
    credis.delete_credis_record(credis_id)?;

    credis.emit(outbe_credis::precompile::ICredis::CredisForfeited {
        credisId: credis_id,
        cca: record.cca,
        gratisBurnedMinor: forfeit.gratis_burned_minor,
        principalWrittenOffMinor: forfeit.principal_written_off,
    })?;
    credis.emit(outbe_credis::precompile::ICredis::Transfer {
        from: record.owner,
        to: Address::ZERO,
        tokenId: credis_id,
    })?;
    Ok(forfeit)
}
