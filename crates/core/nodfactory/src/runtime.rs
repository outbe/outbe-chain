//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_oracle::api::get_utc_day_vwap_for_iso;
use outbe_primitives::addresses::{NOD_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::{previous_date_key, timestamp_to_date_key};

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
    if params.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }

    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;
    if nod_api::get_item(storage, scope, parent, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }

    issue_nod_inner(storage, params, |item| {
        nod_api::add_nod(storage, scope, parent, item, params.entry_price_minor)
    })
}

fn issue_nod_inner(
    storage: &StorageHandle<'_>,
    params: &NodIssueParams,
    add: impl FnOnce(&NodItemState) -> Result<()>,
) -> Result<WwdEntityId> {
    let nod_id = NodContract::generate_nod_id(params.owner, params.worldwide_day)?;

    let bucket_key = NodContract::bucket_key(
        params.worldwide_day,
        params.floor_price_minor,
        params.reference_currency,
    );

    let issued_at = storage.timestamp()?.to::<u64>();

    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: params.owner,
        gratis_load_minor: params.gratis_load_minor,
        worldwide_day: params.worldwide_day,
        league_id: params.league_id,
        floor_price_minor: params.floor_price_minor,
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
            floorPriceMinor: params.floor_price_minor,
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

/// Exercise of one paid Nod. Any caller may submit; the owner's Gratis
/// modify-key MAC/`opNonce` authorizes the mint to that owner.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// Pays a qualified or called Nod's known cost directly in ERC20 base units.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<()> {
    settle(storage, scope, parent, nod_id, |terms, entry_price| {
        let currency = accept_payment_asset(
            storage,
            asset,
            terms.issuance_currency,
            terms.reference_currency,
        )?;
        let cost = cost_in_token(storage, terms, entry_price, asset, currency)?;
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
    settle(storage, scope, parent, nod_id, |terms, entry_price| {
        discharge_cost(storage, terms, entry_price, paynote_proof)
    })
}

/// Currency pair and load a settlement charges against. Copied off the item
/// before `settle_nod` consumes the loaded body.
struct SettlementTerms {
    owner_reference: Address,
    issuance_currency: u16,
    reference_currency: u16,
    gratis_load_minor: U256,
}

fn settle(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    pay: impl FnOnce(&SettlementTerms, U256) -> Result<PaidCost>,
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
    storage.clone().with_checkpoint(|| {
        let owner = item.body().owner;
        let terms = SettlementTerms {
            owner_reference: owner,
            issuance_currency: item.body().issuance_currency,
            reference_currency: item.body().reference_currency,
            gratis_load_minor: item.body().gratis_load_minor,
        };
        let entry_price = bucket.body().entry_price_minor;
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::settle_nod(storage, scope, item, bucket)?;
        let paid = pay(&terms, entry_price)?;
        emit_event(
            storage,
            INodFactory::NodPaid {
                owner,
                nodId: nod_id.to_u256(),
                asset: paid.asset,
                nullifier: paid.nullifier,
                amountCovered: paid.spend_amount,
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
        // Anyone may submit; mint is authorized by the Nod owner's modify key.
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
/// the note cannot be spent twice; running inside the caller's checkpoint means
/// a later failure un-books it. It is called last, after the cheap
/// qualification/deadline guards, so rejected settlement never pays for
/// verification.
fn discharge_cost(
    storage: &StorageHandle<'_>,
    terms: &SettlementTerms,
    entry_price_minor: U256,
    paynote_proof: &[u8],
) -> Result<PaidCost> {
    let claim = outbe_paynote::api::consume(storage, paynote_proof)?;

    // front-running protection
    if claim.owner != terms.owner_reference {
        return Err(NodFactoryError::PayNoteOwnerMismatch {
            expected: terms.owner_reference,
            actual: claim.owner,
        }
        .into());
    }

    let currency = accept_payment_asset(
        storage,
        claim.asset,
        terms.issuance_currency,
        terms.reference_currency,
    )?;
    let cost = cost_in_token(storage, terms, entry_price_minor, claim.asset, currency)?;
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
/// Registration is checked first, so an unregistered asset need not implement
/// `isoCode()` at all; reference is matched first, so a same-currency Nod takes
/// the no-rate branch.
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

/// COEN price of `iso_code` from the last closed UTC day.
fn day_coen_rate(storage: &StorageHandle<'_>, iso_code: u16, now: u64) -> Result<U256> {
    let day = previous_date_key(timestamp_to_date_key(now));
    get_utc_day_vwap_for_iso(storage.clone(), day, iso_code)?
        .ok_or_else(|| NodFactoryError::OracleUnavailable.into())
}

/// Cost of one Nod in `asset`'s minor units. The issuance rail folds the COEN
/// cross rate of the last closed UTC day into the same fraction, so the whole
/// thing is floored once.
fn cost_in_token(
    storage: &StorageHandle<'_>,
    terms: &SettlementTerms,
    entry_price_minor: U256,
    asset: Address,
    currency: PaymentCurrency,
) -> Result<U256> {
    let asset_decimals = read_decimals(storage, asset)?;
    let rate = match currency {
        PaymentCurrency::Reference => None,
        PaymentCurrency::Issuance => {
            // One timestamp, so both legs come from the same closed day.
            let now = storage.timestamp()?.to::<u64>();
            Some((
                day_coen_rate(storage, terms.issuance_currency, now)?,
                day_coen_rate(storage, terms.reference_currency, now)?,
            ))
        }
    };
    settlement_units(
        entry_price_minor,
        terms.gratis_load_minor,
        rate,
        asset_decimals,
    )
}

/// The Nod's cost in the settlement asset's minor units, floored once.
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

/// What settling `nod_id` with `asset` costs, and in which currency.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<(u16, U256)> {
    let (item, bucket) = load_nod(storage, scope, parent, nod_id)?;
    let terms = SettlementTerms {
        owner_reference: item.body().owner,
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
    Ok((
        settlement_currency,
        cost_in_token(
            storage,
            &terms,
            bucket.body().entry_price_minor,
            asset,
            currency,
        )?,
    ))
}

/// PoW gate for `mine_gratis`. The preimage is
/// `nodId || owner || miningSequence=0 || nonce`; the caller is not in it.
pub fn validate_pow(nod_id: WwdEntityId, owner: Address, nonce: u64) -> Result<()> {
    pow::validate_mining_pow(
        nod_id.to_u256(),
        owner,
        pow::SINGLE_EXERCISE_SEQUENCE,
        nonce,
    )
    .map_err(|e| NodFactoryError::from(e).into())
}

/// SHA256 over `nodId_be32 || owner_20 || miningSequence_be8 || nonce_be8`
/// with `miningSequence = 0`.
pub fn compute_pow_hash(nod_id: WwdEntityId, owner: Address, nonce: u64) -> [u8; 32] {
    pow::compute_mining_pow_hash(
        nod_id.to_u256(),
        owner,
        pow::SINGLE_EXERCISE_SEQUENCE,
        nonce,
    )
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
