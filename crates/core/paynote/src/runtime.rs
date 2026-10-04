//! PayNote transition core — the state machine shared by dispatch, the
//! cross-module API, and tests.
//!
//! Every mutating path runs under [`StorageHandle::with_checkpoint`], making
//! tree, replay, token, and event effects one rollback unit. All guards precede
//! mutation. Guard failures convert from [`PayNoteError`] (which fixes the
//! revert texts and the fatal/revert split) via `From`.
//! Canonical field words are exactly 32 big-endian bytes.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use ark_ff::Zero;
use outbe_primitives::addresses::{PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_protocol::codec::{field_from_b256, field_to_b256};
use outbe_zk_backend::barretenberg::verify_circuit;
use outbe_zk_canonical::noir::paynote::Paynote;
use outbe_zk_canonical::paynote::{
    alloy::PublicInputs as PayNotePublicInputs,
    decode_public_inputs as decode_paynote_public_inputs,
};
use outbe_zk_canonical::paynote_merge::{self, PaynoteMerge, MAX_MERGE_INPUTS};

use crate::errors::PayNoteError;
use crate::hash::{empty_subtrees, merkle_node, note_commitment};
use crate::precompile::IPayNote;
use crate::schema::{
    PayNoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_CAPACITY, PAYNOTE_TREE_DEPTH,
};
use crate::sol_ext::IERC20;
use crate::Field;

/// The validated public claim a spend proof carries, returned to the consuming
/// module. PayNote books the nullifier and any change note; deciding what the
/// released value buys is the caller's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayNoteClaim {
    pub asset: Address,
    /// Opaque settlement statement. The caller recomputes it; PayNote does not.
    pub context: B256,
    pub spend_amount: U256,
    /// The canonical nullifier this spend booked. It is the only public
    /// identifier of the payment, so a consuming module can record which note
    /// paid it without learning anything that links back to the depositor.
    pub nullifier: B256,
}

/// The circuit capacity fits u32 (currently four); checked to keep ABI and
/// generated witness changes from silently truncating the published bound.
pub(crate) fn max_merge_inputs() -> Result<u32> {
    u32::try_from(MAX_MERGE_INPUTS)
        .map_err(|_| PrecompileError::Fatal("PayNote merge capacity exceeds u32".into()))
}

/// Proof-authorized consolidation. Shares settlement's canonical nullifiers,
/// tree and checkpoint; touches no token, Reserve, Oracle or right state.
pub(crate) fn merge_pay_notes(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<()> {
    let claim: paynote_merge::alloy::PublicInputs = paynote_merge::decode_public_inputs(proof)
        .and_then(TryInto::try_into)
        .map_err(|error| {
            PayNoteError::InvalidInput(format!("merge proof is malformed: {error}"))
        })?;
    if claim.chain_id != storage.chain_id()? || claim.pool != PAYNOTE_ADDRESS {
        return Err(PayNoteError::InvalidInput(
            "merge chain or pool does not match runtime".into(),
        )
        .into());
    }
    if !(2..=max_merge_inputs()?).contains(&claim.input_count) {
        return Err(PayNoteError::InvalidInput(
            "merge input count is outside supported bounds".into(),
        )
        .into());
    }
    let count = usize::try_from(claim.input_count)
        .map_err(|_| PayNoteError::InvalidInput("merge input count exceeds usize".into()))?;
    let (active, padding) = claim.nullifiers.split_at(count);
    if claim.asset.is_zero()
        || claim.output_commitment.is_zero()
        || padding.iter().any(|word| !word.is_zero())
        || active
            .iter()
            .enumerate()
            .any(|(i, word)| word.is_zero() || active[..i].contains(word))
    {
        return Err(PayNoteError::InvalidInput(
            "merge has zero or duplicate inputs, output, asset, or nonzero padding".into(),
        )
        .into());
    }
    let paynote: PayNoteContract<'_> = storage.contract();
    let leaf_count = paynote.leaf_count.read()?;
    if leaf_count == 0 {
        return Err(PayNoteError::NotInitialized.into());
    }
    if leaf_count >= PAYNOTE_TREE_CAPACITY {
        return Err(PayNoteError::TreeFull.into());
    }
    if !paynote.recent_roots.read_all()?.contains(&claim.root) {
        return Err(PayNoteError::RootNotRecent.into());
    }
    for nullifier in active {
        if paynote.spent_nullifiers.read(nullifier)? {
            return Err(PayNoteError::NullifierSpent.into());
        }
    }
    if paynote.commitments.read(&claim.output_commitment)? {
        return Err(PayNoteError::CommitmentExists.into());
    }
    match verify_circuit::<PaynoteMerge>(proof) {
        Ok(true) => {}
        Ok(false) => return Err(PayNoteError::InvalidInput("merge proof is invalid".into()).into()),
        Err(error) => {
            return Err(PayNoteError::InvalidInput(format!(
                "merge proof verification failed: {error}"
            ))
            .into())
        }
    }
    // The canonical decoder and circuit prove the confidential U256 sum;
    // runtime never receives amounts and must not reconstruct them from logs.
    let output = field_from_b256(&claim.output_commitment)
        .map_err(|_| PayNoteError::InvalidInput("merge output is not a canonical field".into()))?;
    let (_, zeros) = chain_state(storage)?;
    storage.with_checkpoint(|| {
        for nullifier in active {
            paynote.spent_nullifiers.write(nullifier, true)?;
        }
        let (index, root_after) = append(&paynote, &zeros, output)?;
        paynote.commitments.write(&claim.output_commitment, true)?;
        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPayNote::NotesMerged::encode_log_data(&IPayNote::NotesMerged {
                asset: claim.asset,
                outputCommitment: claim.output_commitment,
                nullifiers: active.to_vec(),
            }),
        )?;
        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPayNote::NewNote::encode_log_data(&IPayNote::NewNote {
                commitment: claim.output_commitment,
                leafIndex: index,
                rootAfter: field_to_b256(&root_after)
                    .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
                asset: claim.asset,
                noteAmount: U256::ZERO,
            }),
        )?;
        Ok(())
    })
}

/// Reads the live chain ID and derives its full in-memory empty ladder.
fn chain_state(storage: &StorageHandle<'_>) -> Result<(u64, Vec<Field>)> {
    let chain_id = storage.chain_id()?;
    let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).map_err(|_| PayNoteError::Hash)?;
    Ok((chain_id, zeros))
}

/// Appends `leaf` in O(depth) stored state using the Tornado Cash pattern: for
/// each level, the corresponding `leaf_count` bit decides whether the current
/// node completes a left subtree (store it in `filled_subtrees` and combine
/// with the empty subtree) or joins the stored left subtree (combine as the
/// right node). Finishes with one root/count/root-buffer update and returns
/// `(leaf_index, root_after)`.
///
/// `index` is bounded by [`PAYNOTE_TREE_CAPACITY`] at every call site, so the
/// `u32` narrowing for the returned leaf index cannot truncate.
pub(crate) fn append(
    paynote: &PayNoteContract<'_>,
    zeros: &[Field],
    leaf: Field,
) -> Result<(u32, Field)> {
    let index = paynote.leaf_count.read()?;
    let mut current = leaf;
    for (level, zero) in zeros.iter().enumerate().take(PAYNOTE_TREE_DEPTH) {
        let level_byte = u8::try_from(level).map_err(|_| PayNoteError::CorruptFrontier)?;
        if (index >> level) & 1 == 0 {
            paynote.filled_subtrees.write(
                &level_byte,
                field_to_b256(&current)
                    .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
            )?;
            current = merkle_node(current, *zero).map_err(|_| PayNoteError::Hash)?;
        } else {
            let left = paynote.filled_subtrees.read(&level_byte)?;
            let left = field_from_b256(&left).map_err(|_| PayNoteError::CorruptFrontier)?;
            current = merkle_node(left, current).map_err(|_| PayNoteError::Hash)?;
        }
    }
    let root_after =
        field_to_b256(&current).map_err(|error| PrecompileError::Fatal(error.to_string()))?;
    paynote.current_root.write(root_after)?;
    paynote.leaf_count.write(index + 1)?;
    paynote.recent_roots.push(root_after)?;
    let leaf_index = u32::try_from(index).map_err(|_| PayNoteError::TreeFull)?;
    Ok((leaf_index, current))
}

/// `deposit(asset, amount, noteSn)` — pull the ERC20, route it into the
/// asset's reserve vault, and append the derived note commitment.
pub(crate) fn deposit(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: U256,
    note_sn: B256,
) -> Result<()> {
    // Guards, before any mutation.
    if amount.is_zero() {
        return Err(PayNoteError::InvalidInput("deposit amount must be non-zero".into()).into());
    }
    // `asset != 0` is enforced here such as we do not accept native currency here.
    if asset.is_zero() {
        return Err(PayNoteError::InvalidInput("asset must be non-zero".into()).into());
    }
    let serial = field_from_b256(&note_sn)
        .map_err(|_| PayNoteError::InvalidInput("noteSn is not a canonical BN254 field".into()))?;
    if serial.is_zero() {
        return Err(PayNoteError::InvalidInput("noteSn must be non-zero".into()).into());
    }

    let (chain_id, zeros) = chain_state(&storage)?;
    let paynote: PayNoteContract<'_> = storage.contract();
    let leaf_count = paynote.leaf_count.read()?;
    if leaf_count >= PAYNOTE_TREE_CAPACITY {
        return Err(PayNoteError::TreeFull.into());
    }

    // The commitment is always derived from the asset and amount this call
    // actually moves — never caller-supplied — so Merkle membership attests
    // both. A caller-chosen leaf would let a depositor fund a note in a cheap
    // token and spend it as an expensive one.
    let commitment =
        note_commitment(chain_id, serial, asset, amount).map_err(|_| PayNoteError::Hash)?;
    if commitment.is_zero() {
        return Err(PayNoteError::InvalidInput("commitment must be non-zero".into()).into());
    }
    let commitment_word =
        field_to_b256(&commitment).map_err(|error| PrecompileError::Fatal(error.to_string()))?;
    if paynote.commitments.read(&commitment_word)? {
        return Err(PayNoteError::CommitmentExists.into());
    }

    // One rollback unit: token movement, lazy initialization, append,
    // commitment insert, NewNote. `leaf_count == 0` is the pristine state —
    // initialization and the first append are atomic, so an active tree never
    // observes `leaf_count == 0`.
    storage.with_checkpoint(|| {
        let units = amount;
        // Pull into the pool, then let the router pull from the pool: the
        // router's `deposit` is a `transferFrom(caller, SELF)`, so the pool
        // must both hold the tokens and approve the router.
        let before = token_balance(&storage, asset)?;
        checked_token_call(
            &storage,
            asset,
            IERC20::transferFromCall {
                from: caller,
                to: PAYNOTE_ADDRESS,
                amount: units,
            },
        )?;
        if token_balance(&storage, asset)?.checked_sub(before) != Some(units) {
            return Err(PayNoteError::DepositAmountMismatch.into());
        }
        checked_token_call(
            &storage,
            asset,
            IERC20::approveCall {
                spender: VAULT_ROUTER_ADDRESS,
                amount: units,
            },
        )?;
        outbe_vaultrouter::api::deposit(&storage, asset, units)?;
        if token_balance(&storage, asset)? != before {
            return Err(PayNoteError::DepositAmountMismatch.into());
        }

        if leaf_count == 0 {
            let empty_root = field_to_b256(&zeros[PAYNOTE_TREE_DEPTH])
                .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
            paynote.current_root.write(empty_root)?;
            paynote.recent_roots.setup(PAYNOTE_ROOT_WINDOW)?;
            paynote.recent_roots.push(empty_root)?;
        }
        let (index, root_after) = append(&paynote, &zeros, commitment)?;
        paynote.commitments.write(&commitment_word, true)?;

        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPayNote::NewNote::encode_log_data(&IPayNote::NewNote {
                commitment: commitment_word,
                leafIndex: index,
                rootAfter: field_to_b256(&root_after)
                    .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
                asset,
                noteAmount: amount,
            }),
        )?;
        Ok(())
    })
}

/// An empty return counts as success: some tokens return nothing.
fn checked_token_call(
    storage: &StorageHandle<'_>,
    asset: Address,
    call: impl SolCall,
) -> Result<()> {
    let ret = storage.call(asset, U256::ZERO, call.abi_encode().into())?;
    if !ret.is_empty() && ret.as_ref() != U256::ONE.to_be_bytes::<32>() {
        return Err(PayNoteError::TokenOperationFailed.into());
    }
    Ok(())
}

fn token_balance(storage: &StorageHandle<'_>, asset: Address) -> Result<U256> {
    let ret = storage.staticcall(
        asset,
        IERC20::balanceOfCall {
            account: PAYNOTE_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    IERC20::balanceOfCall::abi_decode_returns(&ret)
        .map_err(|_| PayNoteError::TokenOperationFailed.into())
}

/// `consume(proof)` — verify a frozen `outbe.paynote@1.3.0` spend proof,
/// nullify the note, append any change commitment, and return the validated
/// claim. Moves no tokens.
///
/// The proof is the single source of truth for the statement it carries.
/// `context` is an opaque public word; the caller recomputes the settlement
/// statement and compares it. PayNote does not interpret the word.
///
/// Notes are bearer instruments — spend authority is knowledge of the spend
/// key, not an address — so there is deliberately no caller check. Any address
/// may relay a proof. The nullifier does not include `context`, so every
/// statement for one note shares one use.
pub(crate) fn consume(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<PayNoteClaim> {
    // Framing must decode before any state is touched.
    let claim: PayNotePublicInputs = decode_paynote_public_inputs(proof)
        .and_then(TryInto::try_into)
        .map_err(|error| PayNoteError::InvalidInput(format!("proof is malformed: {error}")))?;

    let (runtime_chain_id, zeros) = chain_state(storage)?;
    let paynote: PayNoteContract<'_> = storage.contract();
    if paynote.leaf_count.read()? == 0 {
        return Err(PayNoteError::NotInitialized.into());
    }

    // A proof-supplied chain ID is never trusted.
    if claim.chain_id != runtime_chain_id {
        return Err(PayNoteError::InvalidInput("chain ID does not match runtime".into()).into());
    }

    // `asset != 0` is enforced here such as we do not accept native currency here.
    if claim.asset.is_zero() {
        return Err(PayNoteError::InvalidInput("asset must be non-zero".into()).into());
    }
    if claim.context == B256::ZERO {
        return Err(PayNoteError::InvalidInput("context must be non-zero".into()).into());
    }
    if claim.spend_amount.is_zero() {
        return Err(PayNoteError::InvalidInput("spend_amount must be non-zero".into()).into());
    }

    let nullifier = field_from_b256(&claim.nullifier).map_err(|_| {
        PayNoteError::InvalidInput("nullifier is not a canonical BN254 field".into())
    })?;
    if nullifier.is_zero() {
        return Err(PayNoteError::InvalidInput("nullifier must be non-zero".into()).into());
    }
    let change = field_from_b256(&claim.change_commitment).map_err(|_| {
        PayNoteError::InvalidInput("changeCommitment is not a canonical BN254 field".into())
    })?;

    let root_word = claim.root;
    if !paynote.recent_roots.read_all()?.contains(&root_word) {
        return Err(PayNoteError::RootNotRecent.into());
    }

    let nullifier_word = claim.nullifier;
    if paynote.spent_nullifiers.read(&nullifier_word)? {
        return Err(PayNoteError::NullifierSpent.into());
    }

    match verify_circuit::<Paynote>(proof) {
        Ok(true) => {}
        Ok(false) => return Err(PayNoteError::InvalidInput("proof is invalid".into()).into()),
        Err(error) => {
            return Err(PayNoteError::InvalidInput(format!(
                "proof is malformed: zk verification backend failed: {error}"
            ))
            .into())
        }
    }

    // A full spend requires the zero change sentinel; a partial spend appends
    // exactly the circuit-derived deterministic change.
    let partial = !change.is_zero();
    let change_word = claim.change_commitment;
    if partial {
        if paynote.leaf_count.read()? >= PAYNOTE_TREE_CAPACITY {
            return Err(PayNoteError::TreeFull.into());
        }
        // Anyone knowing the current key can pre-create the deterministic
        // change; the resulting duplicate reverts atomically. Accepted DoS
        // exposure — never a fallback to spending without recording change.
        if paynote.commitments.read(&change_word)? {
            return Err(PayNoteError::CommitmentExists.into());
        }
    }

    // One rollback unit: nullifier, optional change append, events.
    storage.with_checkpoint(|| {
        paynote.spent_nullifiers.write(&nullifier_word, true)?;
        let change_receipt = if partial {
            let (index, root_after) = append(&paynote, &zeros, change)?;
            paynote.commitments.write(&change_word, true)?;
            Some((index, root_after))
        } else {
            None
        };
        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPayNote::NoteUsed::encode_log_data(&IPayNote::NoteUsed {
                asset: claim.asset,
                context: claim.context,
                nullifier: nullifier_word,
                amountMinor: claim.spend_amount,
            }),
        )?;
        if let Some((index, root_after)) = change_receipt {
            storage.emit_event(
                PAYNOTE_ADDRESS,
                IPayNote::NewNote::encode_log_data(&IPayNote::NewNote {
                    commitment: change_word,
                    leafIndex: index,
                    rootAfter: field_to_b256(&root_after)
                        .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
                    asset: claim.asset,
                    // Sentinel: a change note's remaining value is private.
                    noteAmount: U256::ZERO,
                }),
            )?;
        }
        Ok(())
    })?;

    Ok(PayNoteClaim {
        asset: claim.asset,
        context: claim.context,
        spend_amount: claim.spend_amount,
        nullifier: nullifier_word,
    })
}
