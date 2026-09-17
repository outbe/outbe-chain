//! Quote creation pins both currencies and reserves stablecoins atomically with
//! the private Gratis debit. Cancellation restores Gratis and schedules refunds.
//! Mint/burn apply their Fidelity cohort change in the same journal entry.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use crate::sol_ext::IReferenceCurrency;
use outbe_gratis::api::{self as gratis, ModifyAuth, Quote, Terms};
use outbe_oracle::api::fresh_coen_rate_for;
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::scaled_math::checked_mul_div_floor;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::{checked_protocol_to_native, SCALE_1E6_U256};

/// Reads the pledged asset's ISO 4217 currency code via a static
/// `IReferenceCurrency.isoCode()` sub-call, mirroring credisfactory's
/// `read_iso_code`. The pledge is then priced against COEN/<that currency>
/// rather than a hardcoded pair, so it cannot be quoted in one currency while
/// the Credis position opens against another.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| GratisFactoryError::AssetIsoUndecodable.into())
}

/// Convert canonical six-decimal stablecoin raw units to six-decimal GRATIS,
/// rounded down in the user's favor (C34).
/// Gratis is priced at the COEN price because `mine_coen` converts the two 1:1.
/// Returns `(gratis_cost, rate)`.
fn convert_stables_to_gratis(
    storage: StorageHandle<'_>,
    amount_stables: U256,
    asset: Address,
) -> Result<(U256, U256)> {
    let iso_code = read_iso_code(&storage, asset)?;
    let rate = fresh_coen_rate_for(storage, iso_code)?;
    let gratis = checked_mul_div_floor(amount_stables, SCALE_1E6_U256, rate).map_err(|_| {
        let error: PrecompileError = GratisFactoryError::OracleConversionOverflow.into();
        error
    })?;
    Ok((gratis, rate))
}

/// Quote and reserve liquidity atomically with the private Gratis debit.
pub fn create_pledge_note(storage: StorageHandle<'_>, request: &[u8]) -> Result<Vec<u8>> {
    let request: outbe_tee::pledgenote::CreateRequest =
        outbe_tee::pledgenote::decode(request).map_err(PrecompileError::Revert)?;
    let Quote {
        asset,
        principal_minor,
        max_gratis_minor,
        reference_currency,
    } = request.quote.clone();
    if asset.is_zero() || principal_minor.is_zero() {
        return Err(GratisFactoryError::InvalidAmount.into());
    }
    storage.with_checkpoint(|| {
        let issuance_currency = read_iso_code(&storage, asset)?;
        outbe_oracle::api::check_reference_currency_with_storage(
            storage.clone(),
            reference_currency,
        )?;
        let (gratis_minor, issuance_price) =
            convert_stables_to_gratis(storage.clone(), principal_minor, asset)?;
        if gratis_minor.is_zero() || gratis_minor > max_gratis_minor {
            return Err(GratisFactoryError::GratisCapExceeded.into());
        }
        let entry_price_minor = if reference_currency == issuance_currency {
            issuance_price
        } else {
            fresh_coen_rate_for(storage.clone(), reference_currency)?
        };
        // The header timestamp is u64; reject malformed storage providers.
        let created_at: u64 = storage
            .timestamp()?
            .try_into()
            .map_err(|_| PrecompileError::Revert("invalid quote timestamp".into()))?;
        let valid_until = created_at
            .checked_add(outbe_tee::pledgenote::QUOTE_TTL_SECONDS)
            .ok_or_else(|| PrecompileError::Revert("quote timestamp overflow".into()))?;
        let terms = Terms {
            asset,
            principal_minor,
            gratis_minor,
            issuance_currency,
            reference_currency,
            entry_price_minor,
            created_at,
            valid_until,
        };
        let outcome = gratis::create_note(
            &storage,
            request.quote.clone(),
            terms,
            request.envelope.clone(),
        )?;
        outbe_vaultrouter::api::reserve(
            &storage,
            outcome.reservation_id,
            asset,
            principal_minor,
            valid_until,
        )?;
        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            IGratisFactory::PledgeNoteCreated {
                encryptedReceipt: outcome.encrypted_receipt.clone().into(),
            }
            .encode_log_data(),
        )?;
        Ok(outcome.encrypted_receipt)
    })
}

pub fn cancel_pledge_note(storage: StorageHandle<'_>, encrypted_auth: Vec<u8>) -> Result<Vec<u8>> {
    storage.with_checkpoint(|| {
        let outcome = gratis::cancel_note(&storage, encrypted_auth)?;
        outbe_vaultrouter::api::cancel_reservation(&storage, outcome.reservation_id)?;
        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            IGratisFactory::PledgeNoteCancelled {
                encryptedReceipt: outcome.encrypted_receipt.clone().into(),
            }
            .encode_log_data(),
        )?;
        Ok(outcome.encrypted_receipt)
    })
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The `GratisMinted` event is
/// emitted by the Gratis token.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    gratis::mint_with_fidelity(storage, account, amount, auth).map(|_| ())
}

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let native_amount = checked_protocol_to_native(amount)
        .ok_or_else(|| PrecompileError::Revert("native COEN amount overflow".into()))?;

    storage.with_checkpoint(|| {
        gratis::burn_with_fidelity(storage.clone(), account, amount, auth)?;

        // GRATIS stays at six decimals; the matching native COEN exits at 18 decimals.
        storage.increase_balance(account, native_amount)?;

        storage.emit_event(
            GRATIS_FACTORY_ADDRESS,
            SolEvent::encode_log_data(&IGratisFactory::CoenMined {
                sender: account,
                amount: native_amount,
            }),
        )?;

        Ok(native_amount)
    })
}
