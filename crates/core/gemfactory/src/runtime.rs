use alloy_primitives::{Address, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_gem::{api as gem_api, GemAddParams, GemState};
use outbe_oracle::contract::OracleContract;
use outbe_primitives::addresses::{
    GEM_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS, VAULT_ROUTER_ADDRESS,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use outbe_common::pow;

use crate::constants::{
    CALL_RATE, FLOOR_RATE, POSITION_VALIDITY_SECONDS, SRA_RATE,
};
use crate::errors::GemFactoryError;
use crate::precompile::IGemFactory::{GemBurned, GemIssued, GemSettled};
use crate::schema::{GemFactoryContract, GemPosition, GemTypes};
use crate::sol_ext::{IIntexNFT1155, IReferenceCurrency, IERC20};

pub fn mint_gem(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    gem_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }

    // TODO(multi-currency): only supports gems
    // currencies are the same. Cross-rate resolution (e.g. issuance=EUR with
    // reference=USD) needs a chained oracle lookup that's not wired yet
    outbe_oracle::api::check_reference_currency_with_storage(storage.clone(), reference_currency)?;

    // Entry/floor/call are measured against the reference currency (the same
    // COEN/<reference> rate the qualify/call scans compare against).
    let coen_rate = read_oracle_rate(storage, reference_currency)?;
    let issued_at = storage.timestamp()?.to::<u64>();
    let (cost_amount, floor_price, initial_state) = compute_params(gem_type, gem_load, coen_rate)?;
    let entry_price = coen_rate;
    let call_price = derived_call_price(entry_price)?;

    let params = GemAddParams {
        owner,
        gem_type: gem_type as u8,
        gem_load_minor: gem_load,
        entry_price_minor: entry_price,
        cost_amount_minor: cost_amount,
        floor_price_minor: floor_price,
        call_price_minor: call_price,
        call_rate: CALL_RATE as u16,
        call_window: outbe_gem::CALL_WINDOW,
        call_threshold: outbe_gem::CALL_THRESHOLD,
        issuance_currency,
        reference_currency,
        initial_state,
        issued_at,
    };
    let gem_id = gem_api::add_gem(storage, params)?;

    let factory = GemFactoryContract::new(storage.clone());
    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: gem_type as u8,
            owner,
            gemLoad: gem_load,
            entryPrice: entry_price,
            costAmount: cost_amount,
            floorPrice: floor_price,
            issuedAt: issued_at,
        },
    )?;

    Ok(gem_id)
}

/// Park a merchant's whole Intex series and mint a GemPosition NFT. Burns the
/// merchant's entire Issued holding on IntexNFT1155 (`parkIntex`, GEM_ROLE)
/// and records the position with a snapshot of the source entry/floor and the
/// resulting Promis capacity. Returns the minted `position_id`.
pub fn mint_gem_position(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_intex_id: u32,
    amount: U256,
) -> Result<U256> {
    if caller.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }

    let series = outbe_intex::api::get_series(storage, source_intex_id)?
        .ok_or(GemFactoryError::SourceIntexNotFound)?;
    // v1 supports only same-currency positions (mirrors mint_gem's limitation).
    if series.issuance_currency != series.reference_currency {
        return Err(GemFactoryError::IssuanceReferenceMismatch {
            issuance: series.issuance_currency,
            reference: series.reference_currency,
        }
        .into());
    }

    // Burn `amount` of the merchant's Intex units; `parkIntex` returns the
    // burned count (and reverts on a non-parkable state or a zero amount).
    let units = burn_parked_intex(storage, caller, source_intex_id, amount)?;
    let capacity = series
        .promis_load_minor
        .checked_mul(units)
        .ok_or(GemFactoryError::Overflow)?;

    let parked_at = storage.timestamp()?.to::<u64>();
    let position_id =
        GemFactoryContract::generate_position_id(source_intex_id, storage.block_number()?);

    let mut factory = GemFactoryContract::new(storage.clone());
    factory.add_position(&GemPosition {
        position_id,
        merchant: caller,
        source_intex_id,
        remaining_capacity: capacity,
        source_entry_price: series.entry_price_minor,
        source_floor_price: series.floor_price_minor,
        issuance_currency: series.issuance_currency,
        reference_currency: series.reference_currency,
        parked_at,
    })?;

    let prev_parked = factory.total_intex_parked.read()?;
    let new_parked = prev_parked
        .checked_add(capacity)
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_intex_parked.write(new_parked)?;

    Ok(position_id)
}

/// Burn `amount` of the merchant's Issued Intex units via `parkIntex`
/// (GEM_ROLE) and return the burned count. Reverts if the series is in a
/// non-parkable (non-Issued/Qualified) state or `amount` is zero.
fn burn_parked_intex(
    storage: &StorageHandle<'_>,
    holder: Address,
    series_id: u32,
    amount: U256,
) -> Result<U256> {
    let ret = storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::parkIntexCall {
            holder,
            seriesId: series_id,
            amount,
        }
        .abi_encode()
        .into(),
    )?;
    IIntexNFT1155::parkIntexCall::abi_decode_returns(&ret)
        .map_err(|_| PrecompileError::Revert("parkIntex return undecodable".into()).into())
}

/// Issue one Merchant gem to a customer, draining the position's capacity.
pub fn mint_merchant_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    owner: Address,
    gem_load: U256,
) -> Result<U256> {
    if owner.is_zero() {
        return Err(GemFactoryError::InvalidOwner.into());
    }

    let factory = GemFactoryContract::new(storage.clone());
    let mut record = factory
        .positions
        .get(position_id)?
        .ok_or(GemFactoryError::PositionNotFound)?;
    if record.merchant != caller {
        return Err(GemFactoryError::NotPositionOwner.into());
    }

    let now = storage.timestamp()?.to::<u64>();
    if now >= record.parked_at + POSITION_VALIDITY_SECONDS {
        return Err(GemFactoryError::PositionExpired.into());
    }
    let remaining = record
        .remaining_capacity
        .checked_sub(gem_load)
        .ok_or(GemFactoryError::InsufficientCapacity)?;

    let coen_rate = read_oracle_rate(storage, record.reference_currency)?;
    let entry_price = coen_rate.max(record.source_entry_price);
    let cost_amount = compute_cost(entry_price, gem_load, 100)?;
    let floor_price = derived_floor(entry_price)?.max(record.source_floor_price);
    let call_price = derived_call_price(entry_price)?;

    let gem_id = gem_api::add_gem(
        storage,
        GemAddParams {
            owner,
            gem_type: GemTypes::Merchant as u8,
            gem_load_minor: gem_load,
            entry_price_minor: entry_price,
            cost_amount_minor: cost_amount,
            floor_price_minor: floor_price,
            call_price_minor: call_price,
            call_rate: CALL_RATE as u16,
            call_window: outbe_gem::CALL_WINDOW,
            call_threshold: outbe_gem::CALL_THRESHOLD,
            issuance_currency: record.issuance_currency,
            reference_currency: record.reference_currency,
            initial_state: GemState::Issued,
            issued_at: now,
        },
    )?;

    record.remaining_capacity = remaining;
    factory.positions.update(&record)?;

    let prev_total = factory.total_gems_issued.read()?;
    let new_total = prev_total
        .checked_add(U256::from(1))
        .ok_or(GemFactoryError::Overflow)?;
    factory.total_gems_issued.write(new_total)?;

    emit_event(
        storage,
        GemIssued {
            gemId: gem_id,
            gemType: GemTypes::Merchant as u8,
            owner,
            gemLoad: gem_load,
            entryPrice: entry_price,
            costAmount: cost_amount,
            floorPrice: floor_price,
            issuedAt: now,
        },
    )?;

    Ok(gem_id)
}

pub fn settle_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    asset: Address,
) -> Result<()> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.owner != caller {
        return Err(GemFactoryError::NotGemOwner.into());
    }
    // Settlement is allowed from Qualified (voluntary) or Called (forced). A
    // Called gem must settle before its notice period lapses.
    match item.state {
        s if s == GemState::Qualified as u8 => {}
        s if s == GemState::Called as u8 => {
            let now = storage.timestamp()?.to::<u64>();
            let deadline = item.called_at + u64::from(item.call_notice_period);
            if now > deadline {
                return Err(GemFactoryError::DeadlineExpired.into());
            }
        }
        _ => return Err(GemFactoryError::InvalidState.into()),
    }

    // The Cost Amount is paid in the gem's Settlement Currency
    // Reject any payment asset whose ISO 4217 code differs from it.
    let expected =
        settlement_currency_iso(storage, item.issuance_currency, item.reference_currency)?;
    let asset_iso = read_iso_code(storage, asset)?;
    if asset_iso != expected {
        return Err(GemFactoryError::SettlementCurrencyMismatch {
            asset: asset_iso,
            expected,
        }
        .into());
    }

    gem_api::set_state(storage, gem_id, GemState::Settled)?;

    if !item.cost_amount_minor.is_zero() {
        // The caller pays in `asset`, validated above to match the Settlement
        // Currency.
        deposit_to_vault(storage, caller, item.cost_amount_minor, asset)?;
    }

    emit_event(
        storage,
        GemSettled {
            gemId: gem_id,
            owner: caller,
            amountPaid: item.cost_amount_minor,
            issuanceCurrency: item.issuance_currency,
        },
    )?;

    Ok(())
}

fn deposit_to_vault(
    storage: &StorageHandle<'_>,
    caller: Address,
    amount: U256,
    asset: Address,
) -> Result<()> {
    let transfer = IERC20::transferFromCall {
        from: caller,
        to: GEM_FACTORY_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, transfer.into())?;

    let approve = IERC20::approveCall {
        spender: VAULT_ROUTER_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, approve.into())?;

    // Deposit into the reserve vault via the router's Solidity ABI.
    outbe_vaultrouter::api::deposit(storage, asset, amount)?;

    Ok(())
}

/// Issuance currency when a COEN
/// pair is registered for it in the Oracle, otherwise the reference currency.
fn settlement_currency_iso(
    storage: &StorageHandle<'_>,
    issuance_currency: u16,
    reference_currency: u16,
) -> Result<u16> {
    let oracle = OracleContract::new(storage.clone());
    let pair_hash = oracle.settlement_iso_to_pair.read(&issuance_currency)?;
    Ok(if pair_hash.is_zero() {
        reference_currency
    } else {
        issuance_currency
    })
}

/// Reads the settlement asset's ISO 4217 code via a static
/// `IReferenceCurrency.isoCode()` sub-call. Reverts `InvalidAsset` on
/// undecodable data. Mirrors credisfactory's `read_iso_code`.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| GemFactoryError::InvalidAsset.into())
}

pub fn mine_gem_promis(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    nonce: U256,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    let item = gem_api::get_gem(storage, gem_id)?.ok_or(GemFactoryError::GemNotFound)?;
    if item.owner != caller {
        return Err(GemFactoryError::NotGemOwner.into());
    }
    if item.state != GemState::Settled as u8 {
        return Err(GemFactoryError::InvalidState.into());
    }

    validate_pow(gem_id, nonce)?;

    gem_api::burn(storage, gem_id)?;

    // The Promis is confidential: the mint runs inside the enclave, authorized by
    // the gem owner's Promis modify key. The client's `mac`/`opNonce` must bind the
    // minted amount (`item.gem_load_minor`), so the client precomputes it.
    outbe_promisfactory::api::mint(storage.clone(), caller, item.gem_load_minor, auth)?;

    emit_event(
        storage,
        GemBurned {
            gemId: gem_id,
            owner: caller,
            gemLoad: item.gem_load_minor,
        },
    )?;

    Ok(item.gem_load_minor)
}

/// Looks up the COEN/`issuance_currency` rate via Oracle's
/// `settlement_iso_to_pair` registry. Reverts with
/// `IssuanceCurrencyNotRegistered` if the ISO code has no pair mapping, or
/// `OracleUnavailable` if the pair exists but no rate has been published.
fn read_oracle_rate(storage: &StorageHandle<'_>, issuance_currency: u16) -> Result<U256> {
    let oracle = OracleContract::new(storage.clone());
    let pair_hash = oracle.settlement_iso_to_pair.read(&issuance_currency)?;
    if pair_hash.is_zero() {
        return Err(GemFactoryError::IssuanceCurrencyNotRegistered {
            iso_code: issuance_currency,
        }
        .into());
    }
    let rate = oracle.exchange_rate.read(&pair_hash)?;
    if rate.is_zero() {
        return Err(GemFactoryError::OracleUnavailable.into());
    }
    Ok(rate)
}

fn compute_params(
    gem_type: GemTypes,
    gem_load: U256,
    coen_rate: U256,
) -> Result<(U256, U256, GemState)> {
    let (cost_amount, floor_price, initial_state) = match gem_type {
        // Genesis: validator gem during the genesis window — born Qualified
        // (no maturity wait), but validators pay like every other agent
        // class: cost = entry × load, floor = rate × 1.08. settleGem moves
        // `cost_amount` into the Reserve vault just like Wallet/Cca/Sra.
        GemTypes::Genesis => {
            let cost = compute_cost(coen_rate, gem_load, 100)?;
            let floor = derived_floor(coen_rate)?;
            (cost, floor, GemState::Qualified)
        }
        GemTypes::Sra => {
            let cost = compute_cost(coen_rate, gem_load, SRA_RATE)?;
            let floor = derived_floor(coen_rate)?;
            (cost, floor, GemState::Issued)
        }
        // Validator (post-genesis), Wallet, Cca — standard agent-class flow:
        // cost = entry × load, floor = rate × 1.08, born Issued.
        GemTypes::Validator | GemTypes::Wallet | GemTypes::Cca => {
            let cost = compute_cost(coen_rate, gem_load, 100)?;
            let floor = derived_floor(coen_rate)?;
            (cost, floor, GemState::Issued)
        }
        // Merchant gems are minted via `mint_merchant_gem` against a GemPosition,
        // not through this agent-class path.
        GemTypes::Merchant => return Err(GemFactoryError::UnsupportedGemType.into()),
    };
    Ok((cost_amount, floor_price, initial_state))
}

/// `(entry × load × percent / 100) / SCALE_1E18`. Both `entry` and `load` are
/// 1e18-scaled minor units; result stays in the same scale.
fn compute_cost(entry: U256, load: U256, cost_num: u64) -> Result<U256> {
    let acc = entry
        .checked_mul(load)
        .ok_or(GemFactoryError::Overflow)?
        .checked_mul(U256::from(cost_num))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64) / SCALE_1E18)
}

/// Floor price = `entry × (100 + FLOOR_RATE) / 100` (8% markup => 1.08x).
fn derived_floor(entry_price: U256) -> Result<U256> {
    let acc = entry_price
        .checked_mul(U256::from(100 + FLOOR_RATE))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64))
}

/// Call price = `entry × (100 + CALL_RATE) / 100` (128% markup => 2.28x).
/// Entry equals the issuance-time coen rate in the single-currency case.
fn derived_call_price(entry_price: U256) -> Result<U256> {
    let acc = entry_price
        .checked_mul(U256::from(100 + CALL_RATE))
        .ok_or(GemFactoryError::Overflow)?;
    Ok(acc / U256::from(100u64))
}

fn emit_event<E: SolEvent>(storage: &StorageHandle<'_>, event: E) -> Result<()> {
    storage.emit_event(GEM_FACTORY_ADDRESS, event.encode_log_data())
}

/// PoW gate for `mine_gem_promis`, delegating to the shared
/// [`outbe_common::pow`] scheme and mapping failures onto [`GemFactoryError`].
pub fn validate_pow(gem_id: U256, nonce: U256) -> Result<()> {
    pow::validate_pow(gem_id, nonce).map_err(|e| GemFactoryError::from(e).into())
}
