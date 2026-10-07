//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_oracle::api::{settlement_fx_rates, VwapSnapshotId};
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E6_U256;

use outbe_common::pow;
use outbe_common::settlement::{floor_to_asset_units, PaymentCurrency};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodBucket, LoadedNodItem};
use outbe_nod::schema::{NodContract, NodItemState};
use outbe_primitives::nod_encryption::EncryptedNodV2;

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use crate::sol_ext::{IReferenceCurrency, IERC20};
use outbe_vaultrouter::api::IVaultRouter;

/// Issues an authenticated encrypted Nod through the block-scoped body lifecycle.
pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    encrypted: &EncryptedNodV2,
) -> Result<WwdEntityId> {
    issue_nod_at(
        storage,
        scope,
        parent,
        encrypted,
        storage.timestamp()?.to::<u64>(),
    )
}

pub(crate) fn issue_nod_at(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    encrypted: &EncryptedNodV2,
    issued_at: u64,
) -> Result<WwdEntityId> {
    let terms = &encrypted.terms;
    if terms.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }
    let nod_id = NodContract::generate_nod_id(terms.owner, terms.worldwide_day)?;
    if terms.nod_id != nod_id
        || terms.chain_id != storage.chain_id()?
        || !encrypted.has_valid_encoding()
    {
        return Err(NodFactoryError::InvalidMaterializationProof.into());
    }
    if nod_api::get_item(storage, scope, parent, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }
    if !NodContract::is_issuable_entry(terms.entry_price_minor) {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    let floor_price_minor = NodContract::floor_price_minor(terms.entry_price_minor)
        .ok_or(NodFactoryError::EntryPriceOutOfBounds)?;
    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: terms.owner,
        encrypted: encrypted.clone(),
        worldwide_day: terms.worldwide_day,
        league_id: terms.league_id,
        bucket_key: NodContract::bucket_key(
            terms.worldwide_day,
            terms.entry_price_minor,
            terms.reference_currency,
        ),
        issuance_currency: terms.issuance_currency,
        reference_currency: terms.reference_currency,
        issued_at,
    };
    // Existing settlement pricing remains public in this privacy stage.
    let settlement_cost_minor = nod_api::settlement_cost_minor(
        terms.entry_price_minor,
        nod_api::calculation_amount(&item)?,
    )?;
    nod_api::add_nod(storage, scope, parent, &item, terms.entry_price_minor)?;
    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: terms.owner,
            nodId: nod_id.to_u256(),
            worldwideDay: U256::from(u32::from(terms.worldwide_day)),
            leagueId: U256::from(terms.league_id),
            floorPriceMinor: floor_price_minor,
            encryptedGratisAmount: encrypted.encrypted_gratis_amount.clone().into(),
            entryPriceMinor: terms.entry_price_minor,
            settlementCostMinor: settlement_cost_minor,
        },
    )?;
    Ok(nod_id)
}

/// Exercise of one paid Nod. Any caller may submit. The owner's Gratis
/// modify-key MAC/`opNonce` authorizes the mint to that owner.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// One payment for a qualified or called Nod.
pub struct SettleNodRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub asset: Address,
    pub snapshot_id: U256,
}

/// Pays a qualified or called Nod's known cost in ERC20 base units of `asset`. An
/// issuance-currency payment must name the VWAP snapshot required at this block.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    request: SettleNodRequest,
) -> Result<()> {
    let SettleNodRequest {
        caller,
        nod_id,
        asset,
        snapshot_id,
    } = request;
    let (item, bucket) = load_nod(storage, scope, parent, nod_id)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    // The call check is one read; the qualification walk goes last.
    match nod_api::settlement_deadline(storage, item.body().bucket_key)? {
        0 if !nod_api::is_qualified(storage, bucket.body())? => {
            return Err(NodFactoryError::NodNotQualified.into());
        }
        0 => {}
        deadline if storage.timestamp()?.to::<u64>() > deadline => {
            return Err(NodFactoryError::CallDeadlineExpired.into());
        }
        _ => {}
    }
    let owner = item.body().owner;
    let terms = SettlementTerms {
        issuance_currency: item.body().issuance_currency,
        reference_currency: item.body().reference_currency,
        gratis_load_minor: nod_api::calculation_amount(item.body())?,
    };
    let currency = accept_payment_asset(
        storage,
        asset,
        terms.issuance_currency,
        terms.reference_currency,
    )?;
    let (cost, snapshot) = cost_in_asset(
        storage,
        &terms,
        bucket.body().entry_price_minor,
        asset,
        currency,
    )?;
    require_snapshot(snapshot, snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::settle_nod(storage, scope, item, bucket)?;
        if !cost.is_zero() {
            let before = token_balance(storage, asset)?;
            checked_token_call(
                storage,
                asset,
                IERC20::transferFromCall {
                    from: caller,
                    to: NOD_FACTORY_ADDRESS,
                    amount: cost,
                },
            )?;
            if token_balance(storage, asset)?.checked_sub(before) != Some(cost) {
                return Err(NodFactoryError::SettlementAmountMismatch.into());
            }
            checked_token_call(
                storage,
                asset,
                IERC20::approveCall {
                    spender: VAULT_ROUTER_ADDRESS,
                    amount: cost,
                },
            )?;
            outbe_vaultrouter::api::deposit(storage, asset, cost)?;
            if token_balance(storage, asset)? != before {
                return Err(NodFactoryError::SettlementAmountMismatch.into());
            }
        }
        emit_event(
            storage,
            INodFactory::NodPaid {
                owner,
                nodId: nod_id.to_u256(),
                asset,
                paymentMinor: cost,
            },
        )
    })
}

/// Currency pair and load a settlement charges against. Callers copy them off
/// the item before `nod_api::settle_nod` consumes the loaded body.
struct SettlementTerms {
    issuance_currency: u16,
    reference_currency: u16,
    gratis_load_minor: U256,
}

fn checked_token_call(
    storage: &StorageHandle<'_>,
    asset: Address,
    call: impl SolCall,
) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(NodFactoryError::TokenOperationFailed.into());
    }
    Ok(())
}

fn token_balance(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        asset,
        IERC20::balanceOfCall {
            account: NOD_FACTORY_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| NodFactoryError::TokenOperationFailed.into())
}

/// Exercises a paid entitlement without payment or a deadline. Mint failure
/// rolls back the removal, preserving the paid Nod for retry.
pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    request: MineGratisRequest,
) -> Result<Bytes> {
    let MineGratisRequest {
        nod_id,
        nonce,
        auth,
        ..
    } = request;
    let (item, bucket) = load_nod(storage, scope, parent, nod_id)?;
    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }
    let owner = item.body().owner;
    validate_pow(nod_id, owner, nonce)?;
    let encrypted = item.body().encrypted.clone();
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, scope, item, bucket)?;
        // The Nod owner authorizes the mint, including when a relayer submits it.
        outbe_gratisfactory::api::mint_encrypted_nod(storage.clone(), &encrypted, auth)?;
        emit_event(
            storage,
            INodFactory::NodExercised {
                owner,
                nodId: nod_id.to_u256(),
                encryptedGratisAmount: encrypted.encrypted_gratis_amount.clone().into(),
            },
        )?;
        emit_event(
            storage,
            INodFactory::NodBurned {
                owner,
                nodId: nod_id.to_u256(),
                encryptedGratisAmount: encrypted.encrypted_gratis_amount.into(),
            },
        )?;
        outbe_gratisfactory::api::encrypted_balance(storage.clone(), owner)
    })
}

fn load_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
) -> Result<(LoadedNodItem, LoadedNodBucket)> {
    let item =
        nod_api::load_item(storage, scope, parent, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if NodContract::new(storage.clone())
        .ocomp_certified_generation(item.body().worldwide_day)?
        .is_some_and(|generation| generation.next_nod_ordinal < generation.nod_count)
    {
        return Err(NodFactoryError::NodGenerationNotMaterialized.into());
    }
    let bucket_id =
        WwdEntityId::from_day_and_digest(item.body().worldwide_day, item.body().bucket_key.0);
    let bucket = nod_api::load_bucket(storage, scope, parent, bucket_id)?
        .ok_or(NodFactoryError::NodNotQualified)?;
    Ok((item, bucket))
}

/// Vaulted asset whose `isoCode()` is the Nod's reference or issuance currency.
/// The function checks registration first, so an unregistered asset need not
/// implement `isoCode()` at all. It matches the reference currency first, so a
/// same-currency Nod takes the no-rate branch.
fn accept_payment_asset(
    storage: &StorageHandle<'_>,
    asset: Address,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<PaymentCurrency> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset }
            .abi_encode()
            .into(),
    )?;
    let vaults = IVaultRouter::assetVaultsCountCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("assetVaultsCount undecodable".into()))?;
    if vaults.is_zero() {
        return Err(NodFactoryError::SettlementAssetNotRegistered { asset }.into());
    }

    let iso = asset_iso_code(storage, asset)?;
    if iso == reference_currency {
        return Ok(PaymentCurrency::Reference);
    }
    if iso == issuance_currency {
        return Ok(PaymentCurrency::Issuance);
    }
    Err(NodFactoryError::SettlementCurrencyMismatch { iso_code: iso }.into())
}

/// Rejects an issuance-rail payment authorized for any snapshot but the required one.
fn require_snapshot(required: Option<VwapSnapshotId>, authorized: U256) -> Result<()> {
    match required.map(VwapSnapshotId::to_u256) {
        Some(required) if required != authorized => Err(NodFactoryError::VwapSnapshotMismatch {
            authorized,
            required,
        }
        .into()),
        _ => Ok(()),
    }
}

/// Cost of one Nod in `asset`'s minor units and, on the issuance rail, the VWAP
/// snapshot both COEN legs came from. The function folds the cross rate into the
/// same fraction, so it floors the whole result once.
fn cost_in_asset(
    storage: &StorageHandle<'_>,
    terms: &SettlementTerms,
    entry_price_minor: U256,
    asset: Address,
    currency: PaymentCurrency,
) -> Result<(U256, Option<VwapSnapshotId>)> {
    let asset_decimals = read_decimals(storage, asset)?;
    let (rate, snapshot) = currency.conversion(|| -> Result<_> {
        let fx = settlement_fx_rates(
            storage.clone(),
            terms.issuance_currency,
            terms.reference_currency,
        )?
        .ok_or(NodFactoryError::OracleUnavailable)?;
        let rate = (
            fx.issuance_currency_vwap_minor,
            fx.reference_currency_vwap_minor,
        );
        Ok((rate, fx.snapshot))
    })?;
    let cost = settlement_units(
        entry_price_minor,
        terms.gratis_load_minor,
        rate,
        asset_decimals,
    )?;
    Ok((cost, snapshot))
}

/// The Nod's cost in the settlement asset's minor units, floored once.
/// Apply the one-reference-minor-unit minimum before asset/currency conversion.
/// `rate` is `(COEN/issuance, COEN/reference)` on the issuance rail.
pub(crate) fn settlement_units(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    const OBLIGATION_DECIMALS: u32 = 12;
    let overflow = || PrecompileError::Revert("nod cost overflow".into());
    let obligation = entry_price_minor
        .checked_mul(gratis_load_minor)
        .ok_or_else(overflow)?;
    // The product has twelve decimals: 1e6 is one six-decimal reference unit.
    // Preserve all precision above this minimum for the single-floor conversion.
    let obligation = if obligation.is_zero() {
        obligation
    } else {
        obligation.max(SCALE_1E6_U256)
    };
    let (numerator, denominator) = match rate {
        Some((to, from)) => (obligation.checked_mul(to).ok_or_else(overflow)?, from),
        None => (obligation, U256::ONE),
    };
    floor_to_asset_units(numerator, denominator, OBLIGATION_DECIMALS, asset_decimals)
        .map_err(|e| NodFactoryError::from(e).into())
}

/// Reads the settlement asset's `decimals()` via a static sub-call.
fn read_decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("settlement asset decimals undecodable".into()))
}

/// Reads the settlement asset's ISO 4217 code via a static sub-call.
fn asset_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("isoCode undecodable".into()))
}

/// What settling `nod_id` with `asset` costs, in which currency, and the VWAP
/// snapshot an issuance-currency payment must name (zero on the reference rail).
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    let (item, bucket) = load_nod(storage, scope, parent, nod_id)?;
    let terms = SettlementTerms {
        issuance_currency: item.body().issuance_currency,
        reference_currency: item.body().reference_currency,
        gratis_load_minor: nod_api::calculation_amount(item.body())?,
    };
    let currency = accept_payment_asset(
        storage,
        asset,
        terms.issuance_currency,
        terms.reference_currency,
    )?;
    let settlement_currency = match currency {
        PaymentCurrency::Reference => terms.reference_currency,
        PaymentCurrency::Issuance => terms.issuance_currency,
    };
    let (cost, snapshot) = cost_in_asset(
        storage,
        &terms,
        bucket.body().entry_price_minor,
        asset,
        currency,
    )?;
    Ok((
        settlement_currency,
        cost,
        snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256),
    ))
}

/// PoW gate for `mine_gratis`. The preimage is
/// `OUTBE_NOD_MINING_V1 || nodId || owner || miningSequence=0 || nonce`. The caller is not in
/// it.
pub fn validate_pow(nod_id: WwdEntityId, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(
        pow::MiningDomain::Nod,
        nod_id.to_u256(),
        owner,
        pow::SINGLE_EXERCISE_SEQUENCE,
        nonce,
    )
    .map_err(|e| NodFactoryError::from(e).into())
}

/// SHA256 over `OUTBE_NOD_MINING_V1 || nodId_be32 || owner_20 || miningSequence_be8 ||
/// nonce_be8` with `miningSequence = 0`.
pub fn compute_pow_hash(nod_id: WwdEntityId, owner: Address, nonce: u64) -> [u8; 32] {
    pow::compute_mining_pow_hash(
        pow::MiningDomain::Nod,
        nod_id.to_u256(),
        owner,
        pow::SINGLE_EXERCISE_SEQUENCE,
        nonce,
    )
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
