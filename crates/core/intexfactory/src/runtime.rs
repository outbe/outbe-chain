//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{keccak256, Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_primitives::addresses::{INTEX_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_intex::IntexState;
use outbe_vaultprovider::api::IVaultProvider;

use crate::config;
use crate::constants::{
    CALL_PRICE_DEN, DIST_CHUNK_LIMIT, FLOOR_PRICE_DEN, INTEX_NFT1155_ADDRESS,
    ORIGIN_ROUTER_ADDRESS, POW_DIFFICULTY, PROCEEDS_FANIN_TIMEOUT_SECS,
};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IERC20};

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Capture series identity in Intex, enroll it in the floor-bin index, and send
/// ISSUANCE_INSTRUCTIONS to every target chain of the day's snapshot. The
/// canonical IntexNFT1155 createSeries now arrives per chain via the ISSUANCE
/// broadcast (including a loopback leg on the origin), so there is no in-process
/// NFT call here.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<()> {
    if params.issued_intex_count == 0 {
        // Zero-winner clearing: no series is created anywhere, so the day's
        // lysis-recorded contributor map would never distribute — discard it.
        return outbe_intex::api::finalize_proceeds(storage, params.worldwide_day);
    }

    // u32 timestamp; bounded until 2106.
    let issued_at = u32::try_from(storage.timestamp()?.to::<u64>())
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;

    let mut factory = IntexFactoryContract::new(storage.clone());
    let cfg = config::read(&factory)?;

    let floor_price_minor = derived_floor(params.entry_price_minor, cfg.floor_price_num)?;
    let call_price_minor = derived_call_price(params.entry_price_minor, cfg.call_price_num)?;

    let entry_price_minor_u64 = u64::try_from(params.entry_price_minor)
        .map_err(|_| PrecompileError::Revert("entry price exceeds u64".into()))?;
    let floor_price_minor_u64 = u64::try_from(floor_price_minor)
        .map_err(|_| PrecompileError::Revert("floor price exceeds u64".into()))?;
    let call_price_minor_u64 = u64::try_from(call_price_minor)
        .map_err(|_| PrecompileError::Revert("call price exceeds u64".into()))?;

    let record = outbe_intex::CreateSeriesParams {
        series_id: params.series_id,
        worldwide_day: params.worldwide_day,
        issued_intex_count: params.issued_intex_count,
        promis_load_minor: params.promis_load_minor,
        entry_price_minor: params.entry_price_minor,
        floor_price_minor,
        call_price_minor,
        call_trigger: outbe_intex::IntexCallTrigger {
            window_days: cfg.call_window_days,
            threshold_days: cfg.call_threshold_days,
            intex_call_period: cfg.intex_call_period_secs,
        },
        issued_at,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
    };
    outbe_intex::api::create_series(storage, record)?;

    // One ISSUANCE per snapshot chain. Relay-float-funded: value 0, the router pays the bridge
    // fee from its float.
    for (chain_id, recipients, quantities) in issuance_legs(&params) {
        let router_params = IOriginRouter::IssuanceInstructionsParams {
            dstChainId: chain_id,
            seriesId: params.series_id,
            worldwideDay: params.worldwide_day,
            issuedIntexCount: params.issued_intex_count,
            promisLoadMinor: params.promis_load_minor,
            entryPriceMinor: entry_price_minor_u64,
            floorPriceMinor: floor_price_minor_u64,
            intexCallPeriod: cfg.intex_call_period_secs,
            issuanceCurrency: params.issuance_currency,
            referenceCurrency: params.reference_currency,
            callWindowDays: cfg.call_window_days,
            callThresholdDays: cfg.call_threshold_days,
            callPriceMinor: call_price_minor_u64,
            recipients,
            quantities,
        };
        storage.call(
            ORIGIN_ROUTER_ADDRESS,
            U256::ZERO,
            IOriginRouter::sendIssuanceInstructionsCall {
                params: router_params,
            }
            .abi_encode()
            .into(),
        )?;
    }

    // Enroll into the unqualified floor-bin index for begin_block qualify.
    factory.insert_unqualified(params.series_id, floor_price_minor)?;

    // Arm the creator-reward proceeds fan-in: the winning chains are expected to
    // route proceeds; creators are paid once all arrive or the deadline passes.
    let deadline = storage
        .timestamp()?
        .to::<u64>()
        .saturating_add(PROCEEDS_FANIN_TIMEOUT_SECS);
    outbe_intex::api::arm_proceeds(
        storage,
        params.series_id,
        &params.recipient_chains,
        deadline,
    )?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesIssued {
            seriesId: params.series_id,
            issuedIntexCount: params.issued_intex_count,
            entryPrice: params.entry_price_minor,
        },
    )
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

pub(crate) fn derived_floor(entry_price: U256, floor_price_num: u64) -> Result<U256> {
    entry_price
        .checked_mul(U256::from(floor_price_num))
        .map(|v| v / U256::from(FLOOR_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("floor price overflow".into()))
}

pub(crate) fn derived_call_price(entry_price: U256, call_price_num: u64) -> Result<U256> {
    entry_price
        .checked_mul(U256::from(call_price_num))
        .map(|v| v / U256::from(CALL_PRICE_DEN))
        .ok_or_else(|| PrecompileError::Revert("call price overflow".into()))
}

/// Per-Intex cost = entry_price * promis_load / 1e30 (payment-token minor).
/// Mirrors the desis derivation: entry(1e18) * PROMIS_LOAD / 1e12, expressed via
/// promis_load_minor (= PROMIS_LOAD * 1e18), so the divisor is 1e30.
pub(crate) fn derived_cost_amount(entry_price: U256, promis_load_minor: U256) -> Result<U256> {
    entry_price
        .checked_mul(promis_load_minor)
        .map(|v| v / U256::from(10u64).pow(U256::from(30u64)))
        .ok_or_else(|| PrecompileError::Revert("cost amount overflow".into()))
}

/// Set the dual-wallet authorized settler for `holder`'s position in `series_id`.
/// `holder` is the caller (the precompile passes its caller).
pub fn set_authorized_settler(
    storage: &StorageHandle<'_>,
    holder: Address,
    series_id: u32,
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
    worldwide_day: u32,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    if caller != ORIGIN_ROUTER_ADDRESS {
        return Err(IntexFactoryError::NotOriginRouter.into());
    }
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    outbe_intex::api::credit_proceeds(storage, worldwide_day, src_chain_id, amount)?;
    let now = storage.timestamp()?.to::<u64>();
    try_settle_proceeds(storage, worldwide_day, now)
}

/// Start a distribution round for a series if its proceeds fan-in is satisfied
/// (all winning chains in) or its deadline has passed. Idempotent: it no-ops
/// while a round is still draining, so repeated arrivals and the begin-block
/// sweep can both call it safely.
pub(crate) fn try_settle_proceeds(
    storage: &StorageHandle<'_>,
    series_id: u32,
    now: u64,
) -> Result<()> {
    // Never overlap a round that is still paying out.
    if outbe_intex::api::get_progress(storage, series_id)?.is_some() {
        return Ok(());
    }
    let deadline = outbe_intex::api::proceeds_deadline(storage, series_id)?;
    if deadline == 0 {
        return Ok(()); // never armed (no issuance for this series)
    }
    let complete = outbe_intex::api::proceeds_ready(storage, series_id)?;
    if !complete && now < deadline {
        return Ok(()); // keep waiting for the remaining chains
    }

    let pot = outbe_intex::api::take_proceeds_pot(storage, series_id)?;
    if pot.is_zero() {
        // Nothing new to pay. Once every chain is in, finalize (clears the map);
        // a forced empty round just idles until a late arrival tops the pot up.
        if complete {
            outbe_intex::api::finalize_proceeds(storage, series_id)?;
        }
        return Ok(());
    }

    let total = outbe_intex::api::contributor_total(storage, series_id)?;
    if total.is_zero() {
        // Ownerless proceeds: sweep to the reserve vault instead of stranding them.
        sweep_to_reserve(storage, series_id, pot)?;
        if complete {
            outbe_intex::api::finalize_proceeds(storage, series_id)?;
        }
        return Ok(());
    }

    // Finalize on completion only when every winning chain is in; otherwise the
    // deadline forced a partial payout and the map is retained for a top-up.
    outbe_intex::api::set_proceeds_finalize_on_done(storage, series_id, complete)?;
    outbe_intex::api::start_distribution(storage, series_id, pot, total)
}

/// Begin-block sweep: settle every series whose proceeds fan-in deadline has
/// passed. Each series runs in its own checkpoint so one failure is retried next
/// block instead of halting the block.
pub(crate) fn sweep_proceeds_deadlines(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = outbe_intex::api::awaiting_proceeds_count(storage)?;
    let mut series_ids = Vec::with_capacity(count as usize);
    for i in 0..count {
        series_ids.push(outbe_intex::api::awaiting_proceeds_at(storage, i)?);
    }
    for series_id in series_ids {
        let res = storage.with_checkpoint(|| try_settle_proceeds(storage, series_id, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::intexfactory", series_id, error = ?e, "proceeds sweep: skipping series");
        }
    }
    Ok(())
}

/// Sweep ownerless proceeds to the reserve vault (native transfer to the vault
/// provider), so a series with no recorded contributors is not stranded.
fn sweep_to_reserve(storage: &StorageHandle<'_>, series_id: u32, amount: U256) -> Result<()> {
    storage.transfer_balance(INTEX_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::ProceedsSweptToReserve {
            seriesId: series_id,
            amount,
        },
    )
}

/// Pay up to `limit` contributors of an in-flight distribution, advancing the
/// cursor. The last contributor absorbs the integer-division remainder so the
/// full `amount` is paid out exactly. On reaching the last contributor the
/// distribution is finalized (progress + contributor map cleared). Driven by
/// the begin-block drain.
pub(crate) fn pay_chunk(storage: &StorageHandle<'_>, series_id: u32, limit: u32) -> Result<()> {
    let mut progress = outbe_intex::api::get_progress(storage, series_id)?
        .ok_or(IntexFactoryError::NoDistribution(series_id))?;
    let count = outbe_intex::api::contributor_count(storage, series_id)?;
    let end = progress.cursor.saturating_add(limit).min(count);

    // A zero denominator would panic on divide; begin-block panics halt the chain (not checkpoint-isolated),
    // so fail as an isolated Err instead.
    if progress.total_nominal.is_zero() {
        return Err(IntexFactoryError::NoContributors(series_id).into());
    }

    let mut paid = progress.paid_so_far;
    for i in progress.cursor..end {
        let (owner, nominal) = outbe_intex::api::contributor_at(storage, series_id, i)?;
        // The final contributor absorbs the rounding remainder so the sum of
        // payouts equals `amount` exactly. checked_mul: isolated Err over a silent wrap.
        let share = if i == count - 1 {
            progress.amount - paid
        } else {
            progress
                .amount
                .checked_mul(nominal)
                .ok_or(IntexFactoryError::DistributionOverflow(series_id))?
                / progress.total_nominal
        };
        storage.transfer_balance(INTEX_FACTORY_ADDRESS, owner, share)?;
        paid += share;
    }

    if end == count {
        // End this round (progress + active-set entry). Whether the contributor
        // map is also cleared depends on the fan-in: finalize when every winning
        // chain is in, otherwise retain the map for a late top-up.
        outbe_intex::api::finish_distribution_round(storage, series_id)?;
        emit_event(
            storage,
            crate::precompile::IIntexFactory::ProceedsDistributed {
                seriesId: series_id,
                amount: progress.amount,
                contributors: count,
            },
        )?;
        if outbe_intex::api::proceeds_finalize_on_done(storage, series_id)? {
            // A straggler (or a chain sending its proceeds in parts) can top the
            // pot up while this final round drains. finalize clears the map, so
            // pay any such top-up over it first and finalize only once the pot is
            // empty — otherwise the top-up is later swept to the reserve.
            let pot = outbe_intex::api::take_proceeds_pot(storage, series_id)?;
            if pot.is_zero() {
                outbe_intex::api::finalize_proceeds(storage, series_id)?;
            } else {
                let total = outbe_intex::api::contributor_total(storage, series_id)?;
                outbe_intex::api::start_distribution(storage, series_id, pot, total)?;
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
    let mut series_ids = Vec::with_capacity(count as usize);
    for i in 0..count {
        series_ids.push(outbe_intex::api::active_dist_at(storage, i)?);
    }
    for series_id in series_ids {
        // Per-series isolation: Err reverts the series' checkpoint, retried next block.
        let res = storage.with_checkpoint(|| pay_chunk(storage, series_id, DIST_CHUNK_LIMIT));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::intexfactory", series_id, error = ?e, "distribution drain: skipping series");
        }
    }
    Ok(())
}

/// Settle: `settler` is the caller. Gating reads Intex; value movement
/// (token / vault / NFT) goes via storage.call.
pub fn settle(
    storage: &StorageHandle<'_>,
    series_id: u32,
    intex_holder: Address,
    settler: Address,
    amount: U256,
) -> Result<()> {
    if intex_holder.is_zero() || settler.is_zero() {
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
        let deadline = u64::from(series.called_at) + u64::from(series.intex_call_period);
        if now > deadline {
            return Err(IntexFactoryError::DeadlineExpired.into());
        }
    }

    // Issued balance (NFT). Issued token id = uint256(seriesId).
    let issued_token_id = U256::from(series_id);
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

    // payment = per-Intex cost * amount; cost derives from entry_price * promis_load.
    let payment = derived_cost_amount(series.entry_price_minor, series.promis_load_minor)?
        .checked_mul(amount)
        .ok_or_else(|| PrecompileError::Revert("settlement cost overflow".into()))?;

    // Pull payment from the settler, deposit into the reserve vault.
    // Fee-on-transfer safe: measure the received delta.
    let payment_token = vault_asset(storage)?;
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
            spender: VAULT_PROVIDER_ADDRESS,
            amount: received,
        }
        .abi_encode()
        .into(),
    )?;

    // Deposit into the reserve vault via the provider's Solidity ABI.
    let shares = outbe_vaultprovider::api::deposit_liquidity(storage, payment_token, received)?;
    if shares.is_zero() {
        return Err(IntexFactoryError::ZeroSharesReceived.into());
    }

    // Burn Issued from holder, mint Settled to the settler.
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::settleCall {
            seriesId: series_id,
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
            seriesId: series_id,
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
        IIntexNFT1155::balanceOfCall { account, id }
            .abi_encode()
            .into(),
    )?;
    IIntexNFT1155::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("NFT balanceOf undecodable".into()))
}

fn vault_asset(storage: &StorageHandle<'_>) -> Result<Address> {
    // TODO pick up the asset ERC20 address properly
    let ret = storage.staticcall(
        VAULT_PROVIDER_ADDRESS,
        IVaultProvider::assetAtCall { index: U256::ZERO }
            .abi_encode()
            .into(),
    )?;
    let asset = IVaultProvider::assetAtCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("assetAt undecodable".into()))?;
    if asset.is_zero() {
        return Err(IntexFactoryError::NotWired.into());
    }
    Ok(asset)
}

fn erc20_balance_of(storage: &StorageHandle<'_>, token: Address, account: Address) -> Result<U256> {
    let ret = storage.staticcall(token, IERC20::balanceOfCall { account }.abi_encode().into())?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("ERC20 balanceOf undecodable".into()))
}

/// minePromis: PoW-gated burn of Settled then mint of Promis. `holder` is the
/// caller.
pub fn mine_promis(
    storage: &StorageHandle<'_>,
    series_id: u32,
    holder: Address,
    amount: U256,
    nonce: U256,
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
            seriesId: series_id,
            amount,
        }
        .abi_encode()
        .into(),
    )?;

    outbe_promisfactory::api::mint(storage.clone(), holder, promis_amount)?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::PromisMined {
            seriesId: series_id,
            holder,
            amount,
            promisAmount: promis_amount,
        },
    )?;
    Ok(promis_amount)
}

/// Settled token id = `uint256(keccak256("SETTLED" ++ seriesId))`.
pub(crate) fn settled_token_id(series_id: u32) -> U256 {
    let mut buf = Vec::with_capacity(7 + 4);
    buf.extend_from_slice(b"SETTLED");
    buf.extend_from_slice(&series_id.to_be_bytes());
    U256::from_be_bytes(keccak256(&buf).0)
}

/// PoW hash: `SHA256(hex(holder ++ promisAmount ++ seriesId ++ seq) ++ nonce_be8)`.
pub(crate) fn compute_pow_hash(
    holder: Address,
    promis_amount: U256,
    series_id: u32,
    seq: u32,
    nonce: U256,
) -> Result<[u8; 32]> {
    if nonce > U256::from(u64::MAX) {
        return Err(PrecompileError::Revert("nonce exceeds uint64 range".into()));
    }
    let mut preimage = String::new();
    preimage.push_str(&hex::encode(holder.as_slice()));
    preimage.push_str(&hex::encode(promis_amount.to_be_bytes::<32>()));
    preimage.push_str(&hex::encode(series_id.to_be_bytes()));
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
    series_id: u32,
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
