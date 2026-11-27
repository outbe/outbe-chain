//! PayNote transition core — the state machine shared by dispatch, the
//! cross-module API, and tests.
//!
//! Every mutating path runs under [`StorageHandle::with_checkpoint`], making
//! tree, replay, token, and event effects one rollback unit. All guards precede
//! mutation. Guard failures convert from [`PayNoteError`] (which fixes the
//! revert texts and the fatal/revert split) via `From`.
//! Canonical field words are exactly 32 big-endian bytes.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use ark_ff::Zero;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS};
use outbe_zk::codec::{field_from_b256, field_to_b256};
use outbe_zk::noir::paynote::Paynote;
use outbe_zk::paynote::{
    alloy::PublicInputs as PayNotePublicInputs,
    decode_public_inputs as decode_paynote_public_inputs,
};
use outbe_zk::paynote_claim::{self, PaynoteClaim};
use outbe_zk::paynote_merge::{self, PaynoteMerge, MAX_MERGE_INPUTS};
use outbe_zk::paynote_transfer::{self, PaynoteTransfer};
use outbe_zk::protocol::zk::Circuit;
use outbe_zk::CircuitId;
use outbe_zk_backend::barretenberg::verify_circuit;

use crate::context::withdraw_context;
use crate::errors::PayNoteError;
use crate::hash::{empty_subtrees, merkle_node, transfer_commitment};
use crate::precompile::IPayNote;
use crate::schema::{
    PayNoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_CAPACITY, PAYNOTE_TREE_DEPTH,
};
use crate::token;
use crate::Field;

/// The validated public claim a spend proof carries, returned to the consuming
/// module. PayNote books the nullifier and any change note. The caller decides
/// what the released value buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayNoteClaim {
    pub asset: Address,
    /// Opaque settlement statement. The caller recomputes it. PayNote does not.
    pub context: B256,
    pub spend_amount: U256,
    /// The canonical nullifier this spend booked. It is the only public
    /// identifier of the payment, so a consuming module can record which note
    /// paid it without learning anything that links back to the depositor.
    pub nullifier: B256,
}

/// The circuit capacity fits u32 (currently four). The conversion is checked so
/// that ABI and generated witness changes cannot silently truncate the
/// published bound.
pub(crate) fn max_merge_inputs() -> Result<u32> {
    u32::try_from(MAX_MERGE_INPUTS)
        .map_err(|_| PrecompileError::Fatal("PayNote merge capacity exceeds u32".into()))
}

/// Proof-authorized consolidation. It shares settlement's canonical nullifiers,
/// tree and checkpoint. It touches no token, Reserve, Oracle or right state.
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
    if claim.output_commitment.is_zero()
        || padding.iter().any(|word| !word.is_zero())
        || active
            .iter()
            .enumerate()
            .any(|(i, word)| word.is_zero() || active[..i].contains(word))
    {
        return Err(PayNoteError::InvalidInput(
            "merge has zero or duplicate inputs, zero output, or nonzero padding".into(),
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
    // The canonical decoder and circuit prove the confidential U256 sum. The
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

/// How long the recipient of a transfer has before the sender may reclaim it.
pub const PAYNOTE_TRANSFER_TIMELOCK_SECS: u64 = 30 * 24 * 60 * 60;

/// Rejects a `deadline` that leaves the recipient less than the timelock.
fn check_transfer_deadline(storage: &StorageHandle<'_>, deadline: u64) -> Result<()> {
    let earliest = storage
        .now_secs()?
        .checked_add(PAYNOTE_TRANSFER_TIMELOCK_SECS)
        .ok_or_else(|| PayNoteError::InvalidInput("transfer deadline overflows".into()))?;
    if deadline < earliest {
        return Err(PayNoteError::InvalidInput(
            "transfer deadline is earlier than the 30-day timelock".into(),
        )
        .into());
    }
    Ok(())
}

/// `transfer(proof)` — spend an ordinary note into a locked transfer note and
/// an optional change note. The recipient can claim or decline the locked note
/// at once. The sender can reclaim it from `deadline` on, which must leave the
/// recipient at least [`PAYNOTE_TRANSFER_TIMELOCK_SECS`]. No funds move.
pub(crate) fn transfer(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<()> {
    let claim: paynote_transfer::alloy::PublicInputs =
        paynote_transfer::decode_public_inputs(proof)
            .and_then(TryInto::try_into)
            .map_err(|error| {
                PayNoteError::InvalidInput(format!("transfer proof is malformed: {error}"))
            })?;
    check_transfer_deadline(storage, claim.deadline)?;
    let mut outputs = vec![claim.locked_commitment];
    // A full transfer carries the zero change sentinel.
    if !claim.change_commitment.is_zero() {
        outputs.push(claim.change_commitment);
    }
    replace_note::<PaynoteTransfer>(
        storage,
        proof,
        (claim.chain_id, claim.root, claim.nullifier, claim.asset),
        &outputs,
        IPayNote::NoteTransferred::encode_log_data(&IPayNote::NoteTransferred {
            asset: claim.asset,
            lockedCommitment: claim.locked_commitment,
            nullifier: claim.nullifier,
            deadline: claim.deadline,
        }),
    )
}

/// `resolveTransfer(proof)` — turn a locked transfer note into an ordinary
/// note. The circuit lets the recipient do it at any time, and the sender only
/// when the deadline is not after `not_before`. Thus the runtime only requires
/// that `not_before` is not in the future.
pub(crate) fn resolve_transfer(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<()> {
    let claim: paynote_claim::alloy::PublicInputs = paynote_claim::decode_public_inputs(proof)
        .and_then(TryInto::try_into)
        .map_err(|error| {
            PayNoteError::InvalidInput(format!("claim proof is malformed: {error}"))
        })?;
    if claim.not_before > storage.now_secs()? {
        return Err(PayNoteError::InvalidInput("claim not_before is in the future".into()).into());
    }
    replace_note::<PaynoteClaim>(
        storage,
        proof,
        (claim.chain_id, claim.root, claim.nullifier, claim.asset),
        &[claim.output_commitment],
        IPayNote::TransferResolved::encode_log_data(&IPayNote::TransferResolved {
            asset: claim.asset,
            outputCommitment: claim.output_commitment,
            nullifier: claim.nullifier,
        }),
    )
}

/// The transition `transfer` and `resolve_transfer` share: verify a `C` proof,
/// nullify one note and append `outputs`, whose values are private. All guards
/// run before the proof is verified, and all effects are one rollback unit.
fn replace_note<C: Circuit + CircuitId>(
    storage: &StorageHandle<'_>,
    proof: &[u8],
    (chain_id, root, nullifier, asset): (u64, B256, B256, Address),
    outputs: &[B256],
    event: alloy_primitives::LogData,
) -> Result<()> {
    let (runtime_chain_id, zeros) = chain_state(storage)?;
    if chain_id != runtime_chain_id {
        return Err(PayNoteError::InvalidInput("chain ID does not match runtime".into()).into());
    }
    if nullifier.is_zero() {
        return Err(PayNoteError::InvalidInput("nullifier must be non-zero".into()).into());
    }
    let paynote: PayNoteContract<'_> = storage.contract();
    let leaf_count = paynote.leaf_count.read()?;
    if leaf_count == 0 {
        return Err(PayNoteError::NotInitialized.into());
    }
    // At most two outputs, so the cast and the sum cannot overflow.
    if leaf_count + outputs.len() as u64 > PAYNOTE_TREE_CAPACITY {
        return Err(PayNoteError::TreeFull.into());
    }
    if !paynote.recent_roots.read_all()?.contains(&root) {
        return Err(PayNoteError::RootNotRecent.into());
    }
    if paynote.spent_nullifiers.read(&nullifier)? {
        return Err(PayNoteError::NullifierSpent.into());
    }
    let mut leaves = Vec::with_capacity(outputs.len());
    for word in outputs {
        let leaf = field_from_b256(word).map_err(|_| {
            PayNoteError::InvalidInput("commitment is not a canonical BN254 field".into())
        })?;
        if leaf.is_zero() {
            return Err(PayNoteError::InvalidInput("commitment must be non-zero".into()).into());
        }
        if paynote.commitments.read(word)? {
            return Err(PayNoteError::CommitmentExists.into());
        }
        leaves.push((leaf, *word));
    }
    match verify_circuit::<C>(proof) {
        Ok(true) => {}
        Ok(false) => return Err(PayNoteError::InvalidInput("proof is invalid".into()).into()),
        Err(error) => {
            return Err(
                PayNoteError::InvalidInput(format!("proof verification failed: {error}")).into(),
            )
        }
    }
    storage.with_checkpoint(|| {
        paynote.spent_nullifiers.write(&nullifier, true)?;
        storage.emit_event(PAYNOTE_ADDRESS, event)?;
        for (leaf, word) in leaves {
            let (index, root_after) = append(&paynote, &zeros, leaf)?;
            paynote.commitments.write(&word, true)?;
            storage.emit_event(
                PAYNOTE_ADDRESS,
                IPayNote::NewNote::encode_log_data(&IPayNote::NewNote {
                    commitment: word,
                    leafIndex: index,
                    rootAfter: field_to_b256(&root_after)
                        .map_err(|error| PrecompileError::Fatal(error.to_string()))?,
                    asset,
                    // Sentinel: the value of the note is private.
                    noteAmount: U256::ZERO,
                }),
            )?;
        }
        Ok(())
    })
}

/// Reads the live chain ID and derives its full in-memory empty ladder.
fn chain_state(storage: &StorageHandle<'_>) -> Result<(u64, Vec<Field>)> {
    let chain_id = storage.chain_id()?;
    let zeros = empty_subtrees(chain_id, PAYNOTE_TREE_DEPTH).map_err(|_| PayNoteError::Hash)?;
    Ok((chain_id, zeros))
}

/// Appends `leaf` in O(depth) stored state with the Tornado Cash pattern. At
/// each level, the corresponding `leaf_count` bit selects one of two cases:
/// - The current node completes a left subtree. Store it in `filled_subtrees`
///   and combine it with the empty subtree.
/// - The current node joins the stored left subtree. Combine it as the right
///   node.
///
/// The function finishes with one root/count/root-buffer update and returns
/// `(leaf_index, root_after)`.
///
/// Every call site bounds `index` by [`PAYNOTE_TREE_CAPACITY`], so the `u32`
/// narrowing for the returned leaf index cannot truncate.
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

/// `deposit(asset, amount, lock, deadline)` — move the asset into the pool and
/// append the derived locked transfer note, the same leaf a [`transfer`] makes.
/// The recipient in `lock` resolves it at once, the sender from `deadline` on.
/// A self-deposit names the depositor as both. `deadline` must leave at least
/// [`PAYNOTE_TRANSFER_TIMELOCK_SECS`].
/// An ERC20 is pulled from the caller and stays in
/// the pool until a spend routes it into the reserve vault of the asset.
/// `asset == 0` is native COEN: the payable route has already credited
/// `msg.value` to the pool, so it must equal `amount`.
pub(crate) fn deposit(
    storage: StorageHandle<'_>,
    caller: Address,
    (asset, amount, value): (Address, U256, U256),
    (lock, deadline): (B256, u64),
) -> Result<()> {
    // Guards, before any mutation.
    if amount.is_zero() {
        return Err(PayNoteError::InvalidInput("deposit amount must be non-zero".into()).into());
    }
    let native = asset.is_zero();
    let expected_value = if native { amount } else { U256::ZERO };
    if value != expected_value {
        return Err(PayNoteError::NativeValueMismatch.into());
    }
    let lock = field_from_b256(&lock)
        .map_err(|_| PayNoteError::InvalidInput("lock is not a canonical BN254 field".into()))?;
    if lock.is_zero() {
        return Err(PayNoteError::InvalidInput("lock must be non-zero".into()).into());
    }
    check_transfer_deadline(&storage, deadline)?;

    let (chain_id, zeros) = chain_state(&storage)?;
    let paynote: PayNoteContract<'_> = storage.contract();
    let leaf_count = paynote.leaf_count.read()?;
    if leaf_count >= PAYNOTE_TREE_CAPACITY {
        return Err(PayNoteError::TreeFull.into());
    }
    // A spend routes the tokens into the vault of the asset. If the asset has
    // no vault, no spend can settle the note. Thus, reject the deposit.
    // NB: native COEN has no vault. A native note can be merged and withdrawn.
    if !native && outbe_vaultrouter::api::asset_vaults_count(&storage, asset)?.is_zero() {
        return Err(PayNoteError::AssetVaultMissing.into());
    }

    // The runtime always derives the commitment from the asset and amount this
    // call actually moves. The caller never supplies it, so Merkle membership
    // attests both. A caller-chosen leaf would let a depositor fund a note in a
    // cheap token and spend it as an expensive one.
    let commitment = transfer_commitment(chain_id, lock, deadline, asset, amount)
        .map_err(|_| PayNoteError::Hash)?;
    if commitment.is_zero() {
        return Err(PayNoteError::InvalidInput("commitment must be non-zero".into()).into());
    }
    let commitment_word =
        field_to_b256(&commitment).map_err(|error| PrecompileError::Fatal(error.to_string()))?;
    if paynote.commitments.read(&commitment_word)? {
        return Err(PayNoteError::CommitmentExists.into());
    }

    // One rollback unit: token movement, lazy initialization, append,
    // commitment insert, NewNote. `leaf_count == 0` is the pristine state.
    // Initialization and the first append are atomic, so an active tree never
    // observes `leaf_count == 0`.
    storage.with_checkpoint(|| {
        // The caller must approve the pool. The commitment uses `amount`. Thus,
        // the pool must receive exactly `amount`. This check also rejects a
        // fee-on-transfer token and a reentrant nested deposit.
        // Native COEN is already in the pool: the route credited `msg.value`.
        if !native {
            token::pull_exact(&storage, asset, caller, amount)?;
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

/// Where a spend sends the released value.
#[derive(Clone, Copy)]
enum Payout {
    /// Settlement: through VaultRouter into the reserve vault of the asset.
    Vault,
    /// Withdrawal: straight to this address.
    Recipient(Address),
}

/// `consume(proof)` — verify a frozen `outbe.paynote@1.4.0` spend proof and
/// return the validated claim. The function nullifies the note and appends the
/// change commitment, if there is one. Then it routes `spend_amount` of the
/// asset from the pool into the reserve vault of the asset.
///
/// The proof is the single source of truth for the statement it carries.
/// `context` is an opaque public word. The caller recomputes the settlement
/// statement and compares it. PayNote does not interpret the word.
///
/// Notes are bearer instruments: spend authority is knowledge of the spend
/// key, not an address. Thus there is deliberately no caller check. Any address
/// may relay a proof. The nullifier does not include `context`, so every
/// statement for one note shares one use.
pub(crate) fn consume(storage: &StorageHandle<'_>, proof: &[u8]) -> Result<PayNoteClaim> {
    spend(storage, proof, Payout::Vault)
}

/// `withdraw(proof, recipient)` — spend a note and pay `spend_amount` of its
/// asset from the pool to `recipient`: an ERC20 transfer, or a native COEN
/// balance move for a note with the zero asset.
///
/// Any address may relay the proof. The proof binds the recipient through
/// [`withdraw_context`], so a relayer cannot redirect the funds, and a
/// settlement proof is not a withdrawal proof.
pub(crate) fn withdraw(
    storage: &StorageHandle<'_>,
    proof: &[u8],
    recipient: Address,
) -> Result<()> {
    // A payout to the pool itself would nullify the note and strand its value.
    if recipient.is_zero() || recipient == PAYNOTE_ADDRESS {
        return Err(
            PayNoteError::InvalidInput("recipient must not be zero or the pool".into()).into(),
        );
    }
    spend(storage, proof, Payout::Recipient(recipient)).map(|_| ())
}

/// The spend transition that settlement and withdrawal share. All guards run
/// before the proof is verified, and all effects are one rollback unit.
fn spend(storage: &StorageHandle<'_>, proof: &[u8], payout: Payout) -> Result<PayNoteClaim> {
    // Framing must decode before the function touches any state.
    let claim: PayNotePublicInputs = decode_paynote_public_inputs(proof)
        .and_then(TryInto::try_into)
        .map_err(|error| PayNoteError::InvalidInput(format!("proof is malformed: {error}")))?;

    let (runtime_chain_id, zeros) = chain_state(storage)?;
    let paynote: PayNoteContract<'_> = storage.contract();
    if paynote.leaf_count.read()? == 0 {
        return Err(PayNoteError::NotInitialized.into());
    }

    // The runtime never trusts a proof-supplied chain ID.
    if claim.chain_id != runtime_chain_id {
        return Err(PayNoteError::InvalidInput("chain ID does not match runtime".into()).into());
    }

    match payout {
        // NB: native COEN has no vault, so a native note cannot settle. The
        // ERC20 routing below would call address 0, which succeeds silently.
        Payout::Vault => {
            if claim.asset.is_zero() {
                return Err(PayNoteError::InvalidInput("asset must be non-zero".into()).into());
            }
        }
        Payout::Recipient(recipient) => {
            let expected = withdraw_context(recipient)?;
            if claim.context != expected {
                return Err(PayNoteError::ContextMismatch {
                    expected,
                    actual: claim.context,
                }
                .into());
            }
        }
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

    // A full spend requires the zero change sentinel. A partial spend appends
    // exactly the circuit-derived deterministic change.
    let partial = !change.is_zero();
    let change_word = claim.change_commitment;
    if partial {
        if paynote.leaf_count.read()? >= PAYNOTE_TREE_CAPACITY {
            return Err(PayNoteError::TreeFull.into());
        }
        // Anyone who knows the current key can pre-create the deterministic
        // change. The resulting duplicate reverts atomically. This is an
        // accepted DoS exposure. There is never a fallback to spending without
        // recording change.
        if paynote.commitments.read(&change_word)? {
            return Err(PayNoteError::CommitmentExists.into());
        }
    }

    // These steps roll back as one unit: the nullifier, the optional change
    // append, the payout and the events. The state changes occur before the
    // token sub-calls.
    storage.with_checkpoint(|| {
        paynote.spent_nullifiers.write(&nullifier_word, true)?;
        let change_receipt = if partial {
            let (index, root_after) = append(&paynote, &zeros, change)?;
            paynote.commitments.write(&change_word, true)?;
            Some((index, root_after))
        } else {
            None
        };
        // The change value stays in the pool.
        match payout {
            // The router pulls exactly `spend_amount` from the pool. Thus, no
            // allowance stays after the call.
            Payout::Vault => {
                token::approve(
                    storage,
                    claim.asset,
                    VAULT_ROUTER_ADDRESS,
                    claim.spend_amount,
                )?;
                outbe_vaultrouter::api::deposit_from(
                    storage,
                    PAYNOTE_ADDRESS,
                    claim.asset,
                    claim.spend_amount,
                )?;
            }
            Payout::Recipient(recipient) if claim.asset.is_zero() => {
                storage.transfer_balance(PAYNOTE_ADDRESS, recipient, claim.spend_amount)?;
            }
            Payout::Recipient(recipient) => {
                token::transfer(storage, claim.asset, recipient, claim.spend_amount)?;
            }
        }
        storage.emit_event(
            PAYNOTE_ADDRESS,
            IPayNote::NoteUsed::encode_log_data(&IPayNote::NoteUsed {
                asset: claim.asset,
                context: claim.context,
                nullifier: nullifier_word,
                amountMinor: claim.spend_amount,
            }),
        )?;
        if let Payout::Recipient(recipient) = payout {
            storage.emit_event(
                PAYNOTE_ADDRESS,
                IPayNote::NoteWithdrawn::encode_log_data(&IPayNote::NoteWithdrawn {
                    asset: claim.asset,
                    recipient,
                    nullifier: nullifier_word,
                    amount: claim.spend_amount,
                }),
            )?;
        }
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
