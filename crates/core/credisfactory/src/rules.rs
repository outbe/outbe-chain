//! The rules that change a Credis: issue on a reservation, settle fee first, delete
//! a forfeited one. Its call group is called and forfeited by the sweeps. Credis keeps the
//! record and its reads.

use alloy_primitives::{Address, U256};
use outbe_core_common::series_id::SeriesId;
use outbe_credis::errors::CredisError;
use outbe_credis::precompile::ICredis;
use outbe_credis::runtime::effective_state;
use outbe_credis::{CallTerms, Credis, CredisContract, CredisState, CredisView};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::{timestamp_to_date_key, SECONDS_PER_DAY};

use crate::errors::CredisFactoryError;

/// Current execution timestamp's UTC reward day key (YYYYMMDD).
fn reward_day(storage: &StorageHandle<'_>) -> Result<u32> {
    Ok(timestamp_to_date_key(storage.now_secs()?))
}

pub(crate) fn date_step_error(
    err: outbe_system_common::time::TimeError,
) -> outbe_primitives::error::PrecompileError {
    match err.date_step() {
        Some((kind, date_key)) => CredisError::DateStep { kind, date_key }.into(),
        None => outbe_primitives::error::PrecompileError::Fatal(err.to_string()),
    }
}

/// Terms captured when a Credis is issued. Grouped rather than passed positionally
/// so a mis-ordered `U256` cannot silently swap amount for collateral.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueCredisParams {
    pub owner: Address,
    pub cca: Address,
    /// Main account whose pledged Gratis backs the Credis.
    pub pledge_account: Address,
    pub asset: Address,
    /// Decimals of `asset` at the reservation.
    pub asset_decimals: u8,
    /// ISO 4217 numeric code of the disbursed `asset`.
    pub issuance_currency: u16,
    /// ISO 4217 numeric code of the reference currency elected at the reservation and
    /// fixed for the Credis's life.
    pub reference_currency: u16,
    /// `r`, the issuance currency's policy rate, 1e6 scaled.
    pub policy_rate: U256,
    /// `P` - stablecoin minor units disbursed.
    pub amount_minor: U256,
    /// 24-hour COEN/`reference_currency` VWAP at the reservation hour, scale 1e6.
    pub entry_price_minor: U256,
    /// `G` - pledged Gratis collateral.
    pub gratis_minor: U256,
    pub issued_at: u64,
    pub series_id: SeriesId,
    /// UTC hour of the reservation, 0-23.
    pub sub_series: u8,
    pub call_start_day: u32,
    pub call_rate: u16,
    pub call_window_seconds: u32,
    pub call_threshold_seconds: u32,
    pub call_notice_period_seconds: u32,
}

/// Outcome of [`CredisContract::settle`]. The caller moves the money. It pulls
/// `amount_minor` from the payer into the vault. It releases `unpledged_gratis_minor` to
/// the pledger, never to the payer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repayment {
    /// Fee collected, in full, before any of the amount.
    pub settled_fee_minor: U256,
    /// Amount covered by this payment.
    pub settled_amount_minor: U256,
    /// `settled_fee_minor + settled_amount_minor` - what the payer owes for this repayment.
    pub amount_minor: U256,
    /// Collateral freed and owed back to the pledger.
    pub unpledged_gratis_minor: U256,
    /// True when this repayment drove the outstanding amount to zero.
    pub closed: bool,
}

/// What a forfeit writes off. `forfeited_gratis_minor` is the unpaid share of the collateral.
/// Amount written off is never collected. Unpaid fee is not booked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forfeit {
    pub forfeited_gratis_minor: U256,
    pub forfeited_amount_minor: U256,
}

/// The entry price marked up by `rate_pct` percentage points.
pub fn compute_call_price(price: U256, rate_pct: u16) -> Result<U256> {
    outbe_core_common::markup::marked_up(price, rate_pct)
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
        params.sub_series = CredisContract::sub_series_of(params.issued_at);
    }
    if params.call_start_day == 0 {
        params.call_start_day =
            outbe_system_common::time::first_full_day(params.issued_at).map_err(date_step_error)?;
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
            params.amount_minor,
            params.gratis_minor,
            params.entry_price_minor,
        ];
        if terms.iter().any(U256::is_zero) {
            return Err(CredisFactoryError::ZeroIssueTerm.into());
        }
        if params.pledge_account.is_zero() {
            return Err(CredisFactoryError::InvalidPledgeAccount.into());
        }
        if params.owner.is_zero() {
            return Err(CredisFactoryError::InvalidOwner.into());
        }

        let series_id = params.series_id;
        let call_start_day = params.call_start_day;
        // An open sub-series keeps the prices its first Credis sealed.
        let (entry_price_minor, call_price_minor) =
            match credis.sub_series_prices(series_id, params.sub_series)? {
                Some(prices) => prices,
                None => (
                    params.entry_price_minor,
                    compute_call_price(params.entry_price_minor, params.call_rate)?,
                ),
            };

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
            asset_decimals: params.asset_decimals,
            issuance_currency: params.issuance_currency,
            reference_currency: params.reference_currency,
            pledge_account: params.pledge_account,
            amount_minor: params.amount_minor,
            outstanding_amount_minor: params.amount_minor,
            gratis_minor: params.gratis_minor,
            outstanding_gratis_minor: params.gratis_minor,
            policy_rate: params.policy_rate,
            entry_price_minor,
            call_price_minor,
            issued_at: params.issued_at,
            last_settled_at: params.issued_at,
            series_id,
            sub_series: params.sub_series,
            settled_fee_minor: U256::ZERO,
        };
        outbe_ccaregistry::api::credis_issued(
            &credis.storage,
            params.cca,
            reward_day(&credis.storage)?,
            params.gratis_minor,
        )?;
        let terms = CallTerms {
            call_window_seconds: params.call_window_seconds,
            call_threshold_seconds: params.call_threshold_seconds,
            call_notice_period_seconds: params.call_notice_period_seconds,
        };
        let newly_opened = credis.create_credis_record(&record, &terms, call_start_day)?;

        Ok((credis_id, newly_opened))
    })
}

/// Applies a settlement of `amount`: fee first, amount second.
///
/// Any payer may settle any Issued or Called Credis. The released collateral
/// is owed to the pledger recorded on the Credis. Thus a payer can never
/// redirect value to themselves.
///
/// - Called Credis accept repayment through deadline equality.
/// - The function rejects a payment below the accrued fee outright.
/// - The function consumes only what the Credis needs, so it does not
///   over-pull an over-payment. The caller charges `Repayment::amount_minor`.
/// - Collateral release is amount-proportional, rounded up and capped by
///   the locked remainder. Final settlement releases exactly what is left.
pub fn settle(
    credis: &mut CredisContract<'_>,
    credis_id: U256,
    amount: U256,
    now: u64,
    paid_asset: Address,
) -> Result<Repayment> {
    let mut record = credis.load_credis(credis_id)?;
    if record.is_settled() {
        return Err(CredisFactoryError::CredisClosed.into());
    }
    if effective_state(&record, now)? == CredisState::Forfeited {
        return Err(CredisFactoryError::SettlementDeadlinePassed.into());
    }

    let days = CredisContract::elapsed_days(&record, now);
    let fee = CredisContract::accrued_fee(&record, now)?;
    if amount < fee {
        return Err(CredisFactoryError::PaymentBelowAccruedFee.into());
    }
    let amount_paid = (amount - fee).min(record.outstanding_amount_minor);

    // C34 favors the user on each partial. Repeated ceilings can exhaust
    // collateral before the amount, so cap every return at the remainder.
    let unpledged_gratis_minor = if amount_paid == record.outstanding_amount_minor {
        record.outstanding_gratis_minor
    } else {
        record
            .gratis_minor
            .checked_mul(amount_paid)
            .ok_or(CredisError::ArithmeticOverflow)?
            .div_ceil(record.amount_minor)
            .min(record.outstanding_gratis_minor)
    };

    record.outstanding_amount_minor = record
        .outstanding_amount_minor
        .checked_sub(amount_paid)
        .ok_or(CredisError::ArithmeticOverflow)?;
    record.outstanding_gratis_minor = record
        .outstanding_gratis_minor
        .checked_sub(unpledged_gratis_minor)
        .ok_or(CredisError::ArithmeticOverflow)?;
    // Accrual restarts on the reduced amount. No unpaid fee ever
    // carries between settlements. The anchor advances by the whole days
    // actually charged, never to `now`. Settling on a sub-day boundary must
    // not discard the remainder. Otherwise, repeated dust settlements just
    // under 24h apart would hold `days` at zero and evade the coupon entirely.
    record.last_settled_at = record
        .last_settled_at
        .saturating_add(days.saturating_mul(SECONDS_PER_DAY));
    // Sum of successful fee deltas. Every revert path returns above this write.
    record.settled_fee_minor = record
        .settled_fee_minor
        .checked_add(fee)
        .ok_or(CredisError::ArithmeticOverflow)?;

    let closed = record.is_settled();
    credis.update_credis_record(&record)?;
    credis.take_owner_outstanding(record.owner, amount_paid)?;
    credis.take_outstanding_amount(&record, amount_paid)?;

    let total_paid = fee
        .checked_add(amount_paid)
        .ok_or(CredisError::ArithmeticOverflow)?;
    credis.emit(ICredis::CredisSettled {
        credisId: credis_id,
        owner: record.owner,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        asset: paid_asset,
        amountMinor: total_paid,
        feeMinor: fee,
        unpledgedGratisMinor: unpledged_gratis_minor,
    })?;
    if closed {
        credis.leave_group(&record)?;
        credis.delete_credis_record(credis_id)?;
        credis.emit(ICredis::CredisBurned {
            credisId: credis_id,
            owner: record.owner,
        })?;
    }

    Ok(Repayment {
        settled_fee_minor: fee,
        settled_amount_minor: amount_paid,
        amount_minor: total_paid,
        unpledged_gratis_minor,
        closed,
    })
}

/// A Credis id holds its issue block, so the id of this block names a Credis issued in it.
pub(crate) fn in_issue_block(credis: &CredisContract<'_>, record: &Credis) -> Result<bool> {
    Ok(record.credis_id
        == CredisContract::generate_credis_id(
            record.series_id,
            record.owner,
            credis.storage.block_number()?,
        ))
}

/// Validates that `caller` may forfeit `record` in the current block.
pub(crate) fn validate_forfeit_by_owner(
    credis: &CredisContract<'_>,
    record: &CredisView,
    caller: Address,
) -> Result<()> {
    if caller != record.owner {
        return Err(CredisFactoryError::NotCredisOwner.into());
    }
    if in_issue_block(credis, record)? {
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
/// collateral even while an amount remains outstanding.
///
/// Returns what the caller burned.
pub fn delete_forfeited(credis: &mut CredisContract<'_>, record: Credis) -> Result<Forfeit> {
    if record.outstanding_amount_minor.is_zero() {
        return Err(CredisFactoryError::NothingOutstanding.into());
    }
    let credis_id = record.credis_id;
    let forfeit = Forfeit {
        forfeited_gratis_minor: record.outstanding_gratis_minor,
        forfeited_amount_minor: record.outstanding_amount_minor,
    };
    outbe_ccaregistry::api::credis_forfeited(
        &credis.storage,
        record.cca,
        reward_day(&credis.storage)?,
        forfeit.forfeited_gratis_minor,
    )?;
    credis.emit(ICredis::CredisForfeited {
        credisId: credis_id,
        owner: record.owner,
        issuanceCurrency: record.issuance_currency,
        referenceCurrency: record.reference_currency,
        cca: record.cca,
        asset: record.asset,
        unsettledAmountMinor: forfeit.forfeited_amount_minor,
        burnedGratisMinor: forfeit.forfeited_gratis_minor,
    })?;
    credis.leave_group(&record)?;
    credis.take_outstanding_amount(&record, forfeit.forfeited_amount_minor)?;
    credis.delete_credis_record(credis_id)?;
    Ok(forfeit)
}
