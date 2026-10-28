//! Block-scoped consensus enclave witness lifecycle (`ProposerRecord`, `VerifierWithEnclave`, `VerifierReplay`).

use super::*;
use outbe_primitives::{
    consensus::OUTBE_MAX_EXTRA_DATA_SIZE,
    projection::{NodeLocalExecutionAbort, WitnessAbortReason},
    reshare_artifact::{encode_outbe_block_artifacts, ENCLAVE_WITNESS_RESERVED_BYTES},
};
use outbe_tee::witness::{
    compressed_witness_bound, BlockWitnessFault, CandidateRejection, EnclaveWitnessTapeV1,
    TxWitnessV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
};
use std::sync::{Arc, Mutex};

mod init;

#[cfg(test)]
#[path = "tests.rs"]
mod witness_tests;

fn witness_lock_poisoned() -> BlockExecutionError {
    witness_fault_to_block_error(
        BlockWitnessFault::LocalUnavailable,
        "witness state lock poisoned",
    )
}

fn proposer_commit_fits(
    guard: &mut WitnessState,
    body_tx_index: u32,
    base_extra_data: &[u8],
    is_user_candidate: bool,
) -> bool {
    if guard.staged.is_empty() {
        return true;
    }
    let mut candidate_tape = guard.tape.clone();
    candidate_tape.txs.push(TxWitnessV1 {
        tx_index: body_tx_index,
        entries: guard.staged.clone(),
    });
    if tape_fits_budget(&candidate_tape, base_extra_data) {
        return true;
    }
    if is_user_candidate {
        guard.candidate_rejection = Some(CandidateRejection::BudgetDeferred);
    } else {
        guard.latch(BlockWitnessFault::TapeInvalid);
    }
    false
}

fn verifier_commit_consumed(guard: &mut WitnessState, body_tx_index: u32) -> bool {
    let Some(tx_witness) = guard
        .tape
        .txs
        .get(guard.tx_cursor)
        .filter(|tx| tx.tx_index == body_tx_index)
    else {
        return true;
    };
    if guard.replay_cursor == tx_witness.entries.len() {
        return true;
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    false
}

fn clear_uncommitted_witness(guard: &mut WitnessState) {
    guard.staged.clear();
    guard.replay_cursor = 0;
}

fn latched_fault_error(guard: &mut WitnessState) -> Option<BlockExecutionError> {
    let fault = guard.first_witness_fault?;
    clear_uncommitted_witness(guard);
    Some(witness_fault_to_block_error(
        fault,
        "witness fault during transaction execution",
    ))
}

fn candidate_rejection_error(rejection: CandidateRejection, tx_hash: B256) -> BlockExecutionError {
    match rejection {
        CandidateRejection::BudgetDeferred => crate::enclave_witness_budget_deferral(tx_hash),
        CandidateRejection::UnsignedEnclaveError => {
            BlockExecutionError::Validation(BlockValidationError::InvalidTx {
                hash: tx_hash,
                error: Box::new(InvalidTransaction::Str(
                    "unsigned enclave error during candidate execution".into(),
                )),
            })
        }
    }
}

fn record_proposer_tx_witness(
    guard: &mut WitnessState,
    body_tx_index: u32,
    base_extra_data: &[u8],
) -> Result<(), BlockExecutionError> {
    if guard.staged.is_empty() {
        return Ok(());
    }
    let entries = std::mem::take(&mut guard.staged);
    guard.tape.txs.push(TxWitnessV1 {
        tx_index: body_tx_index,
        entries,
    });
    if tape_fits_budget(&guard.tape, base_extra_data) {
        return Ok(());
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    Err(witness_fault_to_block_error(
        BlockWitnessFault::TapeInvalid,
        "committed transaction witness exceeded reserved budget",
    ))
}

fn consume_verifier_tx_witness(
    guard: &mut WitnessState,
    body_tx_index: u32,
) -> Result<(), BlockExecutionError> {
    let Some(expected_len) = guard
        .tape
        .txs
        .get(guard.tx_cursor)
        .filter(|tx| tx.tx_index == body_tx_index)
        .map(|tx| tx.entries.len())
    else {
        return Ok(());
    };
    if guard.replay_cursor == expected_len {
        guard.tx_cursor += 1;
        guard.replay_cursor = 0;
        return Ok(());
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    Err(witness_fault_to_block_error(
        BlockWitnessFault::TapeInvalid,
        format!(
            "unconsumed witness entries at tx_index={body_tx_index}: consumed {} of {expected_len}",
            guard.replay_cursor
        ),
    ))
}

fn record_committed_witness(
    guard: &mut WitnessState,
    body_tx_index: u32,
    base_extra_data: &[u8],
) -> Result<(), BlockExecutionError> {
    match guard.mode {
        WitnessExecutionMode::ProposerRecord => {
            record_proposer_tx_witness(guard, body_tx_index, base_extra_data)
        }
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
            consume_verifier_tx_witness(guard, body_tx_index)
        }
        WitnessExecutionMode::RpcDirect => Ok(()),
    }
}

fn record_prefix_if_it_fits(
    guard: &mut WitnessState,
    base_extra_data: &[u8],
) -> Result<(), BlockExecutionError> {
    let staged = std::mem::take(&mut guard.staged);
    guard.tape.prefix.extend(staged);
    if guard.tape.prefix.is_empty() || tape_fits_budget(&guard.tape, base_extra_data) {
        return Ok(());
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    Err(witness_fault_to_block_error(
        BlockWitnessFault::TapeInvalid,
        "mandatory pre-execution witness prefix exceeds reserved budget",
    ))
}

fn require_prefix_consumed(guard: &mut WitnessState) -> Result<(), BlockExecutionError> {
    if guard.prefix_cursor == guard.tape.prefix.len() {
        return Ok(());
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    Err(witness_fault_to_block_error(
        BlockWitnessFault::TapeInvalid,
        format!(
            "unconsumed pre-execution witness prefix entries: consumed {} of {}",
            guard.prefix_cursor,
            guard.tape.prefix.len()
        ),
    ))
}

fn apply_finalized_prefix(
    guard: &mut WitnessState,
    base_extra_data: &[u8],
) -> Result<(), BlockExecutionError> {
    match guard.mode {
        WitnessExecutionMode::ProposerRecord => record_prefix_if_it_fits(guard, base_extra_data),
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
            require_prefix_consumed(guard)
        }
        WitnessExecutionMode::RpcDirect => Ok(()),
    }
}

fn require_tape_fully_consumed(guard: &mut WitnessState) -> Result<(), BlockExecutionError> {
    if guard.prefix_cursor == guard.tape.prefix.len()
        && guard.tx_cursor == guard.tape.txs.len()
        && guard.replay_cursor == 0
    {
        return Ok(());
    }
    guard.latch(BlockWitnessFault::TapeInvalid);
    Err(witness_fault_to_block_error(
        BlockWitnessFault::TapeInvalid,
        format!(
            "unconsumed witness tape entries at finish: prefix={}/{} txs={}/{} replay_cursor={}",
            guard.prefix_cursor,
            guard.tape.prefix.len(),
            guard.tx_cursor,
            guard.tape.txs.len(),
            guard.replay_cursor
        ),
    ))
}

fn require_frozen_proposer_witness(
    guard: &mut WitnessState,
    frozen: Option<&Bytes>,
    block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    validate_execution_summary: bool,
) -> Result<(), BlockExecutionError> {
    if let Some(frozen) = frozen {
        if block_artifacts.enclave_witness.as_ref() == Some(frozen) {
            return Ok(());
        }
        guard.latch(BlockWitnessFault::TapeInvalid);
        return Err(witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            "final header enclave_witness does not match frozen proposer witness",
        ));
    }
    if (!guard.tape.prefix.is_empty() || !guard.tape.txs.is_empty()) && validate_execution_summary {
        guard.latch(BlockWitnessFault::TapeInvalid);
        return Err(witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            "proposer recorded enclave witness entries but final extra_data omitted enclave_witness",
        ));
    }
    Ok(())
}

fn validate_finished_tape(
    guard: &mut WitnessState,
    frozen: Option<&Bytes>,
    block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    validate_execution_summary: bool,
) -> Result<(), BlockExecutionError> {
    match guard.mode {
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
            require_tape_fully_consumed(guard)
        }
        WitnessExecutionMode::ProposerRecord => require_frozen_proposer_witness(
            guard,
            frozen,
            block_artifacts,
            validate_execution_summary,
        ),
        WitnessExecutionMode::RpcDirect => Ok(()),
    }
}

pub(in crate::executor) const MAX_DECOMPRESSED_WITNESS_BYTES: usize = 262_144;

pub(in crate::executor) fn witness_fault_to_block_error(
    fault: BlockWitnessFault,
    detail: impl Into<String>,
) -> BlockExecutionError {
    let reason = match fault {
        BlockWitnessFault::TapeInvalid => WitnessAbortReason::TapeInvalid,
        BlockWitnessFault::SemanticMismatch => WitnessAbortReason::SemanticMismatch,
        BlockWitnessFault::LocalUnavailable => WitnessAbortReason::LocalUnavailable,
    };
    let abort = NodeLocalExecutionAbort::witness(reason, detail);
    let evm_error: revm::context_interface::result::EVMError<std::convert::Infallible> =
        revm::context_interface::result::EVMError::CustomAny(
            revm::context_interface::result::AnyError::new(abort),
        );
    BlockExecutionError::Internal(InternalBlockExecutionError::Other(Box::new(evm_error)))
}

pub(in crate::executor) fn tape_fits_budget(
    tape: &EnclaveWitnessTapeV1,
    base_extra_data: &[u8],
) -> bool {
    let Ok(raw) = postcard::to_allocvec(tape) else {
        return false;
    };
    if raw.is_empty() || raw.len() > MAX_DECOMPRESSED_WITNESS_BYTES {
        return false;
    }
    let bound = compressed_witness_bound(raw.len());
    if bound > u16::MAX as usize || bound.saturating_add(3) > ENCLAVE_WITNESS_RESERVED_BYTES {
        return false;
    }
    let Ok(mut artifacts) = decode_outbe_block_artifacts(base_extra_data) else {
        return false;
    };
    artifacts.enclave_witness = None;
    artifacts.execution_summary = Some(ExecutionSummaryArtifact {
        validator_fee_sum: U256::MAX,
    });
    artifacts.timestamp_millis_part = 999;
    let Ok(encoded_base) = encode_outbe_block_artifacts(&artifacts) else {
        return false;
    };
    let base_len = encoded_base.len().max(7);
    base_len.saturating_add(3).saturating_add(bound) <= OUTBE_MAX_EXTRA_DATA_SIZE
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(in crate::executor) fn is_verifier_witness_mode(&self) -> bool {
        !self.expected_begin_system_txs.is_empty() || self.block_hash.is_some()
    }

    pub(in crate::executor) fn has_active_consensus_witness_scope(&self) -> bool {
        if outbe_tee::call_context::current_witness_state().is_some() {
            return true;
        }
        self.witness_state.as_ref().is_some_and(|state| {
            state
                .lock()
                .map(|guard| guard.mode != WitnessExecutionMode::RpcDirect)
                .unwrap_or(true)
        })
    }

    pub(in crate::executor) fn init_rpc_direct_witness_state(&mut self) {
        self.witness_state = Some(Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::RpcDirect,
            None,
            EnclaveWitnessTapeV1::default(),
        ))));
    }

    pub(in crate::executor) fn enter_prefix_witness_scope(
        &self,
    ) -> Result<Option<WitnessExecutionScope>, BlockExecutionError> {
        let Some(state) = self.witness_state.as_ref() else {
            return Ok(None);
        };
        {
            let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
            if let Some(fault) = guard.first_witness_fault {
                return Err(witness_fault_to_block_error(
                    fault,
                    "witness fault before pre-execution prefix",
                ));
            }
            guard.tx_index = None;
            guard.is_user_candidate = false;
            guard.replay_cursor = 0;
            guard.staged.clear();
            guard.candidate_rejection = None;
        }
        Ok(Some(WitnessExecutionScope::enter(state.clone())))
    }

    pub(in crate::executor) fn finalize_prefix_witness(
        &mut self,
    ) -> Result<(), BlockExecutionError> {
        let Some(state) = self.witness_state.clone() else {
            return Ok(());
        };
        let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
        if let Some(fault) = guard.first_witness_fault {
            guard.staged.clear();
            return Err(witness_fault_to_block_error(
                fault,
                "witness fault during pre-execution prefix",
            ));
        }
        apply_finalized_prefix(&mut guard, self.block_extra_data.as_ref())
    }

    pub(in crate::executor) fn advance_preexecuted_phase1_witness_index(
        &mut self,
    ) -> Result<(), BlockExecutionError> {
        if let Some(state) = self.witness_state.as_ref() {
            let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
            if let Some(fault) = guard.first_witness_fault {
                return Err(witness_fault_to_block_error(
                    fault,
                    "witness fault before pre-executed phase 1 body tx",
                ));
            }
            if matches!(
                guard.mode,
                WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay
            ) && guard
                .tape
                .txs
                .get(guard.tx_cursor)
                .is_some_and(|tx| tx.tx_index == self.body_tx_index)
            {
                guard.latch(BlockWitnessFault::TapeInvalid);
                return Err(witness_fault_to_block_error(
                    BlockWitnessFault::TapeInvalid,
                    "pre-executed phase 1 cannot carry a body tx witness segment",
                ));
            }
        }
        self.body_tx_index = self.body_tx_index.saturating_add(1);
        Ok(())
    }

    pub(in crate::executor) fn prepare_tx_witness_scope(
        &self,
        is_user_candidate: bool,
    ) -> Result<Option<WitnessExecutionScope>, BlockExecutionError> {
        let Some(state) = self.witness_state.as_ref() else {
            return Ok(None);
        };
        {
            let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
            if let Some(fault) = guard.first_witness_fault {
                return Err(witness_fault_to_block_error(
                    fault,
                    "witness fault before transaction execution",
                ));
            }
            guard.tx_index = Some(self.body_tx_index);
            guard.is_user_candidate =
                is_user_candidate && guard.mode == WitnessExecutionMode::ProposerRecord;
            guard.replay_cursor = 0;
            guard.staged.clear();
            guard.candidate_rejection = None;
        }
        Ok(Some(WitnessExecutionScope::enter(state.clone())))
    }

    pub(in crate::executor) fn check_witness_commit_allowed_with(
        witness_state: Option<&Arc<Mutex<WitnessState>>>,
        body_tx_index: u32,
        base_extra_data: &[u8],
        is_user_candidate: bool,
    ) -> bool {
        let Some(state) = witness_state else {
            return true;
        };
        let Ok(mut guard) = state.lock() else {
            return false;
        };
        if guard.first_witness_fault.is_some() || guard.candidate_rejection.is_some() {
            return false;
        }
        match guard.mode {
            WitnessExecutionMode::ProposerRecord => proposer_commit_fits(
                &mut guard,
                body_tx_index,
                base_extra_data,
                is_user_candidate,
            ),
            WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
                verifier_commit_consumed(&mut guard, body_tx_index)
            }
            WitnessExecutionMode::RpcDirect => true,
        }
    }

    pub(in crate::executor) fn check_no_witness_fault(&self) -> Result<(), BlockExecutionError> {
        let Some(state) = self.witness_state.as_ref() else {
            return Ok(());
        };
        let guard = state.lock().map_err(|_| witness_lock_poisoned())?;
        if let Some(fault) = guard.first_witness_fault {
            return Err(witness_fault_to_block_error(
                fault,
                "witness fault during transaction execution",
            ));
        }
        Ok(())
    }

    pub(in crate::executor) fn finish_tx_witness(
        &mut self,
        tx_hash: B256,
        committed: bool,
    ) -> Result<(), BlockExecutionError> {
        let Some(state) = self.witness_state.clone() else {
            if committed {
                self.body_tx_index = self.body_tx_index.saturating_add(1);
            }
            return Ok(());
        };
        let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
        if let Some(error) = latched_fault_error(&mut guard) {
            return Err(error);
        }
        if let Some(rejection) = guard.candidate_rejection.take() {
            clear_uncommitted_witness(&mut guard);
            return Err(candidate_rejection_error(rejection, tx_hash));
        }
        if committed {
            record_committed_witness(
                &mut guard,
                self.body_tx_index,
                self.block_extra_data.as_ref(),
            )?;
            self.body_tx_index = self.body_tx_index.saturating_add(1);
        } else {
            clear_uncommitted_witness(&mut guard);
        }
        Ok(())
    }

    pub(in crate::executor) fn validate_finish_witness(
        &self,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let Some(state) = self.witness_state.as_ref() else {
            return Ok(());
        };
        let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
        if let Some(fault) = guard.first_witness_fault {
            return Err(witness_fault_to_block_error(
                fault,
                "block finished with latched witness fault",
            ));
        }
        validate_finished_tape(
            &mut guard,
            self.frozen_enclave_witness.as_ref(),
            block_artifacts,
            self.validate_execution_summary,
        )
    }
}
