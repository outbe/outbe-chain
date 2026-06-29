//! Orchestration logic for the credisfactory precompile.
//!
//! - [`request_credis`] verifies a pledge-commitment spend proof through the
//!   gratispool, opens a credis position bound to `bundleAccount`, persists
//!   the user-supplied `(denom_id, reclaim_commitment)` pair for later
//!   reclaim insertion, and delivers the stablecoin loan via the vault
//!   sub-call.
//! - [`pay_anadosis`] advances the position by one installment. When the
//!   position completes (`next_anadosis_number > NUMBER_OF_ANADOSIS`), the
//!   stored reclaim commitment is inserted back into the gratispool so the
//!   reclaim-secret holder can later `unpledgeGratis(args, destination)`.
//!
//! Pre-pool note: the previous flow looked up a plaintext `PledgeTicket` by
//! `secret`, ran a fidelity check on the pledger's address, and minted a
//! fresh ticket per installment. After the shielded-pool migration the
//! pledger's address is no longer observable to the factory, so:
//!
//! - the fidelity gate operates on `bundleAccount` rather than the pledger;
//! - the per-installment ticket mint becomes a single reclaim-commitment
//!   insert at position completion (PoC limitation).

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;

use outbe_credis::{AnadosisResult, CredisContract, NUMBER_OF_ANADOSIS};
use outbe_gratispool::api as pool;
use outbe_gratispool::SpendArgs;
use outbe_oracle::api::get_exchange_rate;
use outbe_primitives::addresses::{CREDIS_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E18;

use crate::errors::CredisFactoryError;
use crate::precompile::ICredisFactory;
use crate::schema::CredisFactoryContract;
use crate::sol_ext::IERC20;

/// Native token base symbol used for the COEN/USD oracle pair lookup.
pub const NATIVE_TOKEN: &str = "COEN";

/// Stable settlement quote symbol. Matches the pair used by tributefactory and
/// metadosis.
pub const STABLECOIN: &str = "0xUSD";

/// Decimal-gap factor between COEN (10^18) and stablecoin (10^6). Cosmos:
/// `decimalsDiff = sdk.NewIntWithDecimal(1, 12)`.
fn decimals_diff() -> U256 {
    U256::from(1_000_000_000_000u128)
}

/// Plain-Rust shape of `ICredisFactory::RequestArgs`. Held on the runtime
/// boundary so the precompile dispatch and the api crate can speak in the
/// same vocabulary without dragging the sol! macro types around.
#[derive(Debug, Clone)]
pub struct RequestArgs {
    pub merkle_root: U256,
    pub nullifier_hash: U256,
    pub denom_id: u8,
    pub receiver_binding: U256,
    pub proof: Vec<u8>,
    pub reclaim_commitment: U256,
}

impl RequestArgs {
    fn spend_args(&self) -> SpendArgs {
        SpendArgs {
            merkle_root: self.merkle_root,
            nullifier_hash: self.nullifier_hash,
            denom_id: self.denom_id,
            receiver_binding: self.receiver_binding,
            proof: self.proof.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// request_credis
// ---------------------------------------------------------------------------

/// Returns `(position_id, amount_stables)`.
///
/// `bundle_account` is the address the credis position binds to (and the
/// target inside the proof's `receiver_binding` public input).
#[allow(clippy::too_many_arguments)]
pub fn request_credis(
    storage: StorageHandle<'_>,
    _caller: Address,
    asset: Address,
    bundle_account: Address,
    args: RequestArgs,
    current_time: u64,
    _current_block: u64,
) -> Result<(U256, U256)> {
    if asset.is_zero() {
        return Err(CredisFactoryError::InvalidAsset.into());
    }
    if bundle_account.is_zero() {
        return Err(CredisFactoryError::InvalidBundleAccount.into());
    }

    // Reject borrowers with overdue anadosis on any of their positions.
    {
        let credis = CredisContract::new(storage.clone());
        if credis.has_overdue_anadosis(bundle_account, current_time)? {
            return Err(CredisFactoryError::OverduePayments.into());
        }
    }

    // Verify the pledge proof, mark the nullifier spent, and learn the
    // gratis amount from the pool's denomination ladder. Receiver binding
    // is recomputed against `bundle_account` (the action_tag is
    // ACTION_REQUEST_CREDIS inside the pool runtime) and folds in
    // `args.reclaim_commitment` as the context nonce so a mempool
    // front-runner cannot swap the reclaim leg and capture the eventual
    // `unpledgeGratis`.
    let spend_args = args.spend_args();
    let gratis_amount = pool::verify_and_spend_for_credis(
        storage.clone(),
        bundle_account,
        args.reclaim_commitment,
        &spend_args,
    )?;

    let amount_stables = convert_gratis_to_stables(storage.clone(), gratis_amount)?;

    // Open the credis position. The `commitment` argument to
    // `create_position` is what builds the position_id; we use the proof's
    // `nullifier_hash` because it is globally unique (the pool already
    // enforces nullifier uniqueness).
    let mut credis = CredisContract::new(storage.clone());
    let position_id = credis.create_position(
        args.nullifier_hash,
        bundle_account,
        VAULT_PROVIDER_ADDRESS,
        asset,
        amount_stables,
        gratis_amount,
        current_time,
    )?;

    // Persist the per-position reclaim metadata so `pay_anadosis` can insert
    // the reclaim commitment when the position completes.
    {
        let factory = CredisFactoryContract::new(storage.clone());
        factory
            .position_denom
            .write(&position_id, args.denom_id as u32)?;
        factory
            .position_reclaim_commitment
            .write(&position_id, args.reclaim_commitment)?;
    }

    // Withdraw the matching stablecoin from the vault to the borrower's smart
    // account via the vaultprovider's in-process api. An error propagates out
    // and unwinds bookkeeping via the surrounding precompile frame. The
    // credisfactory address is the registered liquidity target the gate keys on.
    outbe_vaultprovider::api::withdraw_liquidity(
        storage.clone(),
        CREDIS_FACTORY_ADDRESS,
        asset,
        amount_stables,
        bundle_account,
    )?;

    storage.emit_event(
        CREDIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&ICredisFactory::CredisRequested {
            bundleAccount: bundle_account,
            amount: amount_stables,
        }),
    )?;

    Ok((position_id, amount_stables))
}

// ---------------------------------------------------------------------------
// pay_anadosis
// ---------------------------------------------------------------------------

/// Advances the credis position by one anadosis installment. When the final
/// installment completes the runtime inserts the position's stored reclaim
/// commitment back into the gratispool at the matching denomination so the
/// reclaim-secret holder can later `unpledgeGratis(args, destination)`.
pub fn pay_anadosis(
    storage: StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    current_time: u64,
    _current_block: u64,
) -> Result<AnadosisResult> {
    // Read-only validation pass before any mutation.
    {
        let credis_ro = CredisContract::new(storage.clone());
        let position = credis_ro.get_position(position_id)?;
        let next = credis_ro
            .get_next_anadosis(position_id)?
            .ok_or(CredisFactoryError::PositionCompleted)?;

        if position.asset.is_zero() {
            return Err(CredisFactoryError::InvalidAsset.into());
        }
        if position.vault_provider.is_zero() {
            return Err(CredisFactoryError::InvalidVaultProvider.into());
        }
        if next.anadosis_amount.is_zero() {
            return Err(CredisFactoryError::InvalidAmount.into());
        }
        if caller != position.bundle_account {
            return Err(CredisFactoryError::UnauthorizedCaller.into());
        }
    }

    let mut credis = CredisContract::new(storage.clone());
    let result = credis.make_next_anadosis(position_id, current_time)?;

    // ERC20 + vault sequence. Sub-call reverts propagate out and unwind the
    // bookkeeping via the surrounding precompile frame.
    let amount = result.anadosis_amount;
    let asset = result.asset;
    let vault = result.vault_provider;

    // 1) Pull stablecoin from caller into the credisfactory precompile address.
    let transfer = IERC20::transferFromCall {
        from: caller,
        to: CREDIS_FACTORY_ADDRESS,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, transfer.into())?;

    // 2) Approve the vault to spend that exact amount.
    let approve = IERC20::approveCall {
        spender: vault,
        amount,
    }
    .abi_encode();
    storage.call(asset, U256::ZERO, approve.into())?;

    // 3) Vault pulls and deposits into the reserve via the vaultprovider's
    //    in-process api. The credisfactory address is the registered liquidity
    //    source the gate keys on.
    outbe_vaultprovider::api::deposit_liquidity(
        storage.clone(),
        CREDIS_FACTORY_ADDRESS,
        asset,
        amount,
    )?;

    // 4) If this completed the position, append the per-position reclaim
    //    commitment to the pool so the reclaim secret can later be used to
    //    unpledge. No Gratis-ledger movement: the pledger's per-account
    //    pledged balance was set at pledge time and stays in place until
    //    they (or whoever holds the reclaim secret bound to their address)
    //    invokes `unpledgeGratis`.
    if result.anadosis_number == NUMBER_OF_ANADOSIS {
        let factory = CredisFactoryContract::new(storage.clone());
        let denom_id = factory.position_denom.read(&position_id)? as u8;
        let reclaim_commitment = factory.position_reclaim_commitment.read(&position_id)?;
        // Defensive: a zero reclaim_commitment indicates the position was
        // opened before the shielded-pool migration. Skip the pool-side
        // insertion rather than error out so legacy positions still settle
        // through the existing payAnadosis sub-call sequence.
        if reclaim_commitment != U256::ZERO {
            pool::add_commitment(storage.clone(), denom_id, reclaim_commitment)?;
        }
        // Clear the metadata — single-use per position.
        factory.position_denom.write(&position_id, 0u32)?;
        factory
            .position_reclaim_commitment
            .write(&position_id, U256::ZERO)?;
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Oracle conversion (gratis 10^18 → stablecoin 10^6)
// ---------------------------------------------------------------------------

/// Cosmos formula: `amountStables = gratisAmount * rateInt18 / (decimalsDiff * precision)`.
fn convert_gratis_to_stables(storage: StorageHandle<'_>, gratis_amount: U256) -> Result<U256> {
    let rate = get_exchange_rate(storage, NATIVE_TOKEN, STABLECOIN)?;
    let numerator = gratis_amount
        .checked_mul(rate)
        .ok_or_else(|| -> PrecompileError {
            CredisFactoryError::OracleConversionOverflow.into()
        })?;
    let denominator =
        decimals_diff()
            .checked_mul(SCALE_1E18)
            .ok_or_else(|| -> PrecompileError {
                CredisFactoryError::OracleConversionOverflow.into()
            })?;
    Ok(numerator / denominator)
}
