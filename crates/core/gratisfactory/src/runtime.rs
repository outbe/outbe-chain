//! Orchestration logic for the gratisfactory precompile.
//!
//! Bridges the confidential Gratis token (`outbe_gratis::api`) and the Fidelity
//! ledger. `pledge_gratis`/`unpledge_gratis` move gratis into/out of the credis
//! escrow; `mine`/`mine_coen` own the mint/burn plus Fidelity cohort bookkeeping.
//! The Fidelity cohort op rides INSIDE the gratis enclave round-trip (no extra
//! trip): `mine` folds an acquisition (`In`), `mine_coen` a sale (`Out`), and
//! `pledge_gratis` a read-only league `Probe` for the eligibility gate. The
//! factory persists the returned fidelity outcome. `mine_from_promis` burns
//! public promis and reuses `mine` (promis itself is fidelity-neutral).
//!
//! The credis loan is priced HERE, at pledge time: the pledger names the stablecoin
//! credit they want and this module derives the gratis it costs, sealing both plus
//! the asset and the rate into the ticket. `requestCredis` then reads that quote back
//! out instead of re-pricing the collateral a transaction later.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};

use crate::constants::PLEDGE_QUOTE_TTL_SECS;
use crate::errors::GratisFactoryError;
use crate::precompile::IGratisFactory;
use crate::schema::GratisFactoryContract;
use crate::sol_ext::IReferenceCurrency;
use outbe_fidelity::api::FidelityCohortOp;
use outbe_gratis::api::{self as gratis, ModifyAuth, PledgeTerms};
use outbe_oracle::api::fresh_coen_rate_for;
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::math::scaled_math::checked_mul_div_ceil;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::SCALE_1E6_U256;

/// Reads the pledged asset's ISO 4217 currency code via a static
/// `IReferenceCurrency.isoCode()` sub-call, mirroring credisfactory's
/// `read_iso_code`. The pledge is then priced against COEN/<that currency>
/// rather than a hardcoded pair, so it cannot be quoted in one currency while
/// the Credis position opens against another.
fn read_iso_code(storage: &StorageHandle<'_>, asset: Address) -> Result<u16> {
    let ret = storage.staticcall(
        asset,
        IReferenceCurrency::isoCodeCall {}.abi_encode().into(),
    )?;
    IReferenceCurrency::isoCodeCall::abi_decode_returns(&ret)
        .map_err(|_| GratisFactoryError::AssetIsoUndecodable.into())
}

/// Convert canonical six-decimal stablecoin raw units to six-decimal GRATIS,
/// rounded **up** so the pledged collateral always covers the requested credit.
/// Gratis is priced at the COEN price because `mine_coen` converts the two 1:1.
/// Returns `(gratis_cost, rate)`.
fn convert_stables_to_gratis(
    storage: StorageHandle<'_>,
    amount_stables: U256,
    asset: Address,
) -> Result<(U256, U256)> {
    let iso_code = read_iso_code(&storage, asset)?;
    let rate = fresh_coen_rate_for(storage, iso_code)?;
    let gratis = checked_mul_div_ceil(amount_stables, SCALE_1E6_U256, rate).map_err(|_| {
        let error: PrecompileError = GratisFactoryError::OracleConversionOverflow.into();
        error
    })?;
    Ok((gratis, rate))
}

/// Pledge the gratis that collateralizes `amount_stables` of credit in `asset` into a
/// pending pledge-lock ticket (authorized by the caller's modify key, which binds the
/// STABLES figure). The gratis cost is derived from the oracle rate and rejected if it
/// exceeds `max_gratis` — that cap is the pledger's slippage protection, authenticated
/// by their transaction signature rather than the MAC. Returns
/// `(pledge_handle, gratis_cost)`; the handle is what the CCA presents at
/// `requestCredis`. The loan's own terms — the policy rate, the floor and call prices —
/// are sealed on the Credis position, not on the pledge.
pub fn pledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    asset: Address,
    max_gratis: U256,
    auth: ModifyAuth,
) -> Result<(B256, U256)> {
    if asset.is_zero() {
        return Err(GratisFactoryError::InvalidAsset.into());
    }
    if amount_stables.is_zero() {
        return Err(GratisFactoryError::InvalidAmount.into());
    }

    let (gratis_amount, entry_rate) =
        convert_stables_to_gratis(storage.clone(), amount_stables, asset)?;
    if gratis_amount > max_gratis {
        return Err(GratisFactoryError::GratisCapExceeded.into());
    }
    let terms = PledgeTerms {
        stables_amount: amount_stables,
        gratis_amount,
        asset,
        entry_rate,
    };

    let now = storage.timestamp()?.to::<u64>();

    // The collateral debit and the vault claim must stand or fall together: the
    // reservation is keyed by the pledge handle, so it cannot be taken before the
    // ticket exists, and a debited pledger holding no claim is the exact failure
    // this guarantees against. The precompile frame would revert anyway; owning the
    // atomicity here means any cross-module caller gets it too.
    storage.with_checkpoint(|| {
        // Fold a read-only league probe into the pledge round-trip (no separate
        // fidelity call): the pledge op returns the caller's current league.
        let section = outbe_fidelity::api::cohort_section(
            storage.clone(),
            caller,
            FidelityCohortOp::Probe,
            now,
        )?;
        let (handle, outcome) = gratis::pledge_with_fidelity(
            storage.clone(),
            caller,
            amount_stables,
            terms,
            auth,
            section,
        )?;
        // todo implement correct fidelity eligibility check on `outcome.league`
        if outcome.league == u16::MAX {
            return Err(GratisFactoryError::FidelityNotEligible.into());
        }

        // Claim the credit out of the reserve vault into router custody, wired to
        // this handle. A liquidity *check* here would not survive the gap to
        // `requestCredis` — another withdrawal can take the same shares in between
        // — so the pledge holds the assets rather than merely testing for them.
        // This also subsumes the dry-vault rejection: `reserve` fails on the same
        // shortfall a check would.
        outbe_vaultrouter::api::reserve(&storage, handle, asset, amount_stables)?;

        // Stamp the quote and queue the handle for expiry. The same timestamp is
        // the reservation's deadline, which is why the queue stores no time of its
        // own.
        let contract = GratisFactoryContract::new(storage.clone());
        contract.pledge_quoted_at.write(&handle, now)?;
        contract.pledge_queue.push_back(handle)?;
        Ok((handle, gratis_amount))
    })
}

/// Directly unpledge an unspent pledge back to `caller` (e.g. credis rejected).
/// `amount_stables` is the figure the pledge was quoted for; returns the gratis
/// collateral credited back.
pub fn unpledge_gratis(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    pledge_handle: B256,
    auth: ModifyAuth,
) -> Result<U256> {
    let refunded = gratis::unpledge(storage.clone(), caller, amount_stables, pledge_handle, auth)?;
    // The credit this pledge claimed is no longer owed to anyone, so it goes back
    // to the vault to earn rather than sitting idle until the sweep notices.
    // `returnReservation` is idempotent, so a quote the sweep already unwound still
    // unpledges cleanly — otherwise a lapsed pledger could never reclaim their
    // collateral, which is the one call left to them.
    outbe_vaultrouter::api::return_reservation(&storage, pledge_handle)?;
    // The ticket is gone; its quote can never be exercised again. The queue entry
    // is left behind as a tombstone — the sweep pops it on sight.
    GratisFactoryContract::new(storage)
        .pledge_quoted_at
        .clear(&pledge_handle)?;
    Ok(refunded)
}

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort. The `GratisMinted` event is
/// emitted by the Gratis token; the factory `GratisMined` is emitted here.
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    // Fold the acquisition cohort into the gratis mint round-trip; persist the
    // returned fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::In, now)?;
    let outcome = gratis::mint_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;
    Ok(())
}

/// Burn `amount` confidential promis from `account` and mint the matching Gratis
/// 1:1. Both tokens are enclave-confidential and independently authorized: the
/// promis burn takes the account owner's **Promis** modify key (`promis_auth`) and
/// the gratis mint takes their **Gratis** modify key (`gratis_auth`). Each `auth`
/// binds `amount` to that ledger's own current op-nonce, so the caller supplies two
/// `mac`/`opNonce` pairs.
pub fn mine_from_promis(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    gratis_auth: ModifyAuth,
    promis_auth: ModifyAuth,
) -> Result<U256> {
    outbe_promis::api::burn(storage.clone(), account, amount, promis_auth)?;

    // Reuse `mint`: gratis mint + fresh Fidelity cohort at the current block time.
    mint(storage, account, amount, gratis_auth)?;

    Ok(amount)
}

pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    // Fold the sale cohort into the gratis burn round-trip; persist the returned
    // fidelity blob.
    let now = storage.timestamp()?.to::<u64>();
    let section =
        outbe_fidelity::api::cohort_section(storage.clone(), account, FidelityCohortOp::Out, now)?;
    let outcome = gratis::burn_with_fidelity(storage.clone(), account, amount, auth, section)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)?;

    // Mint native COEN to the seller 1:1 against the burned gratis.
    storage.increase_balance(account, amount)?;

    storage.emit_event(
        GRATIS_FACTORY_ADDRESS,
        SolEvent::encode_log_data(&IGratisFactory::CoenMined {
            sender: account,
            amount,
        }),
    )?;

    Ok(amount)
}

// ---------------------------------------------------------------------------
// pledge-quote expiry
// ---------------------------------------------------------------------------
//
// `pledge_gratis` claims the credit out of the reserve vault the moment the quote
// is struck, so an unspent quote is parked vault liquidity. The sweep walks the
// pledge queue and gives it back.
//
// The queue needs no sorting and no per-entry deadline: `PLEDGE_QUOTE_TTL_SECS` is
// a constant, so handles expire in exactly the order they were pledged. The head is
// therefore always the next thing to expire, and a head that is not yet due ends the
// run — the common case costs two storage reads.

/// Reservations unwound per run. A pledge costs its author real collateral, so the
/// queue cannot be cheaply flooded; this is a block-weight bound, not a defence.
pub const MAX_PLEDGE_EXPIRY_SWEEPS: u32 = 256;

/// Returns expired reservations to their vaults, up to `max` of them. Returns how
/// many were actually unwound (tombstones are not counted).
pub fn sweep_expired(storage: &StorageHandle<'_>, max: u32) -> Result<u32> {
    let now = storage.timestamp()?.to::<u64>();
    let mut contract = GratisFactoryContract::new(storage.clone());
    let mut swept = 0u32;

    for _ in 0..max {
        let Some(handle) = contract.pledge_queue.front()? else {
            break;
        };

        let quoted_at = contract.pledge_quoted_at.read(&handle)?;
        if quoted_at == 0 {
            // Spent at `requestCredis` or unpledged: the reservation is already
            // gone and only the queue slot is left. Drop it and keep walking —
            // this is not expiry work, so it does not consume the budget's intent.
            contract.pledge_queue.pop_front()?;
            continue;
        }
        if now <= quoted_at.saturating_add(PLEDGE_QUOTE_TTL_SECS) {
            // Insertion order is expiry order, so nothing behind the head is due.
            break;
        }

        // Isolate each handle: one wedged reservation (a vault that reverts on
        // deposit, say) must not strand every later pledge behind it.
        let unwound = storage.with_checkpoint(|| {
            outbe_vaultrouter::api::return_reservation(storage, handle)?;
            contract.pledge_quoted_at.clear(&handle)?;
            contract.emit(IGratisFactory::PledgeQuoteExpired {
                pledgeHandle: handle,
                quotedAt: quoted_at,
            })
        });

        // Pop regardless: a handle that cannot be unwound would otherwise block the
        // head forever. Its assets stay in router custody and stay recoverable
        // through the permissionless `returnReservation`.
        contract.pledge_queue.pop_front()?;
        match unwound {
            Ok(()) => swept = swept.saturating_add(1),
            Err(error) => tracing::warn!(
                target: "outbe::gratisfactory",
                %handle,
                %error,
                "expired pledge reservation could not be returned to its vault"
            ),
        }
    }

    Ok(swept)
}

// todo Recovering the pledger's GRATIS is still a manual step. This sweep returns
// the stablecoin claim to the vault and clears the quote, but the collateral stays
// parked in the enclave-sealed pledge ticket. The pledger must call
// `unpledgeGratis(amountStables, pledgeHandle, mac, opNonce)` themselves to get it
// back; `PledgeQuoteExpired` is emitted here so a client can detect the deadline and
// prompt them.
//
// Automating it needs a new unauthenticated `GratisOp::ExpirePledge`: `Unpledge` is
// an owner op gated on a MAC derived from a modify key that never leaves the enclave
// (`bin/outbe-tee-enclave/src/gratis.rs`), so the host cannot synthesize one. That is
// a postcard wire change, an `inputs_canonical_hash` extension, host and enclave
// rolled in lockstep, and a new MRENCLAVE.
