//! Expiry sweep for pledge-time vault reservations.
//!
//! `pledge_gratis` claims the credit out of the reserve vault the moment the quote
//! is struck, so an unspent quote is parked vault liquidity. This walks the pledge
//! queue and gives it back.
//!
//! The queue needs no sorting and no per-entry deadline: `PLEDGE_QUOTE_TTL_SECS` is
//! a constant, so handles expire in exactly the order they were pledged. The head is
//! therefore always the next thing to expire, and a head that is not yet due ends the
//! run — the common case costs two storage reads.
//!
//! This does NOT run as a begin-block hook. Returning assets to the vault is an
//! `IVaultV2.deposit` sub-call, and the block-hook provider
//! (`DirectStorageProvider`) does not implement `StorageProvider::sub_call` — the
//! trait default rejects with `NotAvailable`. It runs from the Cycle trigger
//! instead, which dispatches inside the `CycleTick` system transaction and so has a
//! real EVM frame.

use outbe_credis::constants::PLEDGE_QUOTE_TTL_SECS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::precompile::IGratisFactory;
use crate::schema::GratisFactoryContract;

/// Reservations unwound per run. A pledge costs its author real collateral, so the
/// queue cannot be cheaply flooded; this is a block-weight bound, not a defence.
pub const MAX_PLEDGE_EXPIRY_SWEEPS: u32 = 256;

/// Cycle trigger entry point. Total by construction — a handler that returns `Err`
/// propagates out of `dispatch_triggers` and fails the block.
pub fn run_sweep(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_expired(&ctx.storage, MAX_PLEDGE_EXPIRY_SWEEPS)?;
    Ok(())
}

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
