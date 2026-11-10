//! The rules that change a Credis: issue on a reservation, settle interest first, delete
//! a forfeited one. Its series is called and forfeited by the sweeps. Credis keeps the
//! record and its reads.

use alloy_primitives::{Address, U256};
use outbe_credis::errors::CredisError;
use outbe_credis::runtime::effective_state;
use outbe_credis::{Credis, CredisContract, CredisState};
use outbe_primitives::error::Result;
use outbe_primitives::series_id::SeriesId;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{timestamp_to_date_key, SECONDS_PER_DAY};

use crate::called::emit;
use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;

/// Current execution timestamp's UTC reward day key (YYYYMMDD).
fn reward_day(storage: &StorageHandle<'_>) -> Result<u32> {
    Ok(timestamp_to_date_key(storage.now_secs()?))
}

pub(crate) fn date_step_error(
    err: outbe_primitives::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => CredisError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

/// Terms captured when a Credis is issued. Grouped rather than passed positionally
/// so a mis-ordered `U256` cannot silently swap principal for collateral.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    pub series_id: SeriesId,
    pub call_start_day: u32,
    pub call_rate: u16,
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
}

/// Outcome of [`CredisContract::settle`]. The caller moves the money. It pulls
/// `amount_minor` from the payer into the vault. It releases `gratis_released_minor` to
/// the pledger, never to the payer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repayment {
    /// Interest collected, in full, before any principal.
    pub interest_settled_minor: U256,
    /// Principal covered by this payment.
    pub principal_settled_minor: U256,
    /// `interest_settled_minor + principal_settled_minor` - what the payer owes for this repayment.
    pub amount_minor: U256,
    /// Collateral freed and owed back to the pledger.
    pub gratis_released_minor: U256,
    pub asset: Address,
    /// True when this repayment drove the outstanding principal to zero.
    pub closed: bool,
}

/// What a forfeit writes off. `forfeited_gratis_minor` is the unpaid share of the collateral.
/// Principal written off is never collected. Unpaid interest is not booked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forfeit {
    pub forfeited_gratis_minor: U256,
    pub forfeited_principal_minor: U256,
}

/// The anchor marked up by `rate_pct` percentage points.
pub fn calc_call_price(price: U256, rate_pct: u16) -> Result<U256> {
    outbe_common::markup::marked_up(price, rate_pct)
        .ok_or_else(|| CredisError::ArithmeticOverflow.into())
}

/// Issues a Credis and returns its derived
/// `credis_id = keccak256(series_id || owner || block_number)`.
#[cfg(test)]
pub fn issue(credis: &mut CredisContract<'_>, mut params: IssueCredisParams) -> Result<U256> {
    if params.series_id == SeriesId::default() {
        params.series_id = CredisContract::series_of(
            params.issued_at,
            params.issuance_currency,
            params.reference_currency,
        )?;
    }
    if params.call_start_day == 0 {
        params.call_start_day =
            outbe_primitives::time::first_full_day(params.issued_at).map_err(date_step_error)?;
    }
    if params.call_window_seconds == 0 {
        let call_terms = outbe_credis::config::read(&credis.storage)?;
        params.call_rate = call_terms.call_rate;
        params.call_window_seconds = call_terms.call_window_seconds;
        params.call_threshold_seconds = call_terms.call_threshold_seconds;
        params.call_notice_period_seconds = call_terms.call_notice_period_seconds;
    }
    let (id, _) = issue_in_series(credis, params)?;
    Ok(id)
}

/// Issues a Credis using the terms fixed in `params` at reservation time and returns
/// `(credis_id, newly_opened)`.
pub fn issue_in_series(
    credis: &mut CredisContract<'_>,
    params: IssueCredisParams,
) -> Result<(U256, bool)> {
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

        let series_id = params.series_id;
        let call_start_day = params.call_start_day;

        let credis_id =
            CredisContract::generate_credis_id(series_id, params.owner, storage.block_number()?);
        // One Credis per series/owner tuple per execution block.
        if credis.credis_exists(credis_id)? {
            return Err(CredisFactoryError::CredisAlreadyExists.into());
        }

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
            call_price_minor: calc_call_price(params.call_anchor_price_minor, params.call_rate)?,
            issued_at: params.issued_at,
            last_settled_at: params.issued_at,
            called_at: 0,
            series_id,
            call_notice_period_seconds: params.call_notice_period_seconds,
            call_rate: params.call_rate,
            call_window_seconds: params.call_window_seconds,
            call_threshold_seconds: params.call_threshold_seconds,
            call_anchor_price_minor: params.call_anchor_price_minor,
            interest_settled_minor: U256::ZERO,
        };
        outbe_ccaregistry::api::credis_issued(
            &credis.storage,
            params.cca,
            reward_day(&credis.storage)?,
            params.gratis_minor,
        )?;
        let newly_opened = credis.create_credis_record(&record, call_start_day)?;
        credis.widen_scan_terms(&record)?;

        Ok((credis_id, newly_opened))
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
///   over-pull an over-payment. The caller charges `Repayment::amount_minor`.
/// - Collateral release is principal-proportional, rounded up and capped by
///   the locked remainder. Final settlement releases exactly what is left.
pub fn settle(
    credis: &mut CredisContract<'_>,
    credis_id: U256,
    amount: U256,
    now: u64,
) -> Result<Repayment> {
    let mut record = credis.load_credis(credis_id)?;
    if record.is_settled() {
        return Err(CredisFactoryError::CredisClosed.into());
    }
    if effective_state(&record, now)? == CredisState::Forfeited {
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
    let gratis_released_minor = if principal_paid == record.outstanding_principal_minor {
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
        .checked_sub(gratis_released_minor)
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
    record.interest_settled_minor = record
        .interest_settled_minor
        .checked_add(interest)
        .ok_or(CredisError::ArithmeticOverflow)?;

    let closed = record.is_settled();
    let series = record.series_id;
    // A settled Credis keeps the call and terms it read from its series.
    credis.update_credis_record(&record)?;
    credis.take_owner_outstanding(record.owner, principal_paid)?;
    credis.take_series_unpaid_gratis(series, gratis_released_minor)?;
    if closed {
        credis.leave_series(series, credis_id)?;
    }

    let total_paid = interest
        .checked_add(principal_paid)
        .ok_or(CredisError::ArithmeticOverflow)?;
    emit(
        &credis.storage,
        &ICredisFactory::CredisSettled {
            credisId: credis_id,
            owner: record.owner,
            asset: record.asset,
            issuanceCurrency: record.issuance_currency,
            referenceCurrency: record.reference_currency,
            amountMinor: total_paid,
            interestSettledMinor: interest,
            principalSettledMinor: principal_paid,
        },
    )?;

    Ok(Repayment {
        interest_settled_minor: interest,
        principal_settled_minor: principal_paid,
        amount_minor: total_paid,
        gratis_released_minor,
        asset: record.asset,
        closed,
    })
}

/// Validates that `caller` may forfeit `record` in the current block.
pub(crate) fn validate_forfeit_by_owner(
    credis: &CredisContract<'_>,
    record: &Credis,
    caller: Address,
) -> Result<()> {
    if caller != record.owner {
        return Err(CredisFactoryError::NotCredisOwner.into());
    }
    if record.credis_id
        == CredisContract::generate_credis_id(
            record.series_id,
            record.owner,
            credis.storage.block_number()?,
        )
    {
        return Err(CredisFactoryError::CannotForfeitInIssueBlock.into());
    }
    match effective_state(record, credis.storage.now_secs()?)? {
        CredisState::Issued | CredisState::Called => Ok(()),
        CredisState::Settled | CredisState::Forfeited => {
            Err(CredisFactoryError::CredisClosed.into())
        }
    }
}

/// Deletes a forfeited Credis. Only the unpaid share is written off: every settlement
/// already released its proportional share. Rounded-up partial returns may leave zero
/// collateral even while principal remains outstanding.
///
/// Returns what the caller burned.
pub fn delete_forfeited(credis: &mut CredisContract<'_>, record: Credis) -> Result<Forfeit> {
    if record.outstanding_principal_minor.is_zero() {
        return Err(CredisFactoryError::NothingOutstanding.into());
    }
    let credis_id = record.credis_id;
    let forfeit = Forfeit {
        forfeited_gratis_minor: record.outstanding_gratis_minor,
        forfeited_principal_minor: record.outstanding_principal_minor,
    };
    outbe_ccaregistry::api::credis_forfeited(
        &credis.storage,
        record.cca,
        reward_day(&credis.storage)?,
        forfeit.forfeited_gratis_minor,
    )?;
    credis.emit(outbe_credis::precompile::ICredis::CredisForfeited {
        credisId: credis_id,
        owner: record.owner,
        cca: record.cca,
        asset: record.asset,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        forfeitedPrincipalMinor: forfeit.forfeited_principal_minor,
    })?;
    credis.leave_series(record.series_id, credis_id)?;
    credis.take_series_unpaid_gratis(record.series_id, forfeit.forfeited_gratis_minor)?;
    credis.delete_credis_record(credis_id)?;
    Ok(forfeit)
}
