//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_oracle::api::{settlement_fx_rates, VwapSnapshotId};
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E6_U256;

use outbe_common::pow;
use outbe_common::settlement::floor_to_asset_units;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodBucket, LoadedNodItem};
use outbe_nod::schema::{NodContract, NodIssueParams, NodItemState};

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use crate::sol_ext::{IReferenceCurrency, IERC20};
use outbe_vaultrouter::api::IVaultRouter;

/// Issues a Nod through the block-scoped compressed-body lifecycle.
pub fn issue_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: &NodIssueParams,
) -> Result<WwdEntityId> {
    issue_nod_at(
        storage,
        scope,
        parent,
        params,
        storage.timestamp()?.to::<u64>(),
    )
}

pub(crate) fn issue_nod_at(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    params: &NodIssueParams,
    issued_at: u64,
) -> Result<WwdEntityId> {
    if params.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }

    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;
    if nod_api::get_item(storage, scope, parent, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }

    issue_nod_inner(storage, params, issued_at, |item| {
        nod_api::add_nod(storage, scope, parent, item, params.entry_price_minor)
    })
}

fn issue_nod_inner(
    storage: &StorageHandle<'_>,
    params: &NodIssueParams,
    issued_at: u64,
    add: impl FnOnce(&NodItemState) -> Result<()>,
) -> Result<WwdEntityId> {
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;

    if !NodContract::is_issuable_entry(params.entry_price_minor) {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    let floor_price_minor = NodContract::floor_price_minor(params.entry_price_minor)
        .ok_or(NodFactoryError::EntryPriceOutOfBounds)?;
    let bucket_key = NodContract::bucket_key(
        params.worldwide_day,
        params.entry_price_minor,
        params.reference_currency,
    );

    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: params.owner,
        gratis_load_minor: params.gratis_load_minor,
        worldwide_day: params.worldwide_day,
        league_id: params.league_id,
        bucket_key,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        issued_at,
    };
    add(&item)?;

    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: params.owner,
            nodId: nod_id.to_u256(),
            worldwideDay: U256::from(u32::from(params.worldwide_day)),
            leagueId: U256::from(params.league_id),
            floorPriceMinor: floor_price_minor,
            gratisLoadMinor: params.gratis_load_minor,
            entryPriceMinor: params.entry_price_minor,
            settlementCostMinor: nod_api::settlement_cost_minor(
                params.entry_price_minor,
                params.gratis_load_minor,
            )?,
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

/// Pays a qualified or called Nod's known cost directly in ERC20 base units. An
/// issuance-currency payment must name the VWAP snapshot required at this block.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    asset: Address,
    snapshot_id: U256,
) -> Result<()> {
    let quote = |terms: &SettlementTerms, entry_price: U256| {
        let currency = accept_payment_asset(
            storage,
            asset,
            terms.issuance_currency,
            terms.reference_currency,
        )?;
        let (cost, snapshot) = cost_in_asset(storage, terms, entry_price, asset, currency)?;
        require_snapshot(snapshot, snapshot_id)?;
        Ok(cost)
    };
    settle(storage, scope, parent, nod_id, quote, |_, _, cost| {
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
        Ok(PaidCost {
            asset,
            nullifier: B256::ZERO,
            spend_amount: cost,
        })
    })
}

/// Pays a qualified or called Nod's exact cost by spending a PayNote.
pub fn settle_nod_with_paynote(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    paynote_proof: &[u8],
) -> Result<()> {
    settle(
        storage,
        scope,
        parent,
        nod_id,
        |_, _| Ok(()),
        |terms, entry_price, ()| discharge_cost(storage, nod_id, terms, entry_price, paynote_proof),
    )
}

/// Currency pair and load a settlement charges against. Callers copy them off
/// the item before `settle_nod` consumes the loaded body.
struct SettlementTerms {
    issuance_currency: u16,
    reference_currency: u16,
    gratis_load_minor: U256,
}

/// `quote` prices and authorizes the payment before any state changes. `pay`
/// then moves it after the transition, inside the same checkpoint.
fn settle<Q>(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    quote: impl FnOnce(&SettlementTerms, U256) -> Result<Q>,
    pay: impl FnOnce(&SettlementTerms, U256, Q) -> Result<PaidCost>,
) -> Result<()> {
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
        gratis_load_minor: item.body().gratis_load_minor,
    };
    let entry_price = bucket.body().entry_price_minor;
    let quoted = quote(&terms, entry_price)?;
    storage.clone().with_checkpoint(|| {
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::settle_nod(storage, scope, item, bucket)?;
        let paid = pay(&terms, entry_price, quoted)?;
        emit_event(
            storage,
            INodFactory::NodPaid {
                owner,
                nodId: nod_id.to_u256(),
                asset: paid.asset,
                nullifier: paid.nullifier,
                paymentMinor: paid.spend_amount,
            },
        )
    })
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
) -> Result<U256> {
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
    let gratis_load_minor = item.body().gratis_load_minor;
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, scope, item, bucket)?;
        emit_event(
            storage,
            INodFactory::NodExercised {
                owner,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        emit_event(
            storage,
            INodFactory::NodBurned {
                owner,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        // Anyone may submit. The Nod owner's modify key authorizes the mint.
        outbe_gratisfactory::api::mint(storage.clone(), owner, gratis_load_minor, auth)?;
        Ok(gratis_load_minor)
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

/// One discharged Nod cost, as it is reported by `NodPaid`.
struct PaidCost {
    asset: Address,
    nullifier: B256,
    spend_amount: U256,
}

/// Discharges a Nod's cost by spending one PayNote.
///
/// The proof is the payment. `consume` books its nullifier before returning, so
/// the note cannot be spent twice. Because it runs inside the caller's
/// checkpoint, a later failure un-books it. Settlement calls it last, after the
/// cheap qualification/deadline guards, so rejected settlement never pays for
/// verification.
fn discharge_cost(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
    terms: &SettlementTerms,
    entry_price_minor: U256,
    paynote_proof: &[u8],
) -> Result<PaidCost> {
    let claim = outbe_paynote::api::consume(storage, paynote_proof)?;

    let currency = accept_payment_asset(
        storage,
        claim.asset,
        terms.issuance_currency,
        terms.reference_currency,
    )?;
    let (cost, snapshot) = cost_in_asset(storage, terms, entry_price_minor, claim.asset, currency)?;
    let expected = outbe_paynote::api::settlement_context(
        outbe_paynote::api::SettlementDomain::Nod,
        B256::from(nod_id.to_u256()),
        U256::ONE,
        snapshot.map_or(U256::ZERO, VwapSnapshotId::to_u256),
    )?;
    if claim.context != expected {
        return Err(NodFactoryError::PayNoteContextMismatch {
            expected,
            actual: claim.context,
        }
        .into());
    }
    if claim.spend_amount != cost {
        return Err(NodFactoryError::PayNoteCostMismatch {
            covered: claim.spend_amount,
            required: cost,
        }
        .into());
    }

    Ok(PaidCost {
        asset: claim.asset,
        nullifier: claim.nullifier,
        spend_amount: claim.spend_amount,
    })
}

/// Which of a Nod's two currencies a payment asset is denominated in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaymentCurrency {
    Reference,
    Issuance,
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
    let (rate, snapshot) = match currency {
        PaymentCurrency::Reference => (None, None),
        PaymentCurrency::Issuance => {
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
            (Some(rate), Some(fx.snapshot))
        }
    };
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
        gratis_load_minor: item.body().gratis_load_minor,
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
