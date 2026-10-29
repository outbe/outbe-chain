//! NodFactory runtime: issuance, settlement, PoW-gated mining, event emission.
//!
//! All persistent Nod state lives in the entity store at
//! [`outbe_primitives::addresses::NOD_ADDRESS`]. NodFactory mutates that
//! state exclusively through [`outbe_nod::api`] and emits its own events at
//! [`NOD_FACTORY_ADDRESS`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::NOD_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_common::pow;
use outbe_nod::api as nod_api;
use outbe_nod::api::{LoadedNodGroup, LoadedNodItem};
use outbe_nod::schema::{CallTerms, NodItemState};
use outbe_primitives::nod_encryption::EncryptedNodV1;
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::errors::{date_step_error, NodFactoryError};
use crate::precompile::INodFactory;
use outbe_vaultrouter::settlement::{self, Currencies, SettlementCost, SettlementPayment};

fn maintenance_context<'s>(
    storage: &StorageHandle<'s>,
) -> Result<outbe_primitives::block::BlockRuntimeContext<'s>> {
    use outbe_primitives::block::{BlockContext, BlockContextInput, BlockRuntimeContext};
    Ok(BlockRuntimeContext::new(
        BlockContext::new_with_genesis_hash(BlockContextInput {
            block_number: storage.block_number()?,
            timestamp: storage.now_secs()?,
            chain_id: storage.chain_id()?,
            genesis_hash: storage.genesis_hash()?,
            genesis_timestamp: storage.genesis_timestamp()?,
            proposer: Address::ZERO,
            validators: Vec::new(),
        }),
        storage.clone(),
    ))
}

pub fn maintain(
    storage: &StorageHandle<'_>,
    selection: outbe_oracle::call_pass::Selection,
) -> Result<()> {
    outbe_primitives::dispatch::ensure_mutation_allowed(storage)?;
    // TODO: Reward callers for maintenance work performed.
    if !selection.process_call && !selection.process_forfeit && !selection.process_cleanup {
        return Ok(());
    }
    let ctx = maintenance_context(storage)?;
    crate::called::maintain(&ctx, selection)?;
    if selection.process_cleanup {
        crate::cleanup::cleanup(&ctx)?;
    }
    Ok(())
}

pub fn has_maintenance_work(storage: &StorageHandle<'_>) -> Result<(bool, bool, bool)> {
    let ctx = maintenance_context(storage)?;
    let (call, forfeit, _) = crate::called::pending_work(&ctx)?;
    Ok((call, forfeit, crate::cleanup::pending_work(&ctx)?))
}

/// Issues an authenticated encrypted Nod through the block-scoped body lifecycle.
pub fn issue_nod(storage: &StorageHandle<'_>, encrypted: &EncryptedNodV1) -> Result<WwdEntityId> {
    issue_nod_at(storage, encrypted, storage.now_secs()?)
}

/// Input for sealing a Lysis series group in `Nod`.
#[derive(Debug, Clone, Copy)]
pub struct LysisSeriesInput {
    pub group_key: alloy_primitives::B256,
    pub emission_day: u32,
    pub reference_currency: u16,
    pub entry_price_minor: Option<U256>,
    pub unclaimed_tribute_count: u64,
    pub unclaimed_tribute_load: U256,
}

#[derive(Default)]
struct IssueNodHints {
    verified_cost: Option<U256>,
    gratis_load_minor: Option<U256>,
    series_group_key: Option<alloy_primitives::B256>,
}

pub(crate) fn issue_nod_at(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    issued_at: u64,
) -> Result<WwdEntityId> {
    issue_nod_with_cost(storage, encrypted, issued_at, IssueNodHints::default())
}

pub fn issue_lysis_nod(
    storage: &StorageHandle<'_>,
    verified: &outbe_tee::lysis::VerifiedLysisNod,
) -> Result<WwdEntityId> {
    storage.require_metadosis_process_ready()?;
    issue_nod_with_cost(
        storage,
        verified.nod(),
        storage.now_secs()?,
        IssueNodHints {
            verified_cost: Some(verified.settlement_cost_minor()),
            ..IssueNodHints::default()
        },
    )
}

/// Issues a Nod claimed from a Tribute within the post-Metadosis claim window.
pub fn issue_tribute_nod(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    gratis_load_minor: U256,
) -> Result<WwdEntityId> {
    let cost =
        nod_api::settlement_cost_minor(encrypted.terms.entry_price_minor, gratis_load_minor)?;
    if cost.is_zero() {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    issue_nod_with_cost(
        storage,
        encrypted,
        storage.now_secs()?,
        IssueNodHints {
            verified_cost: Some(cost),
            gratis_load_minor: Some(gratis_load_minor),
            series_group_key: None,
        },
    )
}

/// Issues a Nod claimed from a Tribute belonging to a Lysis-sealed series (`series_group_key`).
pub fn issue_tribute_nod_in_series(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    gratis_load_minor: U256,
    series_group_key: alloy_primitives::B256,
) -> Result<WwdEntityId> {
    let cost =
        nod_api::settlement_cost_minor(encrypted.terms.entry_price_minor, gratis_load_minor)?;
    if cost.is_zero() {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    issue_nod_with_cost(
        storage,
        encrypted,
        storage.now_secs()?,
        IssueNodHints {
            verified_cost: Some(cost),
            gratis_load_minor: Some(gratis_load_minor),
            series_group_key: Some(series_group_key),
        },
    )
}

/// Seals a Lysis series group `(emission_day, issuance_currency, reference_currency)`
/// with its initial `unclaimed_tribute_count` and exact `unclaimed_tribute_load`.
pub fn seal_lysis_series(storage: &StorageHandle<'_>, input: LysisSeriesInput) -> Result<()> {
    let LysisSeriesInput {
        group_key,
        emission_day,
        reference_currency,
        entry_price_minor,
        unclaimed_tribute_count,
        unclaimed_tribute_load,
    } = input;
    let full_day_after_issue = outbe_primitives::time::WorldwideDay::new(
        outbe_primitives::time::next_date_key(emission_day).map_err(date_step_error)?,
    );
    let (entry_price, terms) = match entry_price_minor {
        Some(price) if outbe_nod::pricing::is_issuable_entry(price) => {
            (price, call_terms(storage, price, reference_currency)?)
        }
        _ => (U256::ZERO, None),
    };
    nod_api::seal_lysis_series(
        storage,
        nod_api::LysisSeriesSeal {
            group_key,
            full_day_after_issue,
            reference_currency,
            entry_price_minor: entry_price,
            call_terms: terms,
            unclaimed_tribute_count,
            unclaimed_tribute_load,
        },
    )
}

/// `gratis_load_minor` is read through the enclave when the caller does not know it.
fn issue_nod_with_cost(
    storage: &StorageHandle<'_>,
    encrypted: &EncryptedNodV1,
    issued_at: u64,
    hints: IssueNodHints,
) -> Result<WwdEntityId> {
    let IssueNodHints {
        verified_cost,
        gratis_load_minor,
        series_group_key,
    } = hints;
    let terms = &encrypted.terms;
    if terms.owner.is_zero() {
        return Err(NodFactoryError::InvalidOwner.into());
    }
    let nod_id = outbe_nod::identity::generate_nod_id(terms.owner, terms.worldwide_day)?;
    if terms.nod_id != nod_id
        || terms.chain_id != storage.chain_id()?
        || !encrypted.has_valid_encoding()
    {
        return Err(NodFactoryError::InvalidEncryptedNod.into());
    }
    if nod_api::get_item(storage, nod_id)?.is_some() {
        return Err(NodFactoryError::NodAlreadyExists.into());
    }
    if !outbe_nod::pricing::is_issuable_entry(terms.entry_price_minor) {
        return Err(NodFactoryError::EntryPriceOutOfBounds.into());
    }
    let call_terms = call_terms(storage, terms.entry_price_minor, terms.reference_currency)?;
    let mut call_price_minor = call_terms.map_or(U256::ZERO, |t| t.call_price_minor);
    let group_key = match series_group_key {
        Some(key) => {
            let sealed_call_price = outbe_nod::schema::NodContract::new(storage.clone())
                .group_call_price_minor
                .read(&key)?;
            if !sealed_call_price.is_zero() {
                call_price_minor = sealed_call_price;
            }
            key
        }
        None => {
            outbe_nod::identity::group_key(issued_at, call_price_minor, terms.reference_currency)
                .map_err(date_step_error)?
        }
    };
    let item = NodItemState {
        is_settled: false,
        nod_id,
        owner: terms.owner,
        encrypted: encrypted.clone(),
        worldwide_day: terms.worldwide_day,
        league_id: terms.league_id,
        group_key,
        issuance_currency: terms.issuance_currency,
        reference_currency: terms.reference_currency,
        issued_at,
    };
    let gratis_load_minor = match gratis_load_minor {
        Some(load) => load,
        None => nod_api::calculation_amount(&item)?,
    };
    // Existing settlement pricing remains public in this privacy stage.
    let settlement_cost_minor = match verified_cost {
        Some(cost) => cost,
        None => nod_api::settlement_cost_minor(terms.entry_price_minor, gratis_load_minor)?,
    };
    if let Some(series_key) = series_group_key {
        nod_api::add_series_nod(storage, &item, call_terms, gratis_load_minor, series_key)?;
    } else {
        nod_api::add_nod(storage, &item, call_terms, gratis_load_minor)?;
    }
    emit_event(
        storage,
        INodFactory::NodIssued {
            owner: terms.owner,
            nodId: nod_id.to_u256(),
            worldwideDay: U256::from(u32::from(terms.worldwide_day)),
            leagueId: U256::from(terms.league_id),
            callPriceMinor: call_price_minor,
            encryptedGratisAmount: encrypted.encrypted_gratis_amount.clone().into(),
            entryPriceMinor: terms.entry_price_minor,
            settlementCostMinor: settlement_cost_minor,
        },
    )?;
    Ok(nod_id)
}

/// Mining of one paid Nod. Any caller may submit. The owner's Gratis
/// modify-key MAC/`opNonce` authorizes the mint to that owner.
pub struct MineGratisRequest {
    pub caller: Address,
    pub nod_id: WwdEntityId,
    pub nonce: u64,
    pub auth: outbe_gratisfactory::api::ModifyAuth,
}

/// Pays an uncalled or called Nod's known cost in ERC20 base units of `asset`. An
/// issuance-currency payment must name the VWAP snapshot required at this block.
pub fn settle_nod(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
    payment: SettlementPayment,
) -> Result<()> {
    let SettlementPayment {
        payer,
        asset,
        snapshot_id,
    } = payment;
    let (item, group) = load_nod(storage, nod_id)?;
    if item.body().is_settled {
        return Err(NodFactoryError::NodAlreadySettled.into());
    }
    let deadline = nod_api::settlement_deadline(storage, item.body().group_key)?;
    if deadline != 0 && storage.now_secs()? > deadline {
        return Err(NodFactoryError::SettlementDeadlinePassed.into());
    }
    let owner = item.body().owner;
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &settlement_cost(item.body())?,
    )?;
    quote.require_snapshot(snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // Publish the transition before external payment calls so callbacks cannot
        // settle the same Nod twice. A failed payment rolls the transition back.
        nod_api::record_settled(storage, item, group)?;
        settlement::collect(
            storage,
            NOD_FACTORY_ADDRESS,
            payer,
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
    let cost = nod_cost(entry_price_minor, gratis_load_minor);
    outbe_primitives::settlement::settlement_cost_units(&cost, rate, asset_decimals)
        .map_err(|e| settlement::SettlementError::Rounding(e).into())
}

fn currencies(item: &NodItemState) -> Currencies {
    Currencies {
        issuance: item.issuance_currency,
        reference: item.reference_currency,
    }
}

/// One Nod: its entry price times its Gratis load.
fn settlement_cost(item: &NodItemState) -> Result<SettlementCost> {
    Ok(nod_cost(
        item.entry_price_minor(),
        nod_api::calculation_amount(item)?,
    ))
}

/// One Nod at its entry price and Gratis load, at cost.
fn nod_cost(entry_price_minor: U256, gratis_load_minor: U256) -> SettlementCost {
    SettlementCost {
        entry_price_minor,
        load_minor: gratis_load_minor,
        units: U256::ONE,
    }
}

/// The call terms a group opened at `entry_price_minor` seals: the entry marked up by
/// the live profile's call rate, plus the profile's window, threshold and notice.
///
/// `None` when entry is zero: a zero call price would fire on the first scan. Every
/// later check reads the group's sealed copy, so a retune cannot re-term an issued Nod.
pub(crate) fn call_terms(
    storage: &StorageHandle<'_>,
    entry_price_minor: U256,
    reference_currency: u16,
) -> Result<Option<CallTerms>> {
    if entry_price_minor.is_zero() {
        return Ok(None);
    }
    let params = outbe_nod::config::read(storage)?;
    let call_price = outbe_primitives::markup::marked_up(entry_price_minor, params.call_rate)
        .ok_or(crate::errors::NodFactoryError::CallPriceOverflow)?;
    Ok(Some(CallTerms {
        call_price_minor: call_price,
        reference_currency,
        call_rate: params.call_rate,
        call_window_seconds: params.call_window_seconds,
        call_threshold_seconds: params.call_threshold_seconds,
        call_notice_period_seconds: params.call_notice_period_seconds,
    }))
}

/// Mines a paid entitlement without payment or a deadline. Mint failure
/// rolls back the removal, preserving the paid Nod for retry.
pub fn mine_gratis(storage: &StorageHandle<'_>, request: MineGratisRequest) -> Result<Bytes> {
    let MineGratisRequest {
        nod_id,
        nonce,
        auth,
        ..
    } = request;
    let (item, group) = load_nod(storage, nod_id)?;
    if !item.body().is_settled {
        return Err(NodFactoryError::NodNotSettled.into());
    }
    let owner = item.body().owner;
    validate_pow(nod_id, owner, nonce)?;
    let encrypted = item.body().encrypted.clone();
    storage.clone().with_checkpoint(|| {
        nod_api::remove_nod(storage, item, group)?;
        // The Nod owner authorizes the mint, including when a relayer submits it.
        outbe_gratisfactory::api::mint_encrypted_nod(storage.clone(), &encrypted, auth)?;
        emit_event(
            storage,
            INodFactory::NodMined {
                owner,
                nodId: nod_id.to_u256(),
                encryptedGratisAmount: encrypted.encrypted_gratis_amount.clone().into(),
            },
        )?;
        outbe_gratisfactory::api::encrypted_balance(storage.clone(), owner)
    })
}

fn load_nod(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
) -> Result<(LoadedNodItem, LoadedNodGroup)> {
    let item = nod_api::load_item(storage, nod_id)?.ok_or(NodFactoryError::NodNotFound)?;
    let group = nod_api::load_group(storage, item.body().group_key)?
        .ok_or(NodFactoryError::GroupNotFound)?;
    Ok((item, group))
}

/// What settling `nod_id` with `asset` costs, in which currency, and the VWAP
/// snapshot an issuance-currency payment must name (zero on the reference rail).
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    let (item, _) = load_nod(storage, nod_id)?;
    let quote = settlement::quote(
        storage,
        asset,
        currencies(item.body()),
        &settlement_cost(item.body())?,
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
        pow::SINGLE_MINING_SEQUENCE,
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
        pow::SINGLE_MINING_SEQUENCE,
        nonce,
    )
}

pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(NOD_FACTORY_ADDRESS, event.encode_log_data())
}
