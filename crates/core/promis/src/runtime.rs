//! Business logic for the confidential Promis token.
//!
//! Each write does these steps:
//!
//! 1. Read the account's current ciphertext from storage.
//! 2. Hand the op to the enclave. The enclave decrypts, enforces invariants, and
//!    re-encrypts deterministically.
//! 3. Store the returned ciphertext verbatim.
//! 4. Emit the matching event.
//!
//! These methods are crate-private. Other crates reach them through [`crate::api`].
//! The enclave is the sole party that sees plaintext balances (Enclave Return Rule).

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::PROMIS_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityOpSection, MineGratisFromPromisRequestV1, MineGratisFromPromisResultV1,
    MineGratisFromPromisStatusV1, ModifyAuth, PromisOp, PromisOpRequest, PromisOpResult,
    PromisOpStatus,
};

use crate::enclave_client::{apply_promis_op, mine_gratis_from_promis};
use crate::errors::{PromisError, PromisFatalError};
use crate::precompile::IPromis;
use crate::schema::Promis;

/// The chain id the enclave binds a modify-auth to, as a `B256` (host and client
/// must agree on this encoding). Defense-in-depth: the modify key is already
/// chain-bound via the DKG state key.
fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

/// Build a request with the fields common to every op left at their empty defaults.
fn base_request(op: PromisOp, chain_id: B256, account: Address, amount: U256) -> PromisOpRequest {
    PromisOpRequest {
        op,
        chain_id,
        account,
        amount,
        current_balance: Vec::new(),
        modify_auth: ModifyAuth {
            mac: [0u8; 32],
            op_nonce: 0,
        },
    }
}

/// Reject unless the supplied op-nonce equals the account's current on-chain
/// counter. This check makes a captured modify-auth non-replayable.
fn check_op_nonce(promis: &Promis<'_>, account: Address, provided: u64) -> Result<()> {
    let current = promis.op_nonce_of(account)?;
    if provided != current {
        return Err(PromisError::InvalidOpNonce {
            expected: current,
            provided,
        }
        .into());
    }
    Ok(())
}

/// Turn a business rejection from the enclave into a precompile revert.
fn ensure_applied(result: &PromisOpResult) -> Result<()> {
    match &result.status {
        PromisOpStatus::Applied => Ok(()),
        PromisOpStatus::Rejected { reason } => Err(PromisError::Rejected(reason.clone()).into()),
    }
}

/// The enclave moved exactly the requested amount of the balance and advanced the nonce by one.
pub(crate) fn ensure_owner_result(
    amount: U256,
    op_nonce: u64,
    result: &PromisOpResult,
) -> Result<()> {
    if result.event_amount != amount
        || result.new_balance.is_empty()
        || result.encrypted_event_amount.is_empty()
    {
        return Err(PromisFatalError::OwnerResultMismatch.into());
    }
    if op_nonce.checked_add(1) != Some(result.next_op_nonce) {
        return Err(PromisFatalError::WrongOpNonce.into());
    }
    Ok(())
}

/// Store the balance ciphertext the enclave produced (an empty blob means the op
/// did not touch the slot).
fn write_balance(promis: &Promis<'_>, account: Address, result: &PromisOpResult) -> Result<()> {
    if !result.new_balance.is_empty() {
        promis.write_balance_ct(account, &result.new_balance)?;
    }
    Ok(())
}

/// Source entity that triggered a Promis mint.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MintSource {
    Gem(U256),
    Intex(U256),
}

/// Mint `amount` promis to `caller` from `source` (`Gem` or `Intex`).
pub(crate) fn mint(
    storage: StorageHandle<'_>,
    source: MintSource,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    let promis = Promis::new(storage.clone());
    let op_nonce = auth.op_nonce;
    check_op_nonce(&promis, caller, op_nonce)?;
    let mut req = base_request(PromisOp::Mint, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = promis.balance_ct_of(caller)?;
    req.modify_auth = auth;
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    let result = apply_promis_op(req)?;
    ensure_applied(&result)?;
    ensure_owner_result(amount, op_nonce, &result)?;
    write_balance(&promis, caller, &result)?;
    promis.set_op_nonce(caller, result.next_op_nonce)?;
    let encrypted_amount_minor = result.encrypted_event_amount.into();
    let event = match source {
        MintSource::Gem(gem_id) => SolEvent::encode_log_data(&IPromis::PromisMinedFromGem {
            gemId: gem_id,
            account: caller,
            encryptedAmountMinor: encrypted_amount_minor,
        }),
        MintSource::Intex(intex_id) => SolEvent::encode_log_data(&IPromis::PromisMinedFromIntex {
            intexId: intex_id,
            account: caller,
            encryptedAmountMinor: encrypted_amount_minor,
        }),
    };
    storage.emit_event(PROMIS_ADDRESS, event)
}

/// Burn `amount` promis from `caller` (owner-authorized).
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    let promis = Promis::new(storage.clone());
    let op_nonce = auth.op_nonce;
    check_op_nonce(&promis, caller, op_nonce)?;
    let mut req = base_request(PromisOp::Burn, chain_id_b256(&storage)?, caller, amount);
    req.current_balance = promis.balance_ct_of(caller)?;
    req.modify_auth = auth;
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&storage)?;
    let result = apply_promis_op(req)?;
    ensure_applied(&result)?;
    ensure_owner_result(amount, op_nonce, &result)?;
    write_balance(&promis, caller, &result)?;
    promis.set_op_nonce(caller, result.next_op_nonce)?;
    storage.emit_event(
        PROMIS_ADDRESS,
        SolEvent::encode_log_data(&IPromis::PromisBurned {
            account: caller,
            encryptedAmountMinor: result.encrypted_event_amount.into(),
        }),
    )
}

/// Inputs for [`execute_mine_gratis`].
pub struct MineGratisInput {
    pub caller: Address,
    pub encrypted_promis_minor: Vec<u8>,
    pub promis_auth: ModifyAuth,
    pub current_gratis_balance: Vec<u8>,
    pub gratis_auth: ModifyAuth,
    pub fidelity: FidelityOpSection,
}

/// Execute the combined Promis->Gratis conversion in the enclave, persist the
/// Promis burn state + `PromisBurned` event, and return the enclave result so the
/// caller can persist the Gratis + Fidelity halves.
pub(crate) fn execute_mine_gratis(
    storage: &StorageHandle<'_>,
    input: MineGratisInput,
) -> Result<MineGratisFromPromisResultV1> {
    if input.encrypted_promis_minor.len() != outbe_tee::offer_encrypt::ENCRYPTED_TEE_AMOUNT_LEN {
        return Err(PromisError::Rejected("decrypt failed".into()).into());
    }
    let promis = Promis::new(storage.clone());
    let promis_nonce = input.promis_auth.op_nonce;
    check_op_nonce(&promis, input.caller, promis_nonce)?;
    let req = MineGratisFromPromisRequestV1 {
        chain_id: chain_id_b256(storage)?,
        account: input.caller,
        encrypted_promis_amount: input.encrypted_promis_minor,
        current_promis_balance: promis.balance_ct_of(input.caller)?,
        promis_modify_auth: input.promis_auth,
        current_gratis_balance: input.current_gratis_balance,
        gratis_modify_auth: input.gratis_auth,
        fidelity: input.fidelity,
    };
    let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(storage)?;
    let result = mine_gratis_from_promis(req)?;
    match &result.status {
        MineGratisFromPromisStatusV1::Applied => {}
        MineGratisFromPromisStatusV1::PromisRejected { reason }
        | MineGratisFromPromisStatusV1::GratisRejected { reason } => {
            return Err(PromisError::Rejected(reason.clone()).into());
        }
    }
    if result.new_promis_balance.is_empty() || result.encrypted_promis_event_amount.is_empty() {
        return Err(PromisFatalError::OwnerResultMismatch.into());
    }
    if promis_nonce.checked_add(1) != Some(result.next_promis_op_nonce) {
        return Err(PromisFatalError::WrongOpNonce.into());
    }
    promis.write_balance_ct(input.caller, &result.new_promis_balance)?;
    promis.set_op_nonce(input.caller, result.next_promis_op_nonce)?;
    storage.emit_event(
        PROMIS_ADDRESS,
        SolEvent::encode_log_data(&IPromis::PromisBurned {
            account: input.caller,
            encryptedAmountMinor: result.encrypted_promis_event_amount.clone().into(),
        }),
    )?;
    Ok(result)
}
