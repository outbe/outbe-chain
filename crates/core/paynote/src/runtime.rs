//! Paynote transition core — the state machine shared by dispatch, the
//! cross-module API, and tests.
//!
//! Every mutating path runs under [`StorageHandle::with_checkpoint`], making
//! tree, replay, token, and event effects one rollback unit. All guards precede
//! mutation. Guard failures convert from [`PaynoteError`] (which fixes the
//! revert texts and the fatal/revert split) via `From`.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{SolCall, SolEvent};
use ark_ff::Zero;
use outbe_primitives::addresses::{PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_zkproof::{decode_paynote_public_inputs, verify_paynote, ZkProofError};

use crate::errors::PaynoteError;
use crate::hash::{
    empty_subtrees, field_from_be_bytes, field_to_be_bytes, merkle_node, note_commitment, Field,
};
use crate::precompile::IPaynote;
use crate::schema::{
    PaynoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_CAPACITY, PAYNOTE_TREE_DEPTH,
};
use crate::sol_ext::IERC20;

/// The validated public claim a spend proof carries, returned to the consuming
/// module. Paynote books the nullifier and any change note; deciding what the
/// released value buys is the caller's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaynoteClaim {
    pub asset: Address,
    pub spender: Address,
    pub spend_amount: u128,
}

/// Reads the live chain ID and derives its full in-memory empty ladder.
fn chain_state(storage: &StorageHandle<'_>) -> Result<(u64, Vec<Field>)> {
    let chain_id = storage.chain_id()?;
    let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH)?;
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
    paynote: &PaynoteContract<'_>,
    zeros: &[Field],
    leaf: Field,
) -> Result<(u32, Field)> {
    let index = paynote.leaf_count.read()?;
    let mut current = leaf;
    for (level, zero) in zeros.iter().enumerate().take(PAYNOTE_TREE_DEPTH) {
        let level_byte = u8::try_from(level).map_err(|_| PaynoteError::CorruptFrontier)?;
        if (index >> level) & 1 == 0 {
            paynote
                .filled_subtrees
                .write(&level_byte, B256::new(field_to_be_bytes(current)))?;
            current = merkle_node(current, *zero)?;
        } else {
            let left = paynote.filled_subtrees.read(&level_byte)?;
            let left = field_from_be_bytes(&left.0).ok_or(PaynoteError::CorruptFrontier)?;
            current = merkle_node(left, current)?;
        }
    }
    let root_after = B256::new(field_to_be_bytes(current));
    paynote.current_root.write(root_after)?;
    paynote.leaf_count.write(index + 1)?;
    paynote.recent_roots.push(root_after)?;
    let leaf_index = u32::try_from(index).map_err(|_| PaynoteError::TreeFull)?;
    Ok((leaf_index, current))
}

/// `deposit(asset, amount, noteSn)` — pull the ERC20, route it into the
/// asset's reserve vault, and append the derived note commitment.
pub(crate) fn deposit(
    storage: StorageHandle<'_>,
    caller: Address,
    asset: Address,
    amount: u128,
    note_sn: B256,
) -> Result<()> {
    // Guards, before any mutation.
    if amount == 0 {
        return Err(PaynoteError::DepositAmountZero.into());
    }
    if asset.is_zero() {
        return Err(PaynoteError::MustBeNonZero("asset").into());
    }
    let serial =
        field_from_be_bytes(&note_sn.0).ok_or(PaynoteError::NonCanonicalField("noteSn"))?;
    if serial.is_zero() {
        return Err(PaynoteError::MustBeNonZero("noteSn").into());
    }

    let (chain_id, zeros) = chain_state(&storage)?;
    let paynote: PaynoteContract<'_> = storage.contract();
    let leaf_count = paynote.leaf_count.read()?;
    if leaf_count >= PAYNOTE_TREE_CAPACITY {
        return Err(PaynoteError::TreeFull.into());
    }

    // The commitment is always derived from the asset and amount this call
    // actually moves — never caller-supplied — so Merkle membership attests
    // both. A caller-chosen leaf would let a depositor fund a note in a cheap
    // token and spend it as an expensive one.
    let commitment = note_commitment(chain_id, serial, asset.into(), amount)?;
    if commitment.is_zero() {
        return Err(PaynoteError::MustBeNonZero("commitment").into());
    }
    let commitment_word = B256::new(field_to_be_bytes(commitment));
    if paynote.commitments.read(&commitment_word)? {
        return Err(PaynoteError::CommitmentExists.into());
    }

    // One rollback unit: token movement, lazy initialization, append,
    // commitment insert, NewNote. `leaf_count == 0` is the pristine state —
    // initialization and the first append are atomic, so an active tree never
    // observes `leaf_count == 0`.
    storage.with_checkpoint(|| {
        let units = U256::from(amount);
        // Pull into the pool, then let the router pull from the pool: the
        // router's `deposit` is a `transferFrom(caller, SELF)`, so the pool
        // must both hold the tokens and approve the router.
        storage.call(
            asset,
            U256::ZERO,
            IERC20::transferFromCall {
                from: caller,
                to: PAYNOTE_ADDRESS,
                amount: units,
            }
            .abi_encode()
            .into(),
        )?;
        storage.call(
            asset,
            U256::ZERO,
            IERC20::approveCall {
                spender: VAULT_ROUTER_ADDRESS,
                amount: units,
            }
            .abi_encode()
            .into(),
        )?;
        outbe_vaultrouter::api::deposit(&storage, asset, units)?;

        if leaf_count == 0 {
            let empty_root = B256::new(field_to_be_bytes(zeros[PAYNOTE_TREE_DEPTH]));
            paynote.current_root.write(empty_root)?;
            paynote.recent_roots.setup(PAYNOTE_ROOT_WINDOW)?;
            paynote.recent_roots.push(empty_root)?;
        }
        let (index, root_after) = append(&paynote, &zeros, commitment)?;
        paynote.commitments.write(&commitment_word, true)?;

        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPaynote::NewNote::encode_log_data(&IPaynote::NewNote {
                commitment: commitment_word,
                leafIndex: index,
                rootAfter: root_after_word(root_after),
                asset,
                noteAmount: amount,
            }),
        )?;
        Ok(())
    })
}

fn root_after_word(root: Field) -> B256 {
    B256::new(field_to_be_bytes(root))
}

/// `consume(proof)` — verify a frozen `outbe.paynote@1.0.0` spend proof,
/// nullify the note, append any change commitment, and return the validated
/// claim. Moves no tokens.
///
/// The proof is the single source of truth for the statement: there is no
/// calldata path, so there is nothing to cross-check it against and no
/// statement-mismatch failure mode.
///
/// Notes are bearer instruments — spend authority is knowledge of the spend
/// key, not an address — so there is deliberately no caller check. The circuit
/// binds `spender` as the payout target, so a third party who replays someone
/// else's proof only spends their own gas; the claim still names the intended
/// spender.
pub(crate) fn consume(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<PaynoteClaim> {
    // Framing must decode before any state is touched.
    let claim = decode_paynote_public_inputs(proof)
        .map_err(|error| PaynoteError::MalformedProof(error.to_string()))?;

    let (runtime_chain_id, zeros) = chain_state(storage)?;
    let paynote: PaynoteContract<'_> = storage.contract();
    if paynote.leaf_count.read()? == 0 {
        return Err(PaynoteError::NotInitialized.into());
    }

    // A proof-supplied chain ID is never trusted.
    if claim.chain_id != runtime_chain_id {
        return Err(PaynoteError::ChainIdMismatch.into());
    }

    // `asset != 0` is enforced here regardless of the circuit: the frozen
    // 1.0.0 circuit asserts it, but the head `.nr` source has dropped that
    // assert, so a future re-freeze would silently remove it. The pool never
    // builds a zero-asset leaf, so rejecting one costs nothing.
    if claim.asset.is_zero() {
        return Err(PaynoteError::MustBeNonZero("asset").into());
    }
    if claim.spender.is_zero() {
        return Err(PaynoteError::MustBeNonZero("spender").into());
    }
    if claim.spend_amount == 0 {
        return Err(PaynoteError::SpendAmountZero.into());
    }

    let nullifier = field_from_be_bytes(&claim.nullifier)
        .ok_or(PaynoteError::NonCanonicalField("nullifier"))?;
    if nullifier.is_zero() {
        return Err(PaynoteError::MustBeNonZero("nullifier").into());
    }
    let change = field_from_be_bytes(&claim.change_commitment)
        .ok_or(PaynoteError::NonCanonicalField("changeCommitment"))?;

    let root_word = B256::new(claim.root);
    if !paynote.recent_roots.read_all()?.contains(&root_word) {
        return Err(PaynoteError::RootNotRecent.into());
    }

    let nullifier_word = B256::new(field_to_be_bytes(nullifier));
    if paynote.spent_nullifiers.read(&nullifier_word)? {
        return Err(PaynoteError::NullifierSpent.into());
    }

    match verify_paynote(proof) {
        Ok(true) => {}
        Ok(false) => return Err(PaynoteError::ProofInvalid.into()),
        // CRS initialization is the distinguishable infrastructure signal and
        // stays fatal.
        Err(ZkProofError::CrsInitialization(message)) => {
            return Err(PaynoteError::VerifierUnavailable(message).into())
        }
        // Every other verifier error is raised while verifying
        // attacker-controllable material and fails closed as a user revert.
        // The backend cannot distinguish rejected input from a genuine FFI
        // failure at this seam; promoting attacker input to a fatal error
        // would be an unprivileged consensus-visible DoS.
        Err(error) => return Err(PaynoteError::MalformedProof(error.to_string()).into()),
    }

    // A full spend requires the zero change sentinel; a partial spend appends
    // exactly the circuit-derived deterministic change.
    let partial = !change.is_zero();
    let change_word = B256::new(field_to_be_bytes(change));
    if partial {
        if paynote.leaf_count.read()? >= PAYNOTE_TREE_CAPACITY {
            return Err(PaynoteError::TreeFull.into());
        }
        // Anyone knowing the current key can pre-create the deterministic
        // change; the resulting duplicate reverts atomically. Accepted DoS
        // exposure — never a fallback to spending without recording change.
        if paynote.commitments.read(&change_word)? {
            return Err(PaynoteError::CommitmentExists.into());
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
            IPaynote::NoteUsed::encode_log_data(&IPaynote::NoteUsed {
                asset: claim.asset,
                spender: claim.spender,
                nullifier: nullifier_word,
                spendAmount: claim.spend_amount,
            }),
        )?;
        if let Some((index, root_after)) = change_receipt {
            storage.emit_event(
                PAYNOTE_ADDRESS,
                IPaynote::NewNote::encode_log_data(&IPaynote::NewNote {
                    commitment: change_word,
                    leafIndex: index,
                    rootAfter: root_after_word(root_after),
                    asset: claim.asset,
                    // Sentinel: a change note's remaining value is private.
                    noteAmount: 0,
                }),
            )?;
        }
        Ok(())
    })?;

    Ok(PaynoteClaim {
        asset: claim.asset,
        spender: claim.spender,
        spend_amount: claim.spend_amount,
    })
}
