//! IntexFactory runtime use-cases: issuance, settlement, Promis mining.

use alloy_primitives::{keccak256, Address, U256};
use alloy_sol_types::{SolCall, SolEvent};

use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use outbe_intex::IntexState;

use crate::config;
use crate::constants::{
    CALL_PRICE_DEN, FLOOR_PRICE_DEN, INTEX_NFT1155_ADDRESS, ORIGIN_ROUTER_ADDRESS,
    POW_DIFFICULTY,
};
use crate::errors::IntexFactoryError;
use crate::schema::{IntexFactoryContract, IssuanceParams};
use crate::sol_ext::IIntexNFT1155::{CreateSeriesParams, IntexCallTrigger};
use crate::sol_ext::{IIntexNFT1155, IOriginRouter, IERC20};

/// Emit an IntexFactory event from `INTEX_FACTORY_ADDRESS`.
pub(crate) fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(INTEX_FACTORY_ADDRESS, event.encode_log_data())
}

/// Capture series identity in Intex and enroll it in the floor-bin
/// index. The outbound LayerZero send is added with messenger wiring.
pub fn issue(storage: &StorageHandle<'_>, params: IssuanceParams) -> Result<()> {
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

    // Register the series on the local IntexNFT1155 so holders can be tracked
    // on the Outbe side (required for the call-bridge credit path).
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::createSeriesCall {
            params: CreateSeriesParams {
                seriesId: params.series_id,
                issuanceCurrency: params.issuance_currency,
                referenceCurrency: params.reference_currency,
                issuedIntexCount: params.issued_intex_count,
                promisLoadMinor: params.promis_load_minor,
                entryPriceMinor: entry_price_minor_u64,
                floorPriceMinor: floor_price_minor_u64,
                callPriceMinor: call_price_minor_u64,
                callTrigger: IntexCallTrigger {
                    windowDays: cfg.call_window_days,
                    thresholdDays: cfg.call_threshold_days,
                    intexCallPeriod: cfg.intex_call_period_secs,
                },
            },
        }
        .abi_encode()
        .into(),
    )?;

    // Send ISSUANCE_INSTRUCTIONS to BNB over the bridge (relay-float-funded, see below).
    let floor_price_minor_u64 = u64::try_from(floor_price_minor)
        .map_err(|_| PrecompileError::Revert("floor price exceeds u64".into()))?;
    let call_price_minor_u64 = u64::try_from(call_price_minor)
        .map_err(|_| PrecompileError::Revert("call price exceeds u64".into()))?;
    let messenger_params = IOriginRouter::IssuanceInstructionsParams {
        seriesId: params.series_id,
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
        recipients: params.recipients,
        quantities: params.quantities,
    };
    // Relay-float-funded: value 0, so the messenger self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendIssuanceInstructionsCall {
            params: messenger_params,
        }
        .abi_encode()
        .into(),
    )?;

    // Enroll into the unqualified floor-bin index for begin_block qualify.
    factory.insert_unqualified(params.series_id, floor_price_minor)?;

    emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesIssued {
            seriesId: params.series_id,
            issuedIntexCount: params.issued_intex_count,
            entryPrice: params.entry_price_minor,
        },
    )
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
            spender: outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS,
            amount: received,
        }
        .abi_encode()
        .into(),
    )?;

    let shares = outbe_vaultprovider::api::deposit_liquidity(
        storage.clone(),
        INTEX_FACTORY_ADDRESS,
        payment_token,
        received,
        outbe_vaultprovider::api::LiquiditySource::IntexStrikePrice,
    )?;
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
    let asset = outbe_vaultprovider::api::asset_at(storage.clone(), 0)?;
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

    outbe_promisfactory::api::mine(storage.clone(), holder, promis_amount)?;

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
