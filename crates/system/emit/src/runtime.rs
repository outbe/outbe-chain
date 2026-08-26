//! Emit transition core — the single state machine shared by dispatch and
//! tests.
//!
//! Every mutating path runs under [`StorageHandle::with_checkpoint`], making
//! tree, replay, balance, and event effects one rollback unit. All guards
//! precede mutation, in the frozen transition-matrix order. Guard failures
//! convert from [`EmitError`] (which fixes the stable revert texts and the
//! fatal/revert split) via `From`.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use ark_ff::Zero;
use outbe_primitives::addresses::EMIT_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_zkproof::{decode_emit_mint_public_inputs, verify_emit_mint, ZkProofError};

use crate::errors::EmitError;
use crate::hash::{
    empty_subtrees, field_from_be_bytes, field_to_be_bytes, merkle_node, note_commitment, Field,
};
use crate::precompile::IEmit;
use crate::schema::{EmitContract, EMIT_ROOT_WINDOW, EMIT_TREE_CAPACITY, EMIT_TREE_DEPTH};
/// The explicit mint statement, exactly as it arrives on the ABI.
pub(crate) struct MintStatement {
    pub chain_id: u64,
    pub root: B256,
    pub nullifier: B256,
    pub note_owner: Address,
    pub mint_units: u64,
    pub change_commitment: B256,
}

/// Reads the live chain ID and derives its full in-memory empty ladder.
fn chain_state(storage: &StorageHandle<'_>) -> Result<(u64, Vec<Field>)> {
    let chain_id = storage.chain_id()?;
    let zeros = empty_subtrees(chain_id, EMIT_TREE_DEPTH);
    Ok((chain_id, zeros))
}
/// Appends `leaf` in O(depth) stored state using the Tornado Cash pattern:
/// for each level, the corresponding `leaf_count` bit decides whether the
/// current node completes a left subtree (store it in `filled_subtrees` and
/// combine with the empty subtree) or joins the stored left subtree (combine
/// as the right node). Finishes with one root/count/root-buffer update and
/// returns `(leaf_index, root_after)`.
fn append(emit: &EmitContract<'_>, zeros: &[Field], leaf: Field) -> Result<(u32, Field)> {
    let index = emit.leaf_count.read()?;
    debug_assert_eq!(zeros.len(), EMIT_TREE_DEPTH + 1);
    let mut current = leaf;
    for level in 0..EMIT_TREE_DEPTH {
        let level_byte = level as u8;
        if (index >> level) & 1 == 0 {
            emit.filled_subtrees
                .write(&level_byte, B256::new(field_to_be_bytes(current)))?;
            current = merkle_node(current, zeros[level]);
        } else {
            let left = emit.filled_subtrees.read(&level_byte)?;
            let left = field_from_be_bytes(&left.0).ok_or(PrecompileError::Fatal(
                "Emit filled-subtree slot is not a canonical field".into(),
            ))?;
            current = merkle_node(left, current);
        }
    }
    emit.current_root
        .write(B256::new(field_to_be_bytes(current)))?;
    emit.leaf_count.write(index + 1)?;
    emit.recent_roots
        .push(B256::new(field_to_be_bytes(current)))?;
    Ok((index, current))
}

/// `burn(noteSn)` — runtime-only native-COEN transition into a private note.
pub(crate) fn burn(
    storage: StorageHandle<'_>,
    _caller: Address,
    value: U256,
    note_sn: B256,
) -> Result<()> {
    // Guards, in transition-matrix order.
    if value.is_zero() {
        return Err(EmitError::BurnValueZero.into());
    }
    if value > U256::from(u64::MAX) {
        return Err(EmitError::BurnValueExceedsUint64.into());
    }
    let amount = value.to::<u64>();
    let serial = field_from_be_bytes(&note_sn.0)
        .ok_or(EmitError::NonCanonicalField("noteSn"))
        .map_err(PrecompileError::from)?;
    if serial.is_zero() {
        return Err(EmitError::MustBeNonZero("noteSn").into());
    }

    let (chain_id, zeros) = chain_state(&storage)?;
    let emit: EmitContract<'_> = storage.contract();
    let leaf_count = emit.leaf_count.read()?;
    if leaf_count >= EMIT_TREE_CAPACITY {
        return Err(EmitError::TreeFull.into());
    }

    // The commitment is always derived — never caller-supplied — so the
    // hidden note value is bound to the burned supply and runtime chain ID.
    let commitment = note_commitment(chain_id, serial, amount);
    if commitment.is_zero() {
        return Err(EmitError::MustBeNonZero("commitment").into());
    }
    let commitment_word = B256::new(field_to_be_bytes(commitment));
    if emit.commitments.read(&commitment_word)? {
        return Err(EmitError::CommitmentExists.into());
    }

    // One rollback unit: lazy initialization, append, commitment insert,
    // native burn, NewNote. `leaf_count == 0` is the pristine state —
    // initialization and the first append are one atomic unit, so an active
    // tree never observes `leaf_count == 0`.
    storage.with_checkpoint(|| {
        if leaf_count == 0 {
            let empty_root = B256::new(field_to_be_bytes(zeros[EMIT_TREE_DEPTH]));
            emit.current_root.write(empty_root)?;
            emit.recent_roots.setup(EMIT_ROOT_WINDOW)?;
            emit.recent_roots.push(empty_root)?;
        }
        let (index, root_after) = append(&emit, &zeros, commitment)?;
        emit.commitments.write(&commitment_word, true)?;

        // revm credited `msg.value` to EMIT_ADDRESS before dispatch; burn it
        // back out for exactly this call's value. A shortfall is corruption.
        let balance = storage.balance(EMIT_ADDRESS)?;
        if balance < value {
            return Err(EmitError::UnderfundedBurn {
                balance,
                credited: value,
            }
            .into());
        }
        storage.decrease_balance(EMIT_ADDRESS, value)?;

        storage.emit_event(
            EMIT_ADDRESS,
            IEmit::NewNote::encode_log_data(&IEmit::NewNote {
                commitment: commitment_word,
                leafIndex: index,
                rootAfter: B256::new(field_to_be_bytes(root_after)),
                noteAmount: amount,
            }),
        )?;
        Ok(())
    })
}

/// `mint(...)` — consume a frozen `outbe.emit.mint@1.0.0` proof.
pub(crate) fn mint(
    storage: StorageHandle<'_>,
    caller: Address,
    payout_recipient: Address,
    statement: MintStatement,
    proof: &[u8],
) -> Result<()> {
    // Combined-proof framing must decode before any state is touched; on a
    // pristine chain it is the only check allowed before the initialization
    // gate (frozen matrix: "ABI/proof framing may decode, then
    // `Emit is not initialized`").
    let embedded = decode_emit_mint_public_inputs(proof)
        .map_err(|error| PrecompileError::from(EmitError::MalformedProof(error.to_string())))?;

    let (runtime_chain_id, zeros) = chain_state(&storage)?;
    let emit: EmitContract<'_> = storage.contract();
    if emit.leaf_count.read()? == 0 {
        return Err(EmitError::NotInitialized.into());
    }

    // Statement field elements must be canonical before they are compared or
    // hashed. `chain_id` and `mint_units` are already exact ABI-decoded u64s.
    let root = field_from_be_bytes(&statement.root.0)
        .ok_or_else(|| PrecompileError::from(EmitError::NonCanonicalField("root")))?;
    let nullifier = field_from_be_bytes(&statement.nullifier.0)
        .ok_or_else(|| PrecompileError::from(EmitError::NonCanonicalField("nullifier")))?;
    let change = field_from_be_bytes(&statement.change_commitment.0)
        .ok_or_else(|| PrecompileError::from(EmitError::NonCanonicalField("changeCommitment")))?;

    // The embedded statement must equal the explicit calldata exactly — a
    // security check, not optional redundancy.
    let statement_matches = embedded.chain_id == statement.chain_id
        && embedded.root == statement.root.0
        && embedded.nullifier == statement.nullifier.0
        && embedded.note_owner == statement.note_owner
        && embedded.mint_units == statement.mint_units
        && embedded.change_commitment == statement.change_commitment.0;
    if !statement_matches {
        return Err(EmitError::StatementMismatch.into());
    }

    // A proof-supplied chain ID is never trusted.
    if statement.chain_id != runtime_chain_id {
        return Err(EmitError::ChainIdMismatch.into());
    }

    if statement.note_owner.is_zero() {
        return Err(EmitError::OwnerZero.into());
    }
    if payout_recipient.is_zero() {
        return Err(EmitError::RecipientZero.into());
    }
    if statement.mint_units == 0 {
        return Err(EmitError::MintUnitsZero.into());
    }
    if nullifier.is_zero() {
        return Err(EmitError::MustBeNonZero("nullifier").into());
    }
    if caller != statement.note_owner {
        return Err(EmitError::NotNoteOwner.into());
    }

    let root_word = B256::new(field_to_be_bytes(root));
    if !emit.recent_roots.read_all()?.contains(&root_word) {
        return Err(EmitError::RootNotRecent.into());
    }

    let nullifier_word = B256::new(field_to_be_bytes(nullifier));
    if emit.spent_nullifiers.read(&nullifier_word)? {
        return Err(EmitError::NullifierSpent.into());
    }
    match verify_emit_mint(proof) {
        Ok(true) => {}
        Ok(false) => return Err(EmitError::ProofInvalid.into()),
        // CRS initialization is the distinguishable infrastructure signal;
        // it stays fatal per the frozen proof-consumer split.
        Err(ZkProofError::CrsInitialization(message)) => {
            return Err(EmitError::VerifierUnavailable(message).into())
        }
        // Every other verifier error — including a `VerificationBackend`
        // rejection of caller-supplied proof bytes — is raised while
        // verifying attacker-controllable material and fails closed as a
        // user revert. The backend cannot distinguish rejected input from a
        // genuine FFI failure at this seam; misreporting the rare genuine
        // failure as an invalid proof is safe and retryable, while promoting
        // attacker input to a fatal error would be an unprivileged
        // consensus-visible DoS.
        Err(error) => return Err(EmitError::MalformedProof(error.to_string()).into()),
    }

    // Recipient credit overflow is a user guard, not corruption.
    let units = U256::from(statement.mint_units);
    let recipient_balance = storage.balance(payout_recipient)?;
    if recipient_balance.checked_add(units).is_none() {
        return Err(EmitError::PayoutOverflow.into());
    }

    // Full mint requires the zero change sentinel; partial mint appends the
    // circuit-derived nonzero deterministic change.
    let partial = !change.is_zero();
    let change_word = B256::new(field_to_be_bytes(change));
    if partial {
        let leaf_count = emit.leaf_count.read()?;
        if leaf_count >= EMIT_TREE_CAPACITY {
            return Err(EmitError::TreeFull.into());
        }
        // Anyone knowing the current key can pre-create the deterministic
        // change; the resulting duplicate reverts atomically. Accepted DoS
        // exposure — never a fallback to minting without change.
        if emit.commitments.read(&change_word)? {
            return Err(EmitError::CommitmentExists.into());
        }
    }

    // One rollback unit: nullifier, optional change append, credit, events.
    storage.with_checkpoint(|| {
        emit.spent_nullifiers.write(&nullifier_word, true)?;
        let change_receipt = if partial {
            let (index, root_after) = append(&emit, &zeros, change)?;
            emit.commitments.write(&change_word, true)?;
            Some((index, root_after))
        } else {
            None
        };
        storage.increase_balance(payout_recipient, units)?;
        storage.emit_event(
            EMIT_ADDRESS,
            IEmit::NoteUsed::encode_log_data(&IEmit::NoteUsed {
                noteOwner: statement.note_owner,
                payoutRecipient: payout_recipient,
                nullifier: nullifier_word,
                mintAmount: statement.mint_units,
            }),
        )?;
        if let Some((index, root_after)) = change_receipt {
            storage.emit_event(
                EMIT_ADDRESS,
                IEmit::NewNote::encode_log_data(&IEmit::NewNote {
                    commitment: change_word,
                    leafIndex: index,
                    rootAfter: B256::new(field_to_be_bytes(root_after)),
                    noteAmount: 0,
                }),
            )?;
        }
        Ok(())
    })
}
