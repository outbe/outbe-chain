//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_common::settlement::floor_to_asset_units;
use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_oracle::api::{settlement_fx_rates, VwapSnapshotId};
use outbe_primitives::addresses::{INTEX_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result, SweepFailure};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::units::{PROTOCOL_AMOUNT_DECIMALS, SCALE_1E6_U256};

use outbe_intex::payout::ContributorLeafData;
use outbe_intex::IntexState;
use outbe_vaultrouter::api::IVaultRouter;

use crate::config;
use crate::constants::{
    INTEX_NFT1155_ADDRESS, MAX_RECIPIENTS_PER_ISSUANCE, MAX_SERIES_PER_MESSAGE,
    ORIGIN_ROUTER_ADDRESS, PRICE_RATE_DEN, PROCEEDS_FANIN_TIMEOUT_SECS, SETTLED_TAG,
};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IReferenceCurrency, IERC1155, IERC20};
use IOriginRouter::IssuanceInstructionsParams;

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Capture series identity in Intex, enroll it in the call-price bin index, and send
/// ISSUANCE_INSTRUCTIONS to every target chain of the day's snapshot. The
/// canonical IntexNFT1155 createSeries now arrives per chain via the ISSUANCE
/// broadcast. This includes a loopback leg on the origin. So there is no
/// in-process NFT call here.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<Vec<IssuanceLeg>> {
    if params.issued_units == 0 {
        // Whether the day distributes is the caller's decision: one empty group
        // must not touch the state its siblings armed.
        return Ok(Vec::new());
    }

    // u32 timestamp. It is bounded until 2106.
    let issued_at = u32::try_from(storage.timestamp()?.to::<u64>())
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;

    let mut factory = IntexFactoryContract::new(storage.clone());
    let cfg = config::read_from(&factory, storage.chain_id()?)?;

    let floor_price_minor = marked_up(params.entry_price_minor, cfg.floor_rate)?;
    let call_price_minor = marked_up(params.entry_price_minor, cfg.call_rate)?;

    let entry_price_minor_u64 = to_wire_price(params.entry_price_minor)?;
    let floor_price_minor_u64 = to_wire_price(floor_price_minor)?;
    let call_price_minor_u64 = to_wire_price(call_price_minor)?;

    let record = outbe_intex::CreateSeriesParams {
        series_id: params.series_id,
        worldwide_day: params.worldwide_day,
        issued_units: params.issued_units,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        floor_price_minor,
        call_price_minor,
        call_trigger: outbe_intex::IntexCallTrigger {
            call_window_seconds: cfg.call_window_seconds,
            call_threshold_seconds: cfg.call_threshold_seconds,
            call_notice_period_seconds: cfg.call_notice_period_seconds,
        },
        issued_at,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
    };
    outbe_intex::api::create_series(storage, record)?;
    factory.widen_call_terms(
        params.reference_currency,
        cfg.call_window_seconds,
        cfg.call_threshold_seconds,
    )?;

    // Not sent here: only the caller sees the whole day, and a chain's share of it
    // travels in as few messages as the caps allow.
    let legs: Vec<IssuanceLeg> = issuance_legs(&params)
        .into_iter()
        .map(|(chain_id, recipients, units)| IssuanceLeg {
            chain_id,
            payload: IOriginRouter::IssuanceInstructionsParams {
                seriesId: params.series_id.into(),
                worldwideDay: params.worldwide_day.into(),
                issuedAt: issued_at,
                issuedUnits: params.issued_units,
                promisLoadMinor: params.promis_load_minor,
                entryPriceMinor: entry_price_minor_u64,
                floorPriceMinor: floor_price_minor_u64,
                callNoticePeriod: cfg.call_notice_period_seconds,
                issuanceCurrency: params.issuance_currency,
                referenceCurrency: params.reference_currency,
                callWindow: cfg.call_window_seconds,
                callThreshold: cfg.call_threshold_seconds,
                callPriceMinor: call_price_minor_u64,
                recipients,
                units,
            },
        })
        .collect();

    // Enroll into the call-price bin index the daily Called scan walks.
    factory.insert_call_bin(
        params.series_id,
        params.reference_currency,
        call_price_minor,
    )?;

    // Arm the creator-reward proceeds fan-in: the winning chains are expected to
    // route proceeds. Creators are paid once all arrive or the deadline passes.
    let deadline = storage
        .timestamp()?
        .to::<u64>()
        .saturating_add(PROCEEDS_FANIN_TIMEOUT_SECS);
    outbe_intex::api::arm_proceeds(
        storage,
        params.worldwide_day,
        &params.recipient_chains,
        deadline,
    )?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesIssued {
            seriesId: params.series_id.into(),
            issuedUnits: params.issued_units,
            entryPriceMinor: params.entry_price_minor,
        },
    )?;

    Ok(legs)
}

/// What one series adds to one chain: the series to create and that chain's winners
/// (empty on a chain with none, which still needs the series for bridging).
#[derive(Clone)]
pub struct IssuanceLeg {
    pub chain_id: u32,
    pub payload: IOriginRouter::IssuanceInstructionsParams,
}

/// Pack a day's legs into per-chain messages, up to `MAX_SERIES_PER_MESSAGE` series and
/// `MAX_RECIPIENTS_PER_ISSUANCE` recipients each. A series with more winners spans several,
/// which the receiver's create-if-absent makes safe.
pub fn pack_issuance_messages(
    legs: Vec<IssuanceLeg>,
) -> Vec<(u32, Vec<IssuanceInstructionsParams>)> {
    let mut per_chain: Vec<(u32, Vec<IssuanceInstructionsParams>)> = Vec::new();
    for leg in legs {
        for slice in split_recipients(leg.payload) {
            // Legs arrive series by series, so this chain's open message is not the last one
            // built. Matching on the tail alone would batch nothing.
            let open = per_chain
                .iter_mut()
                .rev()
                .find(|(chain, _)| *chain == leg.chain_id);
            match open {
                Some((_, message))
                    if message.len() < MAX_SERIES_PER_MESSAGE
                        && recipient_count(message) + slice.recipients.len()
                            <= MAX_RECIPIENTS_PER_ISSUANCE =>
                {
                    message.push(slice);
                }
                _ => per_chain.push((leg.chain_id, vec![slice])),
            }
        }
    }
    per_chain
}

/// Send a day's packed issuance messages, each stamped with its position in the chain's run.
/// Relay-float-funded: value 0. The router quotes and pays the bridge fee from its own float.
pub fn send_issuance(storage: &StorageHandle<'_>, legs: Vec<IssuanceLeg>) -> Result<()> {
    for ((chain_id, worldwide_day), messages) in
        chunk_issuance_messages(pack_issuance_messages(legs))
    {
        let total_chunks = u16::try_from(messages.len())
            .map_err(|_| PrecompileError::Revert("issuance chunk count exceeds u16".into()))?;
        for (chunk_index, series) in messages.into_iter().enumerate() {
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendIssuanceInstructionsCall {
                    dstChainId: chain_id,
                    worldwideDay: worldwide_day,
                    chunkIndex: chunk_index as u16,
                    totalChunks: total_chunks,
                    series,
                }
                .abi_encode()
                .into(),
            )?;
        }
    }
    Ok(())
}

/// One (chain, worldwide day) run of issuance messages, in send order: what the chunk header
/// numbers.
pub(crate) type IssuanceRun = ((u32, u32), Vec<Vec<IssuanceInstructionsParams>>);

/// Group packed messages into the runs the chunk header numbers: one per (chain, day), the pair
/// the receiver counts chunks against. Callers pass one day's legs (clearing does). This is also
/// what keeps `pack_issuance_messages` from batching two days into one message.
pub(crate) fn chunk_issuance_messages(
    packed: Vec<(u32, Vec<IssuanceInstructionsParams>)>,
) -> Vec<IssuanceRun> {
    let mut runs: Vec<IssuanceRun> = Vec::new();
    for (chain_id, message) in packed {
        let key = (chain_id, message[0].worldwideDay);
        match runs.iter_mut().find(|(run_key, _)| *run_key == key) {
            Some((_, messages)) => messages.push(message),
            None => runs.push((key, vec![message])),
        }
    }
    runs
}

fn recipient_count(message: &[IssuanceInstructionsParams]) -> usize {
    message.iter().map(|item| item.recipients.len()).sum()
}

/// One series' instructions cut into pieces a message can carry. Only the winners differ.
fn split_recipients(payload: IssuanceInstructionsParams) -> Vec<IssuanceInstructionsParams> {
    if payload.recipients.len() <= MAX_RECIPIENTS_PER_ISSUANCE {
        return vec![payload];
    }
    (0..payload.recipients.len())
        .step_by(MAX_RECIPIENTS_PER_ISSUANCE)
        .map(|start| {
            let end = (start + MAX_RECIPIENTS_PER_ISSUANCE).min(payload.recipients.len());
            IssuanceInstructionsParams {
                recipients: payload.recipients[start..end].to_vec(),
                units: payload.units[start..end].to_vec(),
                ..payload.clone()
            }
        })
        .collect()
}

/// One `(chain, recipients, units)` issuance leg per snapshot chain. Winners land on their
/// own chain, and every other chain gets an empty leg. So the series is created there too
/// (needed for user NFT bridging).
pub(crate) fn issuance_legs(params: &IssuanceParams) -> Vec<(u32, Vec<Address>, Vec<U256>)> {
    params
        .snapshot_chains
        .iter()
        .map(|&chain_id| {
            let mut recipients = Vec::new();
            let mut units = Vec::new();
            for (i, &c) in params.recipient_chains.iter().enumerate() {
                if c == chain_id {
                    recipients.push(params.recipients[i]);
                    units.push(params.units[i]);
                }
            }
            (chain_id, recipients, units)
        })
        .collect()
}

/// Narrows a six-decimal COEN/ISO price to the wire's existing `u64` shape.
pub fn to_wire_price(price_minor: U256) -> Result<u64> {
    u64::try_from(price_minor)
        .map_err(|_| PrecompileError::Revert("price exceeds the wire type".into()))
}

/// Applies a markup rate in percentage points: `entry * (100 + rate) / 100`.
pub fn marked_up(entry_price: U256, rate: u16) -> Result<U256> {
    entry_price
        .checked_mul(U256::from(PRICE_RATE_DEN + rate))
        .map(|v| v / U256::from(PRICE_RATE_DEN))
        .ok_or_else(|| PrecompileError::Revert("marked-up price overflow".into()))
}

/// Decimals a price x PROMIS load product carries: both protocol factors stay
/// on the six-decimal scale independently of native COEN denomination.
const PRODUCT_DECIMALS: u32 = 2 * PROTOCOL_AMOUNT_DECIMALS as u32;

/// Disjoint unit counts of a series, for a reader that must not redo the arithmetic.
pub(crate) fn series_unit_counts(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
) -> Result<crate::precompile::IIntexFactory::UnitCounts> {
    let counts = outbe_intex::api::unit_counts(storage, series_id)?;
    Ok(crate::precompile::IIntexFactory::UnitCounts {
        issuedUnits: counts.issued,
        activeUnits: counts.active,
        settledUnits: counts.settled,
        exercisedUnits: counts.exercised,
        gemFactoryUnits: counts.gem_factory,
        forfeitedUnits: counts.forfeited,
    })
}

/// One owner's units of `series_id`: its current balances, Issued ones only until the
/// series expires, and its own exercise and Gem Factory history.
pub(crate) fn owner_balances(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
) -> Result<crate::precompile::IIntexFactory::OwnerBalances> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    let now = storage.timestamp()?.to::<u64>();
    let issued = if series.effective_state(now)? == IntexState::Expired {
        0
    } else {
        nft_units_of(storage, owner, issued_token_id(series_id))?
    };
    let settled = nft_units_of(storage, owner, settled_token_id(series_id))?;
    Ok(crate::precompile::IIntexFactory::OwnerBalances {
        issuedUnits: issued,
        settledUnits: settled,
        exercisedUnits: outbe_intex::api::owner_exercised_units(storage, series_id, owner)?,
        gemFactoryUnits: outbe_intex::api::owner_gem_factory_units(storage, series_id, owner)?,
        ownerUnits: issued
            .checked_add(settled)
            .ok_or_else(|| PrecompileError::Revert("owner units exceed u32".into()))?,
    })
}

/// Settle paying the cost from `settler` in `asset` by ERC20 transfer. The settled
/// units stay with `owner`. An issuance-currency payment must name the VWAP
/// snapshot required at this block.
pub fn settle_intex(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    owner: Address,
    settler: Address,
    units: U256,
    asset: Address,
    snapshot_id: U256,
) -> Result<()> {
    if owner.is_zero() || settler.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if units.is_zero() {
        return Err(IntexFactoryError::ZeroUnits.into());
    }

    let series = outbe_intex::api::read_series(storage, series_id)?;
    // The call check is one read; the qualification walk goes last.
    match series.lifecycle_state()? {
        IntexState::Called => {
            let now = storage.timestamp()?.to::<u64>();
            let deadline =
                u64::from(series.called_at) + u64::from(series.call_notice_period_seconds);
            if now > deadline {
                return Err(IntexFactoryError::DeadlineExpired.into());
            }
        }
        IntexState::Issued if is_qualified(storage, &series)? => {}
        _ => return Err(IntexFactoryError::NotSettleable(series.state).into()),
    }

    let balance = nft_balance_of(storage, owner, issued_token_id(series_id))?;
    if balance.is_zero() {
        return Err(IntexFactoryError::ZeroBalance.into());
    }
    if units > balance {
        return Err(IntexFactoryError::UnitsExceedBalance.into());
    }

    let currency = accept_payment_asset(storage, asset, &series)?;
    let (cost, snapshot) = cost_in_asset(storage, &series, asset, currency, units)?;
    require_snapshot(snapshot, snapshot_id)?;
    storage.clone().with_checkpoint(|| {
        // The units move before payment so a token callback cannot settle them
        // twice. A failed payment rolls the move back.
        storage.call(
            INTEX_NFT1155_ADDRESS,
            U256::ZERO,
            IIntexNFT1155::settleIntexCall {
                seriesId: series_id.into(),
                owner,
                units,
            }
            .abi_encode()
            .into(),
        )?;

        let settled_units = u32::try_from(units)
            .map_err(|_| PrecompileError::Revert("settled units exceed u32".into()))?;
        outbe_intex::api::record_settled_units(storage, series_id, settled_units)?;

        deposit_payment(storage, settler, asset, cost)?;

        emit_event(
            storage,
            crate::precompile::IIntexFactory::Settled {
                seriesId: series_id.into(),
                owner,
                units,
            },
        )
    })
}

/// Rejects an issuance-rail payment authorized for any snapshot but the required one.
fn require_snapshot(required: Option<VwapSnapshotId>, authorized: U256) -> Result<()> {
    match required.map(VwapSnapshotId::to_u256) {
        Some(required) if required != authorized => Err(IntexFactoryError::VwapSnapshotMismatch {
            authorized,
            required,
        }
        .into()),
        _ => Ok(()),
    }
}

/// Pulls exactly `cost` of `asset` from `payer` and deposits it into the reserve
/// vault through the router, leaving the factory's own balance untouched.
fn deposit_payment(
    storage: &StorageHandle<'_>,
    payer: Address,
    asset: Address,
    cost: U256,
) -> Result<()> {
    if cost.is_zero() {
        return Ok(());
    }
    let before = token_balance(storage, asset)?;
    checked_token_call(
        storage,
        asset,
        IERC20::transferFromCall {
            from: payer,
            to: INTEX_FACTORY_ADDRESS,
            amount: cost,
        },
    )?;
    if token_balance(storage, asset)?.checked_sub(before) != Some(cost) {
        return Err(IntexFactoryError::SettlementAmountMismatch.into());
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
        return Err(IntexFactoryError::SettlementAmountMismatch.into());
    }
    Ok(())
}

fn checked_token_call(
    storage: &StorageHandle<'_>,
    asset: Address,
    call: impl SolCall,
) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(IntexFactoryError::TokenOperationFailed.into());
    }
    Ok(())
}

fn token_balance(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        asset,
        IERC20::balanceOfCall {
            account: INTEX_FACTORY_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| IntexFactoryError::TokenOperationFailed.into())
}

// --- storage.call helpers (localnet-exercised) ---

fn nft_units_of(storage: &StorageHandle<'_>, account: Address, id: U256) -> Result<u32> {
    u32::try_from(nft_balance_of(storage, account, id)?)
        .map_err(|_| PrecompileError::Revert("NFT balance exceeds u32".into()))
}

fn nft_balance_of(storage: &StorageHandle<'_>, account: Address, id: U256) -> Result<U256> {
    let ret = storage.staticcall(
        INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall { account, id }.abi_encode().into(),
    )?;
    IERC1155::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("NFT balanceOf undecodable".into()))
}

mod mining;
mod pricing;
mod proceeds;

pub use mining::mine_promis;
#[cfg(test)]
pub(crate) use mining::{compute_pow_hash, validate_pow};
pub(crate) use mining::{issued_token_id, settled_token_id};
#[cfg(test)]
pub(crate) use pricing::settlement_units;
use pricing::{accept_payment_asset, cost_in_asset};
pub use pricing::{is_qualified, is_series_qualified, quote_settlement};
pub use proceeds::distribute;
#[cfg(test)]
pub(crate) use proceeds::try_settle_proceeds;
pub(crate) use proceeds::{
    contributor_payout_round, pay_contributor_batch, sweep_proceeds_deadlines,
};
