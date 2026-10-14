//! NodFactory runtime: issuance, settlement, PoW-gated exercise, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::NOD_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodBucket, LoadedNodItem};
use outbe_nod::schema::{CallTerms, NodBucketState, NodContract, NodItemState};
use outbe_primitives::nod_encryption::EncryptedNodV2;

use crate::errors::NodFactoryError;
use crate::precompile::INodFactory;
use outbe_vaultrouter::settlement::{self, Currencies, Obligation};

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
    let nod_id = outbe_nod::identity::generate_nod_id(terms.owner, terms.worldwide_day)?;
    if terms.nod_id != nod_id
        || terms.chain_id != storage.chain_id()?
        || !encrypted.has_valid_encoding()
    {
        return Err(NodFactoryError::InvalidMaterializationProof.into());
    }
    if nod_api::get_item(storage, scope, parent, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }
    if !outbe_nod::pricing::is_issuable_entry(terms.entry_price_minor) {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    let floor_price_minor = outbe_nod::pricing::floor_price_minor(terms.entry_price_minor)
        .ok_or(NodFactoryError::EntryPriceOutOfBounds)?;
    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: terms.owner,
        encrypted: encrypted.clone(),
        worldwide_day: terms.worldwide_day,
        league_id: terms.league_id,
        bucket_key: outbe_nod::identity::bucket_key(
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
    let call_terms = call_terms(storage, terms.entry_price_minor, terms.reference_currency)?;
    nod_api::add_nod(
        storage,
        outbe_compressed_entities::ExecutionReaders { scope, parent },
        &item,
        terms.entry_price_minor,
        call_terms,
    )?;
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
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &obligation(item.body(), bucket.body())?,
    )?;
    quote.require_snapshot(snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::record_settled(storage, scope, item, bucket)?;
        settlement::collect(
            storage,
            NOD_FACTORY_ADDRESS,
            caller,
            asset,
            quote.payment_minor,
        )?;
        emit_event(
            storage,
            INodFactory::NodSettled {
                owner,
                nodId: nod_id.to_u256(),
                asset,
                paymentMinor: quote.payment_minor,
            },
        )
    })
}

/// Cost of one Nod at `entry_price_minor` and `gratis_load_minor` in an asset.
#[cfg(test)]
pub(crate) fn settlement_units(
    entry_price_minor: U256,
    gratis_load_minor: U256,
    rate: Option<(U256, U256)>,
    asset_decimals: u8,
) -> Result<U256> {
    let obligation = Obligation {
        entry_price_minor,
        load_minor: gratis_load_minor,
        units: U256::ONE,
        cost_rate_pct: 100,
    };
    outbe_common::settlement::obligation_units(&obligation, rate, asset_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

fn currencies(item: &NodItemState) -> Currencies {
    Currencies {
        issuance: item.issuance_currency,
        reference: item.reference_currency,
    }
}

/// One Nod: its bucket's entry price times its Gratis load.
fn obligation(item: &NodItemState, bucket: &NodBucketState) -> Result<Obligation> {
    Ok(Obligation {
        entry_price_minor: bucket.entry_price_minor,
        load_minor: nod_api::calculation_amount(item)?,
        units: U256::ONE,
        cost_rate_pct: 100,
    })
}

/// The call terms a bucket opened at `entry_price_minor` seals: the entry marked up by
/// the live profile's call rate, plus the profile's window, threshold and notice.
///
/// `None` when entry is zero: a zero call price would fire on the first scan. Every
/// later check reads the bucket's sealed copy, so a retune cannot re-term an issued Nod.
pub fn call_terms(
    storage: &StorageHandle<'_>,
    entry_price_minor: U256,
    reference_currency: u16,
) -> Result<Option<CallTerms>> {
    if entry_price_minor.is_zero() {
        return Ok(None);
    }
    let params = outbe_nod::config::read(storage)?;
    let call_price = outbe_primitives::markup::marked_up(entry_price_minor, params.call_rate)
        .ok_or_else(|| PrecompileError::Fatal("Nod call price overflow".into()))?;
    Ok(Some(CallTerms {
        call_price_minor: call_price,
        reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    }))
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
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &obligation(item.body(), bucket.body())?,
    )?;
    Ok((
        quote.settlement_currency,
        quote.payment_minor,
        quote.snapshot_id(),
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

pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
