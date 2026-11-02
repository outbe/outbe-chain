//! Orchestration logic for the promisfactory precompile.
//!
//! Owns the promis mint/burn orchestration on top of the confidential Promis token
//! (`outbe_promis::api`). Writes are authorized by the caller's Promis modify key
//! (`mac` + `opNonce`). `mint` wraps `outbe_promis::api::mint`. `mine_coen` is the
//! symmetric sale path: it wraps `outbe_promis::api::burn`, mints native COEN 1:1,
//! and emits `CoenMined`. `mine_gratis` is the conversion path: it burns promis and
//! mints the matching Gratis through `outbe_gratisfactory::api::mint`.

use alloy_primitives::{Address, Bytes, U256};

use crate::errors::PromisFactoryError;
use crate::precompile::IPromisFactory;
use outbe_primitives::addresses::PROMIS_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::units::checked_protocol_to_native;
use outbe_promis::api::{self as promis, ModifyAuth};

/// Mint `amount` promis to `account` from a settled Gem position (`gem_id`).
///
/// Internal cross-module API (not exposed on the precompile ABI).
pub fn mint_from_gem(
    storage: StorageHandle<'_>,
    gem_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    promis::mint_from_gem(storage, gem_id, account, amount, auth)
}

/// Mint `amount` promis to `account` from a settled Intex position (`intex_id`).
///
/// Internal cross-module API (not exposed on the precompile ABI).
pub fn mint_from_intex(
    storage: StorageHandle<'_>,
    intex_id: U256,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    promis::mint_from_intex(storage, intex_id, account, amount, auth)
}

/// Burn `amount` promis from `account`, mint the matching native COEN to `account`
/// 1:1, and emit `CoenMinedFromPromis`. Returns the minted native amount. The
/// confidential burn runs inside the enclave. The caller's Promis modify key
/// (`auth`) authorizes the burn.
pub fn mine_coen(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    let native_amount =
        checked_protocol_to_native(amount).ok_or(PromisFactoryError::NativeCoenOverflow)?;

    promis::burn(storage.clone(), account, amount, auth)?;

    // PROMIS stays at six decimals. The matching native COEN exits at 18 decimals.
    storage.increase_balance(account, native_amount)?;

    storage.emit_event(
        PROMIS_FACTORY_ADDRESS,
        alloy_sol_types::SolEvent::encode_log_data(&IPromisFactory::CoenMinedFromPromis {
            account,
            amountMinor: native_amount,
        }),
    )?;

    Ok(native_amount)
}

/// Burn confidential promis (encrypted to the TEE public key) from `account` and
/// mint the matching Gratis 1:1. Both tokens are enclave-confidential and
/// independently keyed: the promis burn takes the account owner's **Promis** modify
/// key (`promis_auth`) and the gratis mint takes their **Gratis** modify key
/// (`gratis_auth`). Each `auth` binds the decrypted amount to that ledger's own
/// current op-nonce, so the caller supplies two `mac`/`opNonce` pairs.
pub fn mine_gratis(
    storage: StorageHandle<'_>,
    account: Address,
    encrypted_promis_minor: Vec<u8>,
    promis_auth: ModifyAuth,
    gratis_auth: ModifyAuth,
) -> Result<Bytes> {
    storage.with_checkpoint(|| {
        let gratis_op_nonce = gratis_auth.op_nonce;
        let (current_gratis_balance, fidelity) =
            outbe_gratisfactory::api::prepare_mine_from_promis(&storage, account, gratis_op_nonce)?;
        let result = promis::execute_mine_gratis(
            &storage,
            promis::MineGratisInput {
                caller: account,
                encrypted_promis_minor,
                promis_auth,
                current_gratis_balance,
                gratis_auth,
                fidelity,
            },
        )?;
        outbe_gratisfactory::api::commit_mine_from_promis(
            &storage,
            account,
            gratis_op_nonce,
            &result,
        )?;
        Ok(Bytes::from(result.encrypted_gratis_event_amount))
    })
}
