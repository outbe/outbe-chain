//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_primitives::addresses::NOD_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_common::settlement::floor_to_asset_units;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodBucket, LoadedNodItem};
use outbe_nod::schema::{NodContract, NodIssueParams, NodItemState};

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use crate::sol_ext::IERC20;

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
            costAmountMinor: nod_api::cost_amount_minor(
                params.entry_price_minor,
                params.gratis_load_minor,
            )?,
        },
    )?;

    Ok(nod_id)
}

/// Owner-authorized exercise of one paid Nod.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// Pays a qualified Nod's exact cost and preserves it for later exercise.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
    paynote_proof: &[u8],
) -> Result<()> {
    let (item, bucket) = load_owned_nod(storage, scope, parent, caller, nod_id)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    if !bucket.body().is_qualified {
        return Err(NodFactoryError::NodNotQualified.into());
    }
    let deadline = nod_api::settlement_deadline(storage, item.body().bucket_key)?;
    if deadline != 0 && storage.timestamp()?.to::<u64>() > deadline {
        return Err(NodFactoryError::CallDeadlineExpired.into());
    }
    storage.clone().with_checkpoint(|| {
        let paid = discharge_cost(
            storage,
            item.body(),
            bucket.body().entry_price_minor,
            caller,
            paynote_proof,
        )?;
        nod_api::settle_nod(storage, scope, item, bucket)?;
        emit_event(
            storage,
            INodFactory::NodPaid {
                owner: caller,
                nodId: nod_id.to_u256(),
                asset: paid.asset,
                nullifier: paid.nullifier,
                amountCovered: paid.spend_amount,
            },
        )
    })
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
        caller,
        nod_id,
        nonce,
        auth,
    } = request;
    let (item, bucket) = load_owned_nod(storage, scope, parent, caller, nod_id)?;
    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }
    validate_pow(nod_id, nonce)?;
    let gratis_load_minor = item.body().gratis_load_minor;
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, scope, item, bucket)?;
        emit_event(
            storage,
            INodFactory::NodExercised {
                owner: caller,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        emit_event(
            storage,
            INodFactory::NodBurned {
                owner: caller,
                nodId: nod_id.to_u256(),
                gratisLoadMinor: gratis_load_minor,
            },
        )?;
        outbe_gratisfactory::api::mint(storage.clone(), caller, gratis_load_minor, auth)?;
        Ok(gratis_load_minor)
    })
}

fn load_owned_nod(
    storage: &StorageHandle<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    caller: Address,
    nod_id: WwdEntityId,
) -> Result<(LoadedNodItem, LoadedNodBucket)> {
    let item =
        nod_api::load_item(storage, scope, parent, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    if caller != item.body().owner {
        return Err(NodFactoryError::NotOwner.into());
    }
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

/// Discharges `item`'s cost by spending one PayNote.
///
/// The proof is the payment. `consume` books its nullifier before returning, so
/// the note cannot be spent twice; running inside the caller's checkpoint means
/// a later failure un-books it. It is called last, after the cheap
/// owner/qualification/deadline guards, so rejected settlement never pays for
/// verification.
fn discharge_cost(
    storage: &StorageHandle<'_>,
    item: &NodItemState,
    entry_price_minor: U256,
    caller: Address,
    paynote_proof: &[u8],
) -> Result<PaidCost> {
    let claim = outbe_paynote::api::consume(storage, paynote_proof)?;

    // PayNote notes are bearer instruments: the proof names its own owner and
    // anyone can relay it. Binding that owner to the caller is what stops an
    // observer from lifting a broadcast proof to pay for their own Nod.
    if claim.owner != caller {
        return Err(NodFactoryError::PayNoteOwnerMismatch {
            expected: caller,
            actual: claim.owner,
        }
        .into());
    }
    check_settlement_asset(storage, item.reference_currency, claim.asset)?;
    let cost = settlement_units(
        entry_price_minor,
        item.gratis_load_minor,
        read_decimals(storage, claim.asset)?,
    )?;
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

/// The Nod's cost in the settlement asset's minor units, floored once.
pub(crate) fn settlement_units(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    asset_decimals: u8,
) -> Result<U256> {
    const OBLIGATION_DECIMALS: u32 = 12;
    let obligation = entry_price_minor
        .checked_mul(gratis_load_minor)
        .ok_or_else(|| PrecompileError::Revert("nod cost overflow".into()))?;
    floor_to_asset_units(obligation, U256::ONE, OBLIGATION_DECIMALS, asset_decimals)
        .map_err(|e| NodFactoryError::from(e).into())
}

/// Reads the settlement asset's `decimals()` via a static sub-call.
fn read_decimals(storage: &StorageHandle<'_>, asset: Address) -> Result<u8> {
    let ret = storage.staticcall(asset, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("settlement asset decimals undecodable".into()))
}

/// Rejects a note whose asset the vault router does not register under
/// `reference_currency`.
///
/// The cost is denominated in the Nod's own reference currency, so any asset
/// registered under it settles the Nod: the registry lists interchangeable
/// alternatives, not a preference, and the payer picks which one their note
/// carries. An empty registry is a configuration error, not a payer one.
fn check_settlement_asset(
    storage: &StorageHandle<'_>,
    reference_currency: u16,
    asset: Address,
) -> Result<()> {
    let registered =
        outbe_vaultrouter::api::reference_currency_assets(storage, reference_currency)?;
    if !registered.contains(&asset) {
        return Err(NodFactoryError::PayNoteAssetMismatch {
            asset,
            reference_currency,
        }
        .into());
    }
    Ok(())
}

/// PoW gate for `mine_gratis`, delegating to the shared [`outbe_common::pow`]
/// scheme and mapping failures onto [`NodFactoryError`].
pub fn validate_pow(nod_id: WwdEntityId, nonce: u64) -> Result<()> {
    pow::validate_pow(nod_id.to_u256(), nonce).map_err(|e| NodFactoryError::from(e).into())
}

/// Shared PoW hash over `nod_id.to_be_bytes::<32>() || nonce.to_be_bytes()`.
pub fn compute_pow_hash(nod_id: WwdEntityId, nonce: u64) -> [u8; 32] {
    pow::compute_pow_hash(nod_id.to_u256(), nonce)
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
