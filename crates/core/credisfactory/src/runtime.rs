//! Orchestration logic for the credisfactory precompile.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;

use outbe_credis::{AnadosisResult, CredisContract};
use outbe_oracle::api::{get_exchange_rate, get_refinancing_rate};
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::schema::CredisFactoryContract;
use crate::sol_ext::{IReferenceCurrency, IERC20};

/// Native token base symbol used for the COEN/USD oracle pair lookup.
pub const NATIVE_TOKEN: &str = "COEN";

/// Stable settlement quote symbol. Matches the pair used by tributefactory and
/// metadosis.
pub const STABLECOIN: &str = "0xUSD";

/// Decimal-gap factor between COEN (10^18) and stablecoin (10^6). Cosmos:
/// `decimalsDiff = sdk.NewIntWithDecimal(1, 12)`.
fn decimals_diff() -> U256 {
    U256::from(1_000_000_000_000u128)
}

// ---------------------------------------------------------------------------
// request_credis
// ---------------------------------------------------------------------------

/// Consumes a confidential Gratis pledge (identified by `pledge_handle` +
/// `spend_auth`, which binds it to `bundle_account`), moves the collateral into
/// the `CREDIS_ADDRESS` escrow, opens a credis position bound to `bundleAccount`,
/// persists the pledge handle for the later per-installment unlock, and delivers
/// the stablecoin loan via the vault sub-call. Returns `(position_id, amount_stables)`.
///
// TODO(privacy): `eoa_account` is passed in plaintext calldata so external
// observers can read the pledger's address. Carry it in a client-encrypted blob
// (decrypted inside the enclave) in a future slice.
pub fn request_credis(
    storage: StorageHandle<'_>,
    _caller: Address,
    asset: Address,
    bundle_account: Address,
    eoa_account: Address,
    pledge_handle: B256,
    spend_auth: [u8; 32],
) -> Result<(U256, U256)> {
    if asset.is_zero() {
        return Err(CredisFactoryError::InvalidAsset.into());
    }
    if bundle_account.is_zero() {
        return Err(CredisFactoryError::InvalidBundleAccount.into());
    }

    // Block timestamp is read from the execution frame rather than threaded in
    // by the caller.
    let current_time = storage.timestamp()?.to::<u64>();

    // Reject borrowers with overdue anadosis on any of their positions.
    {
        let credis = CredisContract::new(storage.clone());
        if credis.has_overdue_anadosis(bundle_account, current_time)? {
            return Err(CredisFactoryError::OverduePayments.into());
        }
    }

    // Consume the pledge (the enclave verifies `spend_auth` binds it to
    // `bundle_account`, so a mempool copy cannot redirect the loan) and move the
    // collateral into the CREDIS_ADDRESS escrow. The enclave checks `eoa_account`
    // matches the pledge record. Returns the pledged gratis amount.
    let gratis_amount = outbe_gratis::api::pledge_to_bundle(
        storage.clone(),
        pledge_handle,
        bundle_account,
        eoa_account,
        spend_auth,
    )?;

    let amount_stables = convert_gratis_to_stables(storage.clone(), gratis_amount)?;

    // Derive the issuance currency from the disbursed asset (it self-reports its
    // ISO 4217 code via `IReferenceCurrency.isoCode()`) and pin the matching
    // refinancing rate read from the Oracle's reference-currency collection.
    let issuance_currency = read_iso_code(&storage, asset)?;
    let refinancing_rate = get_refinancing_rate(storage.clone(), issuance_currency)?;

    // Open the credis position. The `commitment` argument to `create_position`
    // builds the position_id; we use the globally-unique pledge handle.
    let handle_id = U256::from_be_bytes(pledge_handle.0);
    let mut credis = CredisContract::new(storage.clone());
    let position_id = credis.create_position(
        handle_id,
        bundle_account,
        asset,
        issuance_currency,
        refinancing_rate,
        amount_stables,
        gratis_amount,
        current_time,
    )?;

    // Persist the pledge handle so `pay_anadosis` can address the right pledge
    // record one installment at a time. The pledger EOA is deliberately NOT stored
    // (the caller re-supplies it at `anadosis`), so the position carries no durable
    // EOA↔bundle linkage.
    {
        let factory = CredisFactoryContract::new(storage.clone());
        factory
            .position_pledge_handle
            .write(&position_id, pledge_handle)?;
    }

    // Withdraw the matching stablecoin from the vault to the smart account.
    outbe_vaultprovider::api::withdraw_liquidity(&storage, asset, amount_stables, bundle_account)?;

    storage.emit_event(
        CREDIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisRequested {
            bundleAccount: bundle_account,
            amount: amount_stables,
        }),
    )?;

    Ok((position_id, amount_stables))
}

// ---------------------------------------------------------------------------
// pay_anadosis
// ---------------------------------------------------------------------------

/// Advances the credis position by one anadosis installment and releases 1/N of
/// the escrowed collateral from `CREDIS_ADDRESS` back to `eoa_account`'s
/// confidential Gratis balance. The enclave checks `eoa_account` matches the
/// pledge record, so the release can only reach the rightful pledger. The paid
/// installment (the ERC20 → vault deposit below) is itself the authorization for
/// the release — no separate proof is required.
///
// TODO(privacy): `eoa_account` is passed in plaintext calldata. Carry it in a
// client-encrypted blob (decrypted inside the enclave) in a future slice.
pub fn pay_anadosis(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    eoa_account: Address,
) -> Result<AnadosisResult> {
    // Read-only validation pass before any mutation.
    {
        let credis_ro = CredisContract::new(storage.clone());
        let position = credis_ro.get_position(position_id)?;
        let next = credis_ro
            .get_next_anadosis(position_id)?
            .ok_or(CredisFactoryError::PositionCompleted)?;

        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        if next.anadosis_amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }
        if caller != position.bundle_account {
            return Err(CredisFactoryError::UnauthorizedCaller.into());
        }
    }

    let current_time = storage.timestamp()?.to::<u64>();
    let mut credis = CredisContract::new(storage.clone());
    let result = credis.make_next_anadosis(position_id, current_time)?;

    // ERC20 + vault sequence. Sub-call reverts propagate out and unwind the
    // bookkeeping via the surrounding precompile frame.
    let amount = result.anadosis_amount;
    let asset = result.asset;

    // 1) Pull stablecoin from caller into the credisfactory precompile address.
    let transfer = IERC20::transferFromCall {
        from: caller,
        to: CREDIS_FACTORY_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, transfer.into())?;

    // 2) Approve the vault to spend that exact amount.
    let approve = IERC20::approveCall {
        spender: VAULT_PROVIDER_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, approve.into())?;

    // 3) Vault pulls and deposits into the reserve vault via its Solidity ABI.
    outbe_vaultprovider::api::deposit_liquidity(&storage, asset, amount)?;

    // 4) Release this installment's share of the escrowed collateral from
    //    CREDIS_ADDRESS back to the pledger's encrypted Gratis balance. The enclave
    //    checks `eoa_account` binds to the pledge record.
    let factory = CredisFactoryContract::new(storage.clone());
    let pledge_handle = factory.position_pledge_handle.read(&position_id)?;
    outbe_gratis::api::unlock_to_eoa(storage.clone(), eoa_account, pledge_handle)?;

    Ok(result)
}

// ---------------------------------------------------------------------------
// Oracle conversion (gratis 10^18 → stablecoin 10^6)
// ---------------------------------------------------------------------------

/// Reads the disbursed asset's ISO 4217 currency code via a static
/// `IReferenceCurrency.isoCode()` sub-call. Mirrors the `staticcall` +
/// `abi_decode_returns` pattern used by intexfactory's ERC20 reads.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| CredisFactoryError::AssetIsoUndecodable.into())
}

/// Cosmos formula: `amountStables = gratisAmount * rateInt18 / (decimalsDiff * precision)`.
fn convert_gratis_to_stables(storage: StorageHandle<'_>, gratis_amount: U256) -> Result<U256> {
    let rate = get_exchange_rate(storage, NATIVE_TOKEN, STABLECOIN)?;
    let numerator = gratis_amount
        .checked_mul(rate)
        .ok_or_else(|| -> PrecompileError {
            CredisFactoryError::OracleConversionOverflow.into()
        })?;
    let denominator =
        decimals_diff()
            .checked_mul(SCALE_1E18)
            .ok_or_else(|| -> PrecompileError {
                CredisFactoryError::OracleConversionOverflow.into()
            })?;
    Ok(numerator / denominator)
}
