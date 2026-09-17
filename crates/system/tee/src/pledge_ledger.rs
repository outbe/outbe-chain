//! Chain-authoritative journal and host transport for the resident ledger.
//! All persistent writes have global sequence keys; no owner selects a slot.

use alloy_primitives::{keccak256, B256, U256};
use outbe_primitives::{
    addresses::{FIDELITY_ADDRESS, GRATIS_ADDRESS},
    error::{PrecompileError, Result},
    storage::{pledge_journal_slot, StorageHandle},
};

use crate::{
    pledgenote::*,
    protocol::{EnclaveRequest, EnclaveResponse},
};

// Serialize recovery plus apply across local execution/RPC connections. A lock
// per request would let another connection replace the cache between batches.
static EXECUTION: std::sync::Mutex<()> = std::sync::Mutex::new(());

const ENTRY_BYTES: usize = 32 + JOURNAL_PLAINTEXT_BYTES + 16;
// Fixed work charge, independent of private account/cohort size or cache state.
const LEDGER_COMMAND_GAS: u64 = 1_000_000;

fn fault(message: impl Into<String>) -> PrecompileError {
    PrecompileError::Fatal(message.into())
}
fn root_slot() -> U256 {
    U256::from_be_bytes(keccak256(b"outbe/pledgenote/root/v1").0)
}
fn sequence_slot() -> U256 {
    U256::from_be_bytes(keccak256(b"outbe/pledgenote/sequence/v1").0)
}

pub fn head(storage: &StorageHandle<'_>) -> Result<Head> {
    // Sequence is explicitly a u64 counter in the versioned journal format.
    let sequence = storage
        .sload(GRATIS_ADDRESS, sequence_slot())?
        .try_into()
        .map_err(|_| fault("ledger sequence overflow"))?;
    let root = B256::from(storage.sload(GRATIS_ADDRESS, root_slot())?);
    Ok(Head { sequence, root })
}

fn exchange(request: EnclaveRequest, expected_hash: B256) -> Result<Reply> {
    #[cfg(feature = "test-utils")]
    if let Some(response) = test_backend::request(&request) {
        let response = response.map_err(fault)?;
        if response.inputs_hash != expected_hash {
            return Err(fault("test ledger input mismatch"));
        }
        return Ok(response.reply);
    }
    let (public_key, result) =
        crate::try_with_enclave(|client| (client.attestation_pub(), client.request(&request)))
            .ok_or_else(|| fault("tee_sidecar_unavailable"))?;
    let response = match result.map_err(|e| fault(format!("pledge ledger transport: {e}")))? {
        EnclaveResponse::PledgeLedger { response } => response,
        EnclaveResponse::Error { message } => return Err(fault(message)),
        _ => return Err(fault("unexpected pledge ledger response")),
    };
    if response.inputs_hash != expected_hash {
        return Err(fault("pledge ledger input mismatch"));
    }
    verify_response(&public_key, &response).map_err(fault)?;
    Ok(response.reply)
}

fn replay_batch(request: ReplayRequest) -> Result<Head> {
    let hash = replay_hash(&request).map_err(fault)?;
    match exchange(
        EnclaveRequest::ReplayPledgeLedger {
            request: Box::new(request),
        },
        hash,
    )? {
        Reply::Applied(outcome) => Ok(outcome.head),
        _ => Err(fault("pledge ledger replay interrupted")),
    }
}

/// Recovery reads are unmetered and strictly sequential. They reconstruct local
/// state only; ordinary command/journal gas does not depend on enclave warmth.
/// Each replay batch is at most 15 fixed-width records. Missing history is fatal.
fn recover(storage: &StorageHandle<'_>, expected: Head) -> Result<()> {
    let chain_id = B256::from(U256::from(storage.chain_id()?));
    let mut current = replay_batch(ReplayRequest {
        chain_id,
        parent: Head::default(),
        reset: true,
        entries: Vec::new(),
    })?;
    while current.sequence < expected.sequence {
        let end = current
            .sequence
            .saturating_add(REPLAY_BATCH_ENTRIES as u64)
            .min(expected.sequence);
        let mut entries = Vec::new();
        for index in current.sequence..end {
            let mut entry = Vec::with_capacity(130 * 32);
            for word in 0..130 {
                entry.extend_from_slice(
                    &storage
                        .pledge_journal_word(index, word)?
                        .to_be_bytes::<32>(),
                );
            }
            if entry[ENTRY_BYTES..].iter().any(|&v| v != 0) {
                return Err(fault("noncanonical journal storage"));
            }
            entry.truncate(ENTRY_BYTES);
            entries.push(entry);
        }
        let next = replay_batch(ReplayRequest {
            chain_id,
            parent: current,
            reset: false,
            entries,
        })?;
        if next.sequence != end {
            return Err(fault("ledger recovery did not advance"));
        }
        current = next;
    }
    if current != expected {
        return Err(fault("recovered ledger does not match chain head"));
    }
    Ok(())
}

/// Warm the parent ledger before block execution. Reorgs and restarts use the
/// same committed history; speculative heads never bypass the parent check.
pub fn synchronize(storage: &StorageHandle<'_>) -> Result<()> {
    let _guard = EXECUTION
        .lock()
        .map_err(|_| fault("ledger execution lock poisoned"))?;
    let expected = head(storage)?;
    if expected == Head::default() {
        return Ok(());
    }
    let request = ReplayRequest {
        chain_id: B256::from(U256::from(storage.chain_id()?)),
        parent: expected,
        reset: false,
        entries: Vec::new(),
    };
    let hash = replay_hash(&request).map_err(fault)?;
    match exchange(
        EnclaveRequest::ReplayPledgeLedger {
            request: Box::new(request),
        },
        hash,
    )? {
        Reply::Applied(outcome) if outcome.head == expected => Ok(()),
        Reply::NeedsReplay => recover(storage, expected),
        _ => Err(fault("invalid ledger readiness response")),
    }
}

pub fn execute(storage: &StorageHandle<'_>, command: Command) -> Result<Outcome> {
    let _guard = EXECUTION
        .lock()
        .map_err(|_| fault("ledger execution lock poisoned"))?;
    storage.deduct_gas(LEDGER_COMMAND_GAS)?;
    let parent = head(storage)?;
    // Execution headers bound timestamp to u64; reject malformed providers.
    let timestamp = storage
        .timestamp()?
        .try_into()
        .map_err(|_| fault("ledger timestamp overflow"))?;
    let context = Context {
        chain_id: B256::from(U256::from(storage.chain_id()?)),
        genesis_hash: storage.genesis_hash()?,
        block_number: storage.block_number()?,
        timestamp,
    };
    let read_only = command.is_read_only();
    let request = Request {
        schema: SCHEMA_VERSION,
        parent,
        context,
        command,
    };
    let hash = request_hash(&request).map_err(fault)?;
    let mut reply = exchange(
        EnclaveRequest::ApplyPledgeLedger {
            request: Box::new(request.clone()),
        },
        hash,
    )?;
    if reply == Reply::NeedsReplay {
        recover(storage, parent)?;
        reply = exchange(
            EnclaveRequest::ApplyPledgeLedger {
                request: Box::new(request),
            },
            hash,
        )?;
    }
    let outcome = match reply {
        Reply::Applied(outcome) => *outcome,
        Reply::Rejected(reason) => return Err(PrecompileError::Revert(reason)),
        Reply::NeedsReplay => return Err(fault("pledge ledger cache changed during execution")),
    };
    if read_only {
        if outcome.head != parent || !outcome.journal_entry.is_empty() {
            return Err(fault("read-only ledger command changed state"));
        }
        return Ok(outcome);
    }
    if outcome.head.sequence
        != parent
            .sequence
            .checked_add(1)
            .ok_or_else(|| fault("ledger sequence exhausted"))?
        || outcome.journal_entry.len() != ENTRY_BYTES
        || outcome.head.root != keccak256(&outcome.journal_entry)
        || outcome.journal_entry[..32] != hash[..]
    {
        return Err(fault("invalid ledger successor"));
    }
    storage.with_checkpoint(|| {
        for (word, chunk) in outcome.journal_entry.chunks(32).enumerate() {
            let mut bytes = [0u8; 32];
            bytes[..chunk.len()].copy_from_slice(chunk);
            // The validated 4144-byte record has exactly 130 words (< u16::MAX).
            let word = u16::try_from(word).map_err(|_| fault("journal word overflow"))?;
            storage.sstore(
                GRATIS_ADDRESS,
                pledge_journal_slot(parent.sequence, word)?,
                U256::from_be_bytes(bytes),
            )?;
        }
        storage.sstore(
            GRATIS_ADDRESS,
            root_slot(),
            U256::from_be_bytes(outcome.head.root.0),
        )?;
        storage.sstore(
            GRATIS_ADDRESS,
            sequence_slot(),
            U256::from(outcome.head.sequence),
        )?;
        storage.sstore(GRATIS_ADDRESS, U256::ZERO, outcome.total_supply)?;
        storage.sstore(GRATIS_ADDRESS, U256::from(1), outcome.pledged_supply)?;
        storage.sstore(
            FIDELITY_ADDRESS,
            U256::ZERO,
            U256::from(outcome.first_qualified_start),
        )
    })?;
    Ok(outcome)
}

#[cfg(feature = "test-utils")]
pub mod test_backend {
    use super::*;
    use std::cell::RefCell;
    type Handler = Box<dyn FnMut(&EnclaveRequest) -> std::result::Result<Response, String>>;
    thread_local! { static BACKEND: RefCell<Option<Handler>> = const { RefCell::new(None) }; }
    pub fn install(handler: Handler) {
        BACKEND.with(|slot| *slot.borrow_mut() = Some(handler));
    }
    pub fn uninstall() {
        BACKEND.with(|slot| *slot.borrow_mut() = None);
    }
    pub(super) fn request(
        request: &EnclaveRequest,
    ) -> Option<std::result::Result<Response, String>> {
        BACKEND.with(|slot| slot.borrow_mut().as_mut().map(|handler| handler(request)))
    }
}
