//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{keccak256, Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_common::WorldwideDay;
use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_primitives::addresses::{INTEX_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_intex::IntexState;
use outbe_vaultrouter::api::IVaultRouter;

use crate::config;
use crate::constants::{
    DIST_CHUNK_LIMIT, FX_RATE_MAX_AGE_SECONDS, INTEX_NFT1155_ADDRESS, MAX_RECIPIENTS_PER_MESSAGE,
    MAX_SERIES_PER_MESSAGE, ORACLE_TO_WIRE_SCALE, ORIGIN_ROUTER_ADDRESS, POW_DIFFICULTY,
    PRICE_RATE_DEN, PROCEEDS_FANIN_TIMEOUT_SECS,
};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IReferenceCurrency, IERC1155, IERC20};
use IOriginRouter::IssuanceInstructionsParams;

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Capture series identity in Intex, enroll it in the floor-bin index, and send
/// ISSUANCE_INSTRUCTIONS to every target chain of the day's snapshot. The
/// canonical IntexNFT1155 createSeries now arrives per chain via the ISSUANCE
/// broadcast (including a loopback leg on the origin), so there is no in-process
/// NFT call here.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<Vec<IssuanceLeg>> {
    if params.issued_intex_count == 0 {
        // Nothing to issue. Whether the day as a whole distributes is the
        // caller's decision — a day may issue several series, and one empty
        // group must not touch the state its siblings armed.
        return Ok(Vec::new());
    }

    // u32 timestamp; bounded until 2106.
    let issued_at = u32::try_from(storage.timestamp()?.to::<u64>())
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;

    let mut factory = IntexFactoryContract::new(storage.clone());
    let cfg = config::read(&factory)?;

    let floor_price_minor = marked_up(params.entry_price_minor, cfg.floor_rate)?;
    let call_price_minor = marked_up(params.entry_price_minor, cfg.call_rate)?;

    let entry_price_minor_u64 = to_wire_price(params.entry_price_minor)?;
    let floor_price_minor_u64 = to_wire_price(floor_price_minor)?;
    let call_price_minor_u64 = to_wire_price(call_price_minor)?;

    let record = outbe_intex::CreateSeriesParams {
        series_id: params.series_id,
        worldwide_day: params.worldwide_day,
        issued_intex_count: params.issued_intex_count,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        floor_price_minor,
        call_price_minor,
        call_trigger: outbe_intex::IntexCallTrigger {
            call_window: cfg.call_window,
            call_threshold: cfg.call_threshold,
            call_notice_period: cfg.call_notice_period,
        },
        issued_at,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
    };
    outbe_intex::api::create_series(storage, record)?;

    // What each snapshot chain must be told. Not sent here: only the caller sees the whole
    // day, and a chain's share of it travels in as few messages as the caps allow.
    let legs: Vec<IssuanceLeg> = issuance_legs(&params)
        .into_iter()
        .map(|(chain_id, recipients, quantities)| IssuanceLeg {
            chain_id,
            payload: IOriginRouter::IssuanceInstructionsParams {
                seriesId: params.series_id.into(),
                worldwideDay: params.worldwide_day.into(),
                issuedIntexCount: params.issued_intex_count,
                promisLoadMinor: params.promis_load_minor,
                entryPriceMinor: entry_price_minor_u64,
                floorPriceMinor: floor_price_minor_u64,
                callNoticePeriod: cfg.call_notice_period,
                issuanceCurrency: params.issuance_currency,
                referenceCurrency: params.reference_currency,
                callWindow: cfg.call_window,
                callThreshold: cfg.call_threshold,
                callPriceMinor: call_price_minor_u64,
                recipients,
                quantities,
            },
        })
        .collect();

    // Enroll into the unqualified floor-bin index for begin_block qualify.
    factory.insert_unqualified(
        params.series_id,
        params.reference_currency,
        floor_price_minor,
    )?;

    // Arm the creator-reward proceeds fan-in: the winning chains are expected to
    // route proceeds; creators are paid once all arrive or the deadline passes.
    let deadline = storage
        .timestamp()?
        .to::<u64>()
        .saturating_add(PROCEEDS_FANIN_TIMEOUT_SECS);
    outbe_intex::api::arm_proceeds(
        storage,
        params.worldwide_day.value(),
        &params.recipient_chains,
        deadline,
    )?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesIssued {
            seriesId: params.series_id.into(),
            issuedIntexCount: params.issued_intex_count,
            entryPrice: params.entry_price_minor,
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
/// `MAX_RECIPIENTS_PER_MESSAGE` recipients each. A series with more winners spans several,
/// which the receiver's create-if-absent makes safe.
pub fn pack_issuance_messages(
    legs: Vec<IssuanceLeg>,
) -> Vec<(u32, Vec<IssuanceInstructionsParams>)> {
    let mut per_chain: Vec<(u32, Vec<IssuanceInstructionsParams>)> = Vec::new();
    for leg in legs {
        for slice in split_recipients(leg.payload) {
            // Legs arrive series by series, so this chain's open message is not the last one
            // built; matching on the tail alone would batch nothing.
            let open = per_chain
                .iter_mut()
                .rev()
                .find(|(chain, _)| *chain == leg.chain_id);
            match open {
                Some((_, message))
                    if message.len() < MAX_SERIES_PER_MESSAGE
                        && recipient_count(message) + slice.recipients.len()
                            <= MAX_RECIPIENTS_PER_MESSAGE =>
                {
                    message.push(slice);
                }
                _ => per_chain.push((leg.chain_id, vec![slice])),
            }
        }
    }
    per_chain
}

/// Send a day's packed issuance messages. Relay-float-funded: value 0, the router
/// quotes and pays the bridge fee from its own float.
pub fn send_issuance(storage: &StorageHandle<'_>, legs: Vec<IssuanceLeg>) -> Result<()> {
    for (chain_id, series) in pack_issuance_messages(legs) {
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendIssuanceInstructionsCall {
                dstChainId: chain_id,
                series,
            }
            .abi_encode()
            .into(),
        )?;
    }
    Ok(())
}

fn recipient_count(message: &[IssuanceInstructionsParams]) -> usize {
    message.iter().map(|item| item.recipients.len()).sum()
}

/// One series' instructions cut into pieces a message can carry; only the winners differ.
fn split_recipients(payload: IssuanceInstructionsParams) -> Vec<IssuanceInstructionsParams> {
    if payload.recipients.len() <= MAX_RECIPIENTS_PER_MESSAGE {
        return vec![payload];
    }
    (0..payload.recipients.len())
        .step_by(MAX_RECIPIENTS_PER_MESSAGE)
        .map(|start| {
            let end = (start + MAX_RECIPIENTS_PER_MESSAGE).min(payload.recipients.len());
            IssuanceInstructionsParams {
                recipients: payload.recipients[start..end].to_vec(),
                quantities: payload.quantities[start..end].to_vec(),
                ..payload.clone()
            }
        })
        .collect()
}

/// One `(chain, recipients, quantities)` issuance leg per snapshot chain: winners land on their
/// own chain and every other chain gets an empty leg, so the series is created there too (needed
/// for user NFT bridging).
pub(crate) fn issuance_legs(params: &IssuanceParams) -> Vec<(u32, Vec<Address>, Vec<U256>)> {
    params
        .snapshot_chains
        .iter()
        .map(|&chain_id| {
            let mut recipients = Vec::new();
            let mut quantities = Vec::new();
            for (i, &c) in params.recipient_chains.iter().enumerate() {
                if c == chain_id {
                    recipients.push(params.recipients[i]);
                    quantities.push(params.quantities[i]);
                }
            }
            (chain_id, recipients, quantities)
        })
        .collect()
}

/// Narrows an oracle-scale price for the wire's `u64` on the 1e9 scale. At 1e18 the type
/// capped prices near 18.44, which stopped the auction once COEN passed roughly 8.
pub fn to_wire_price(price_minor: U256) -> Result<u64> {
    u64::try_from(price_minor / U256::from(ORACLE_TO_WIRE_SCALE))
        .map_err(|_| PrecompileError::Revert("price exceeds the wire scale".into()))
}

/// Applies a markup rate in percentage points: `entry * (100 + rate) / 100`.
pub fn marked_up(entry_price: U256, rate: u16) -> Result<U256> {
    entry_price
        .checked_mul(U256::from(PRICE_RATE_DEN + rate))
        .map(|v| v / U256::from(PRICE_RATE_DEN))
        .ok_or_else(|| PrecompileError::Revert("marked-up price overflow".into()))
}

/// Per-Intex cost in the payment token's minor units; `entry_price` and `promis_load_minor`
/// are both 1e18-scaled, so their product carries 1e36. Rounded up, as the issuance route is.
pub(crate) fn derived_cost_amount(
    entry_price: U256,
    promis_load_minor: U256,
    payment_decimals: u8,
) -> Result<U256> {
    let exp = 36u32.checked_sub(u32::from(payment_decimals)).ok_or(
        IntexFactoryError::UnsupportedPaymentDecimals(payment_decimals),
    )?;
    entry_price
        .checked_mul(promis_load_minor)
        .map(|v| v.div_ceil(U256::from(10u64).pow(U256::from(exp))))
        .ok_or_else(|| PrecompileError::Revert("cost amount overflow".into()))
}

/// Set the dual-wallet authorized settler for `holder`'s position in `series_id`.
/// `holder` is the caller (the precompile passes its caller).
pub fn set_authorized_settler(
    storage: &StorageHandle<'_>,
    holder: Address,
    series_id: SeriesId,
    settler: Address,
) -> Result<()> {
    if holder.is_zero() || settler.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    let mut factory = IntexFactoryContract::new(storage.clone());
    factory.write_authorized_settler(holder, series_id, settler)
}

/// Credit auction proceeds (native COEN, arriving as `amount` = msg.value) from
/// one target chain into the day's pot. Gated to the OriginRouter. Creators are
/// paid once every winning chain has routed its proceeds (or the fan-in deadline
/// passes); the payout itself runs in the begin-block drain. Because proceeds
/// arrive once per winning chain (loopback same-block, remote minutes later),
/// the credit only accumulates — it never reverts on a repeat or ownerless day,
/// which would strand that chain's delivery.
pub fn distribute(
    storage: &StorageHandle<'_>,
    caller: Address,
    worldwide_day: WorldwideDay,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    outbe_intex::api::credit_proceeds(storage, worldwide_day.value(), src_chain_id, amount)?;
    let now = storage.timestamp()?.to::<u64>();
    try_settle_proceeds(storage, worldwide_day.value(), now)
}

/// Start a distribution round for a series if its proceeds fan-in is satisfied
/// (all winning chains in) or its deadline has passed. Idempotent: it no-ops
/// while a round is still draining, so repeated arrivals and the begin-block
/// sweep can both call it safely.
pub(crate) fn try_settle_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    now: u64,
) -> Result<()> {
    // Never overlap a round that is still paying out.
    if outbe_intex::api::get_progress(storage, worldwide_day)?.is_some() {
        return Ok(());
    }
    let deadline = outbe_intex::api::proceeds_deadline(storage, worldwide_day)?;
    if deadline == 0 {
        return Ok(()); // never armed (no issuance for this series)
    }
    let complete = outbe_intex::api::proceeds_ready(storage, worldwide_day)?;
    if !complete && now < deadline {
        return Ok(()); // keep waiting for the remaining chains
    }

    let pot = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
    if pot.is_zero() {
        // Nothing new to pay. Once every chain is in, finalize (clears the map);
        // a forced empty round just idles until a late arrival tops the pot up.
        if complete {
            outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
        }
        return Ok(());
    }

    let total = outbe_intex::api::contributor_total(storage, worldwide_day)?;
    if total.is_zero() {
        // Ownerless proceeds: burn instead of stranding them.
        burn_ownerless_proceeds(storage, worldwide_day, pot)?;
        if complete {
            outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
        }
        return Ok(());
    }

    // Finalize on completion only when every winning chain is in; otherwise the
    // deadline forced a partial payout and the map is retained for a top-up.
    outbe_intex::api::set_proceeds_finalize_on_done(storage, worldwide_day, complete)?;
    outbe_intex::api::start_distribution(storage, worldwide_day, pot, total)
}

/// Begin-block sweep: settle every series whose proceeds fan-in deadline has
/// passed. Each series runs in its own checkpoint so one failure is retried next
/// block instead of halting the block.
pub(crate) fn sweep_proceeds_deadlines(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = outbe_intex::api::awaiting_proceeds_count(storage)?;
    let mut worldwide_days = Vec::with_capacity(count as usize);
    for i in 0..count {
        worldwide_days.push(outbe_intex::api::awaiting_proceeds_at(storage, i)?);
    }
    for worldwide_day in worldwide_days {
        let res = storage.with_checkpoint(|| try_settle_proceeds(storage, worldwide_day, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::intexfactory", worldwide_day, error = ?e, "proceeds sweep: skipping series");
        }
    }
    Ok(())
}

/// Burn the ownerless proceeds of a series with no recorded contributors:
/// destroy the native COEN held by the factory, reducing total supply.
fn burn_ownerless_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    amount: U256,
) -> Result<()> {
    storage.decrease_balance(INTEX_FACTORY_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsBurned {
            worldwideDay: worldwide_day,
            amount,
        },
    )
}

/// Pay up to `limit` contributors of an in-flight distribution, advancing the
/// cursor. The last contributor absorbs the integer-division remainder so the
/// full `amount` is paid out exactly. On reaching the last contributor the
/// distribution is finalized (progress + contributor map cleared). Driven by
/// the begin-block drain.
pub(crate) fn pay_chunk(storage: &StorageHandle<'_>, worldwide_day: u32, limit: u32) -> Result<()> {
    let mut progress = outbe_intex::api::get_progress(storage, worldwide_day)?
        .ok_or(IntexFactoryError::NoDistribution(worldwide_day))?;
    let count = outbe_intex::api::contributor_count(storage, worldwide_day)?;
    let end = progress.cursor.saturating_add(limit).min(count);

    // A zero denominator would panic on divide; begin-block panics halt the chain (not checkpoint-isolated),
    // so fail as an isolated Err instead.
    if progress.total_nominal.is_zero() {
        return Err(IntexFactoryError::NoContributors(worldwide_day).into());
    }

    let mut paid = progress.paid_so_far;
    for i in progress.cursor..end {
        let (owner, nominal) = outbe_intex::api::contributor_at(storage, worldwide_day, i)?;
        // The final contributor absorbs the rounding remainder so the sum of
        // payouts equals `amount` exactly. checked_mul: isolated Err over a silent wrap.
        let share = if i == count - 1 {
            progress.amount - paid
        } else {
            progress
                .amount
                .checked_mul(nominal)
                .ok_or(IntexFactoryError::DistributionOverflow(worldwide_day))?
                / progress.total_nominal
        };
        storage.transfer_balance(INTEX_FACTORY_ADDRESS, owner, share)?;
        paid += share;
    }

    if end == count {
        // End this round (progress + active-set entry). Whether the contributor
        // map is also cleared depends on the fan-in: finalize when every winning
        // chain is in, otherwise retain the map for a late top-up.
        outbe_intex::api::finish_distribution_round(storage, worldwide_day)?;
        emit_event(
            storage,
            crate::precompile::IIntexFactory::ProceedsDistributed {
                worldwideDay: worldwide_day,
                amount: progress.amount,
                contributors: count,
            },
        )?;
        if outbe_intex::api::proceeds_finalize_on_done(storage, worldwide_day)? {
            // A straggler (or a chain sending its proceeds in parts) can top the
            // pot up while this final round drains. finalize clears the map, so
            // pay any such top-up over it first and finalize only once the pot is
            // empty — otherwise the top-up is later burned as ownerless.
            let pot = outbe_intex::api::take_proceeds_pot(storage, worldwide_day)?;
            if pot.is_zero() {
                outbe_intex::api::finalize_proceeds(storage, worldwide_day)?;
            } else {
                let total = outbe_intex::api::contributor_total(storage, worldwide_day)?;
                outbe_intex::api::start_distribution(storage, worldwide_day, pot, total)?;
            }
        }
    } else {
        progress.cursor = end;
        progress.paid_so_far = paid;
        outbe_intex::api::save_progress(storage, &progress)?;
    }
    Ok(())
}

/// Begin-block drain: advance every in-flight distribution by one chunk
/// (`DIST_CHUNK_LIMIT` payouts). Completed distributions remove themselves from
/// the active set inside `pay_chunk`, so the snapshot avoids iterating a set
/// that mutates underneath us.
pub(crate) fn drain_distributions(storage: &StorageHandle<'_>) -> Result<()> {
    let count = outbe_intex::api::active_dist_count(storage)?;
    let mut worldwide_days = Vec::with_capacity(count as usize);
    for i in 0..count {
        worldwide_days.push(outbe_intex::api::active_dist_at(storage, i)?);
    }
    for worldwide_day in worldwide_days {
        // Per-series isolation: Err reverts the series' checkpoint, retried next block.
        let res = storage.with_checkpoint(|| pay_chunk(storage, worldwide_day, DIST_CHUNK_LIMIT));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::intexfactory", worldwide_day, error = ?e, "distribution drain: skipping series");
        }
    }
    Ok(())
}

/// Settle: `settler` is the caller. Gating reads Intex; value movement
/// (token / vault / NFT) goes via storage.call.
pub fn settle(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    intex_holder: Address,
    settler: Address,
    amount: U256,
    payment_token: Address,
) -> Result<()> {
    if intex_holder.is_zero() || settler.is_zero() || payment_token.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }

    let series = outbe_intex::api::read_series(storage, series_id)?;
    let state = series.lifecycle_state()?;
    // Settle is allowed in Qualified (voluntary) and Called (forced).
    if state != IntexState::Qualified && state != IntexState::Called {
        return Err(IntexFactoryError::NotSettleable(series.state).into());
    }
    // The deadline only constrains forced settlement (Called).
    if state == IntexState::Called {
        let now = storage.timestamp()?.to::<u64>();
        let deadline = u64::from(series.called_at) + u64::from(series.call_notice_period);
        if now > deadline {
            return Err(IntexFactoryError::DeadlineExpired.into());
        }
    }

    // Issued balance (NFT). Issued token id = uint256(seriesId).
    let issued_token_id = U256::from_be_slice(series_id.as_bytes());
    let balance = nft_balance_of(storage, intex_holder, issued_token_id)?;
    if balance.is_zero() {
        return Err(IntexFactoryError::ZeroBalance.into());
    }
    if amount > balance {
        return Err(IntexFactoryError::AmountExceedsBalance.into());
    }

    // Dual-wallet authorization: only the holder or its authorized settler.
    let mut factory = IntexFactoryContract::new(storage.clone());
    if intex_holder != settler
        && factory.read_authorized_settler(intex_holder, series_id)? != settler
    {
        return Err(IntexFactoryError::NotAuthorized.into());
    }

    let currency = accept_payment_token(storage, payment_token, &series)?;

    let payment = cost_in_token(storage, &series, payment_token, currency)?
        .checked_mul(amount)
        .ok_or_else(|| PrecompileError::Revert("settlement cost overflow".into()))?;

    // Pull payment from the settler, deposit into the reserve vault.
    // Fee-on-transfer safe: measure the received delta.
    let before = erc20_balance_of(storage, payment_token, INTEX_FACTORY_ADDRESS)?;
    storage.call(
        payment_token,
        U256::ZERO,
        IERC20::transferFromCall {
            from: settler,
            to: INTEX_FACTORY_ADDRESS,
            amount: payment,
        }
        .abi_encode()
        .into(),
    )?;
    let after = erc20_balance_of(storage, payment_token, INTEX_FACTORY_ADDRESS)?;
    let received = after
        .checked_sub(before)
        .ok_or_else(|| PrecompileError::Revert("payment balance underflow".into()))?;

    storage.call(
        payment_token,
        U256::ZERO,
        IERC20::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: received,
        }
        .abi_encode()
        .into(),
    )?;

    // Deposit into the reserve vault via the router's Solidity ABI.
    let shares = outbe_vaultrouter::api::deposit(storage, payment_token, received)?;
    if shares.is_zero() {
        return Err(IntexFactoryError::ZeroSharesReceived.into());
    }

    // Burn Issued from holder, mint Settled to the settler.
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::settleCall {
            seriesId: series_id.into(),
            from: intex_holder,
            to: settler,
            amount,
        }
        .abi_encode()
        .into(),
    )?;

    factory.bump_settle_count(series_id)?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::Settled {
            seriesId: series_id.into(),
            intexHolder: intex_holder,
            settler,
            amount,
        },
    )
}

// --- storage.call helpers (localnet-exercised) ---

fn nft_balance_of(storage: &StorageHandle<'_>, account: Address, id: U256) -> Result<U256> {
    let ret = storage.staticcall(
        INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall { account, id }.abi_encode().into(),
    )?;
    IERC1155::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("NFT balanceOf undecodable".into()))
}

/// Per-Intex cost of settling `series_id` in `payment_token`, in that token's
/// minor units. Rejects a token the series does not accept.
pub fn quote_cost_amount(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    payment_token: Address,
) -> Result<U256> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    let currency = accept_payment_token(storage, payment_token, &series)?;
    cost_in_token(storage, &series, payment_token, currency)
}

/// Which of the series' two currencies a payment token is denominated in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PaymentCurrency {
    Reference,
    Issuance,
}

/// Per-Intex cost in `token`'s minor units. The reference currency needs no rate; the
/// issuance currency converts through COEN — `cost_ref * rate(COEN/iss) / rate(COEN/ref)`
/// — with the decimals scaling folded into the same division, so it rounds once.
fn cost_in_token(
    storage: &StorageHandle<'_>,
    series: &outbe_intex::SeriesRecord,
    token: Address,
    currency: PaymentCurrency,
) -> Result<U256> {
    let payment_decimals = erc20_decimals(storage, token)?;
    if currency == PaymentCurrency::Reference {
        return derived_cost_amount(
            series.entry_price_minor,
            series.promis_load_minor,
            payment_decimals,
        );
    }

    // entry_price and promis_load are both 1e18-scaled, so their product carries 1e36.
    let scaled = series
        .entry_price_minor
        .checked_mul(series.promis_load_minor)
        .ok_or_else(|| PrecompileError::Revert("cost amount overflow".into()))?;
    let exp = 36u32.checked_sub(u32::from(payment_decimals)).ok_or(
        IntexFactoryError::UnsupportedPaymentDecimals(payment_decimals),
    )?;

    let now = storage.timestamp()?.to::<u64>();
    let rate_issuance = fresh_coen_rate(storage, series.issuance_currency, now)?;
    let rate_reference = fresh_coen_rate(storage, series.reference_currency, now)?;
    let numerator = scaled
        .checked_mul(rate_issuance)
        .ok_or_else(|| PrecompileError::Revert("settlement fx overflow".into()))?;
    let denominator = U256::from(10u64)
        .pow(U256::from(exp))
        .checked_mul(rate_reference)
        .ok_or_else(|| PrecompileError::Revert("settlement fx overflow".into()))?;
    Ok(numerator.div_ceil(denominator))
}

/// The oracle's COEN price in `iso_code`, refused when absent or older than
/// [`FX_RATE_MAX_AGE_SECONDS`].
fn fresh_coen_rate(storage: &StorageHandle<'_>, iso_code: u16, now: u64) -> Result<U256> {
    // A missing pair is an answer; a failed read is not, and must not look like one.
    let Some(pair_index) = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)?
    else {
        return Err(IntexFactoryError::FxRateUnavailable(iso_code).into());
    };
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let rate = oracle.exchange_rate.read(&pair_index)?;
    if rate.is_zero() {
        return Err(IntexFactoryError::FxRateUnavailable(iso_code).into());
    }
    let published = oracle.exchange_rate_timestamp.read(&pair_index)?;
    if now.saturating_sub(published) > FX_RATE_MAX_AGE_SECONDS {
        return Err(IntexFactoryError::FxRateStale(iso_code).into());
    }
    Ok(rate)
}

/// Rejects `token` unless the router holds a vault for it and the token reports
/// one of the series' two currencies; returns which one. Registration is checked
/// first: an unregistered token need not implement `isoCode()` at all.
fn accept_payment_token(
    storage: &StorageHandle<'_>,
    token: Address,
    series: &outbe_intex::SeriesRecord,
) -> Result<PaymentCurrency> {
    let ret = storage.staticcall(
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::assetVaultsCountCall { asset: token }
            .abi_encode()
            .into(),
    )?;
    let vaults = IVaultRouter::assetVaultsCountCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("assetVaultsCount undecodable".into()))?;
    if vaults.is_zero() {
        return Err(IntexFactoryError::PaymentTokenNotRegistered(token).into());
    }

    let iso = asset_iso_code(storage, token)?;
    if iso == series.reference_currency {
        return Ok(PaymentCurrency::Reference);
    }
    if iso == series.issuance_currency {
        return Ok(PaymentCurrency::Issuance);
    }
    Err(IntexFactoryError::SettlementCurrencyMismatch(iso).into())
}

fn asset_iso_code(storage: &StorageHandle<'_>, token: Address) -> Result<u16> {
    let ret = storage.staticcall(
        token,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("isoCode undecodable".into()))
}

fn erc20_balance_of(storage: &StorageHandle<'_>, token: Address, account: Address) -> Result<U256> {
    let ret = storage.staticcall(token, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("ERC20 balanceOf undecodable".into()))
}

fn erc20_decimals(storage: &StorageHandle<'_>, token: Address) -> Result<u8> {
    let ret = storage.staticcall(token, IERC20::decimalsCall {}.abi_encode().into())?;
    IERC20::decimalsCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("ERC20 decimals undecodable".into()))
}

/// minePromis: PoW-gated burn of Settled then mint of Promis. `holder` is the
/// caller.
pub fn mine_promis(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    holder: Address,
    amount: U256,
    nonce: U256,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    if holder.is_zero() {
        return Err(IntexFactoryError::ZeroAddress.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }

    let series = outbe_intex::api::read_series(storage, series_id)?;
    let settled = nft_balance_of(storage, holder, settled_token_id(series_id))?;
    if settled < amount {
        return Err(IntexFactoryError::InsufficientSettled.into());
    }

    let promis_amount = series
        .promis_load_minor
        .checked_mul(amount)
        .ok_or_else(|| PrecompileError::Revert("promis amount overflow".into()))?;

    // PoW over the per-(series, holder) sequence; bump it on success.
    let mut factory = IntexFactoryContract::new(storage.clone());
    let seq = factory.read_mine_seq(series_id, holder)?;
    validate_pow(holder, promis_amount, series_id, seq, nonce)?;
    factory.write_mine_seq(series_id, holder, seq + 1)?;

    // Burn Settled from holder on the NFT.
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::burnSettledCall {
            holder,
            seriesId: series_id.into(),
            amount,
        }
        .abi_encode()
        .into(),
    )?;

    // Promis is confidential: the mint runs inside the enclave, authorized by the
    // holder's Promis modify key (the `mac`/`opNonce` must bind `promis_amount`).
    outbe_promisfactory::api::mint(storage.clone(), holder, promis_amount, auth)?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::PromisMined {
            seriesId: series_id.into(),
            holder,
            amount,
            promisAmount: promis_amount,
        },
    )?;
    Ok(promis_amount)
}

/// Settled token id = `uint256(keccak256("SETTLED" ++ seriesId))`.
pub(crate) fn settled_token_id(series_id: SeriesId) -> U256 {
    let mut buf = Vec::with_capacity(7 + SERIES_ID_LEN);
    buf.extend_from_slice(b"SETTLED");
    buf.extend_from_slice(series_id.as_bytes());
    U256::from_be_bytes(keccak256(&buf).0)
}

/// PoW hash: `SHA256(hex(holder ++ promisAmount ++ seriesId ++ seq) ++ nonce_be8)`.
pub(crate) fn compute_pow_hash(
    holder: Address,
    promis_amount: U256,
    series_id: SeriesId,
    seq: u32,
    nonce: U256,
) -> Result<[u8; 32]> {
    if nonce > U256::from(u64::MAX) {
        return Err(PrecompileError::Revert("nonce exceeds uint64 range".into()));
    }
    let mut preimage = String::new();
    preimage.push_str(&hex::encode(holder.as_slice()));
    preimage.push_str(&hex::encode(promis_amount.to_be_bytes::<32>()));
    preimage.push_str(&hex::encode(series_id.as_bytes()));
    preimage.push_str(&hex::encode(seq.to_be_bytes()));

    let mut data = preimage.into_bytes();
    data.extend_from_slice(&nonce.to::<u64>().to_be_bytes());

    let digest = ring::digest::digest(&ring::digest::SHA256, &data);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    Ok(out)
}

/// The PoW hash must have `POW_DIFFICULTY` leading zero bytes.
pub(crate) fn validate_pow(
    holder: Address,
    promis_amount: U256,
    series_id: SeriesId,
    seq: u32,
    nonce: U256,
) -> Result<()> {
    let hash = compute_pow_hash(holder, promis_amount, series_id, seq, nonce)?;
    for b in &hash[..POW_DIFFICULTY] {
        if *b != 0 {
            return Err(IntexFactoryError::InsufficientProofOfWork.into());
        }
    }
    Ok(())
}
