//! Pre-end admission and the terminal call/cleanup execution phase.

use super::*;
use crate::config::system_txs::sign_end_system_tx;
use crate::errors::ExecutorValidationError;
use outbe_primitives::end_gas::{expected_call_event_gas_limit, expected_cleanup_gas_limit};
use outbe_primitives::system_tx::{
    build_unsigned_system_tx_with_gas_limit, BlockGasBudget, SystemTx, SystemTxEnvelopeInput,
    SystemTxPhase, CALL_EVENT_PROCESSING_ORDINAL, CLEANUP_PROCESSING_ORDINAL,
};
use reth_primitives_traits::Recovered;

struct AdmittedEndTx {
    input: SystemTx,
    ordinal: u8,
    gas_limit: u64,
    capacity: u64,
}

struct EndTxCheck {
    block_number: u64,
    phase: SystemTxPhase,
    chain_id: u64,
    proposer: Address,
}

fn admit_end_tx(
    tx: &TransactionSigned,
    signer: Address,
    check: EndTxCheck,
) -> Result<AdmittedEndTx, BlockExecutionError> {
    if !BlockHeightRegime::for_block(check.block_number).has_end_zone_system_txs() {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let input = SystemTx::decode(tx.input().as_ref())
        .map_err(|error| ExecutorValidationError::decode_system_tx_at_body_index(0, error))?;
    if input.body_zone() != outbe_primitives::system_tx::BodyZone::EndBlock {
        return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
    }
    let ordinal = input
        .nonce_ordinal()
        .ok_or(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed)?;
    let (capacity, gas_limit) =
        expected_end_gas_limit(&input, ordinal, check.phase, tx.gas_limit())?;
    if tx.gas_limit() != gas_limit {
        return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
            kind: input.name(),
            expected: gas_limit,
            actual: tx.gas_limit(),
            capacity,
        }
        .into());
    }
    if signer != check.proposer {
        return Err(ExecutorValidationError::SystemTxSignerMismatch {
            body_index: usize::from(ordinal),
            expected_phase: input.name(),
            proposer: check.proposer,
            signer,
        }
        .into());
    }
    let unsigned = build_unsigned_system_tx_with_gas_limit(SystemTxEnvelopeInput {
        ordinal,
        block_number: check.block_number,
        chain_id: check.chain_id,
        calldata: tx.input().clone(),
        gas_limit,
    })
    .map_err(ExecutorValidationError::plan_visible_system_tx_gas)?;
    if tx.signature_hash() != unsigned.signature_hash() {
        return Err(ExecutorValidationError::SystemTxSignatureHashMismatch {
            body_index: usize::from(ordinal),
            expected_phase: input.name(),
        }
        .into());
    }
    Ok(AdmittedEndTx {
        input,
        ordinal,
        gas_limit,
        capacity,
    })
}

fn expected_end_gas_limit(
    input: &SystemTx,
    ordinal: u8,
    phase: SystemTxPhase,
    signed_limit: u64,
) -> Result<(u64, u64), BlockExecutionError> {
    let (capacity, required) = match (phase, input) {
        (SystemTxPhase::EndBlockCallEvents { gas_capacity }, SystemTx::CallEventProcessing)
            if ordinal == CALL_EVENT_PROCESSING_ORDINAL =>
        {
            (gas_capacity, expected_call_event_gas_limit(gas_capacity))
        }
        (
            SystemTxPhase::EndBlockCleanup {
                gas_capacity,
                call_gas_used,
            },
            SystemTx::CleanupProcessing,
        ) if ordinal == CLEANUP_PROCESSING_ORDINAL => (
            gas_capacity,
            expected_cleanup_gas_limit(gas_capacity, call_gas_used),
        ),
        (_, SystemTx::CallEventProcessing | SystemTx::CleanupProcessing) => {
            let expected_phase = match phase {
                SystemTxPhase::EndBlockCallEvents { .. } => "CallEventProcessing",
                SystemTxPhase::EndBlockCleanup { .. } => "CleanupProcessing",
                _ => "EndBlock",
            };
            return Err(ExecutorValidationError::SystemTxPhaseMismatch {
                body_index: usize::from(ordinal),
                expected_phase,
                actual_phase: input.name(),
            }
            .into());
        }
        _ => {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
    };
    let limit = required.ok_or(ExecutorValidationError::EndBlockGasLimitMismatch {
        kind: input.name(),
        expected: 0,
        actual: signed_limit,
        capacity,
    })?;
    Ok((capacity, limit))
}

impl<E: Evm> OutbeBlockExecutor<'_, E> {
    fn block_gas_budget(&self) -> Result<BlockGasBudget, BlockExecutionError> {
        BlockGasBudget::new(self.inner.evm.block().gas_limit())
            .map_err(|error| ExecutorValidationError::from(error).into())
    }

    pub(super) fn accounted_block_gas(&self) -> u64 {
        if self.inner.evm.cfg_env().enable_amsterdam_eip8037 {
            self.inner.max_block_gas_used()
        } else {
            self.inner.cumulative_tx_gas_used
        }
    }

    pub fn gas_accounting(&self) -> (u64, u64, u64) {
        (
            self.inner.cumulative_tx_gas_used,
            self.inner.block_regular_gas_used,
            self.inner.block_state_gas_used,
        )
    }

    pub fn tx_gas_limit_cap(&self) -> u64
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        use revm::context_interface::Cfg as _;
        self.inner.evm.cfg_env().tx_gas_limit_cap()
    }

    /// Builder stage after user transactions and before execution finalization.
    /// Repeated calls retain the captured capacity and do not execute the suffix twice.
    pub fn apply_end_block_phase<DB>(&mut self) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        if matches!(self.system_tx_phase_cursor, SystemTxPhase::EndBlock { .. }) {
            return Ok(());
        }
        let phase = self.enter_end_block_phase_if_needed()?;
        if let SystemTxPhase::EndBlockCallEvents { gas_capacity } = phase {
            self.execute_built_end_suffix(gas_capacity)?;
        }
        Ok(())
    }

    fn enter_end_block_phase_if_needed(&mut self) -> Result<SystemTxPhase, BlockExecutionError> {
        if matches!(
            self.system_tx_phase_cursor,
            SystemTxPhase::EndBlockCallEvents { .. }
                | SystemTxPhase::EndBlockCleanup { .. }
                | SystemTxPhase::EndBlock { .. }
        ) {
            return Ok(self.system_tx_phase_cursor);
        }
        let capacity = self
            .block_gas_budget()?
            .end_capacity(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let next = if BlockHeightRegime::for_block(block_number).has_end_zone_system_txs() {
            SystemTxPhase::EndBlockCallEvents {
                gas_capacity: capacity,
            }
        } else {
            SystemTxPhase::EndBlock {
                gas_capacity: capacity,
            }
        };
        self.system_tx_phase_cursor = next;
        Ok(next)
    }

    fn end_block_proposer(&self, block_number: u64) -> Result<Address, BlockExecutionError> {
        if BlockHeightRegime::for_block(block_number).is_genesis() {
            return Err(ExecutorValidationError::MissingBeginZoneProposerAddress.into());
        }
        self.proposer_evm_address
            .or_else(|| {
                self.expected_begin_system_txs
                    .first()
                    .map(|tx| Address::from(*tx.signer()))
            })
            .or_else(|| self.evm_signer.as_ref().map(|signer| signer.address()))
            .ok_or_else(|| ExecutorValidationError::MissingBeginZoneProposerAddress.into())
    }

    pub(crate) fn take_staged_end_block_txs(&mut self) -> Vec<Recovered<TransactionSigned>> {
        std::mem::take(&mut self.staged_end_block_txs)
    }

    fn execute_built_end_suffix<DB>(&mut self, capacity: u64) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        let call_limit = expected_call_event_gas_limit(capacity).ok_or(
            ExecutorValidationError::EndBlockGasLimitMismatch {
                kind: "CallEventProcessing",
                expected: 0,
                actual: capacity,
                capacity,
            },
        )?;
        let call_tx = self.sign_end_tx(SystemTx::CallEventProcessing, call_limit)?;
        self.execute_end_block_system_tx(call_tx.tx(), Address::from(*call_tx.signer()))?;
        let SystemTxPhase::EndBlockCleanup { call_gas_used, .. } = self.system_tx_phase_cursor
        else {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        };
        let cleanup_limit = expected_cleanup_gas_limit(capacity, call_gas_used).ok_or(
            ExecutorValidationError::EndBlockGasLimitMismatch {
                kind: "CleanupProcessing",
                expected: 0,
                actual: call_gas_used,
                capacity,
            },
        )?;
        let cleanup_tx = self.sign_end_tx(SystemTx::CleanupProcessing, cleanup_limit)?;
        self.execute_end_block_system_tx(cleanup_tx.tx(), Address::from(*cleanup_tx.signer()))?;
        self.staged_end_block_txs = vec![call_tx, cleanup_tx];
        Ok(())
    }

    fn sign_end_tx<DB>(
        &self,
        input: SystemTx,
        gas_limit: u64,
    ) -> Result<Recovered<TransactionSigned>, BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
    {
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let proposer = self.end_block_proposer(block_number)?;
        sign_end_system_tx(
            self.evm_signer.as_ref(),
            proposer,
            block_number,
            self.chain_id,
            input,
            gas_limit,
        )
    }

    pub(in crate::executor) fn execute_end_block_system_tx<DB>(
        &mut self,
        tx: &TransactionSigned,
        signer: Address,
    ) -> Result<Option<GasOutput>, BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        if !BlockHeightRegime::for_block(block_number).has_end_zone_system_txs() {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
        let phase = self.enter_end_block_phase_if_needed()?;
        let proposer = self.end_block_proposer(block_number)?;
        let admitted = admit_end_tx(
            tx,
            signer,
            EndTxCheck {
                block_number,
                phase,
                chain_id: self.chain_id,
                proposer,
            },
        )?;
        let input = admitted.input;
        let ordinal = admitted.ordinal;
        let expected_limit = admitted.gas_limit;
        let capacity = admitted.capacity;
        let transact_outcome = with_preloaded_system_tx_context(
            PreloadedSystemTxContext::for_end_block(proposer, expected_limit),
            || {
                self.inner.evm.transact_system_call(
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                    outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                    tx.input().clone(),
                )
            },
        );
        let charged = match transact_outcome {
            Ok(result) if result.result.is_success() => {
                let output = EthTxResult {
                    result,
                    blob_gas_used: 0,
                    tx_type: tx.tx_type(),
                };
                self.inner.commit_transaction(output).tx_gas_used()
            }
            Ok(result) => {
                let code = super::system_execution::execute::system_tx_failure_code_for_result(
                    &result.result,
                );
                let reason = format!(
                    "system tx {} did not succeed at body_index={}: {:?}",
                    input.name(),
                    ordinal,
                    result.result
                );
                let gas_used = result.result.tx_gas_used();
                drop(result);
                self.commit_end_block_failure_receipt(tx, code, reason, gas_used)
            }
            Err(error) => {
                if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_some()
                    || outbe_primitives::projection::NodeLocalExecutionAbort::find(&error).is_some()
                {
                    return Err(BlockExecutionError::other(error));
                }
                let reason = format!(
                    "system tx {} execution failed at body_index={ordinal}: {error}",
                    input.name()
                );
                self.commit_end_block_failure_receipt(tx, 299, reason, 0)
            }
        };
        match input {
            SystemTx::CallEventProcessing => {
                self.system_tx_phase_cursor = SystemTxPhase::EndBlockCleanup {
                    gas_capacity: capacity,
                    call_gas_used: charged,
                };
            }
            SystemTx::CleanupProcessing => {
                let SystemTxPhase::EndBlockCleanup { call_gas_used, .. } = phase else {
                    return Err(
                        ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into(),
                    );
                };
                if call_gas_used.saturating_add(charged) > capacity {
                    return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
                        kind: "CleanupProcessing",
                        expected: capacity.saturating_sub(call_gas_used),
                        actual: charged,
                        capacity,
                    }
                    .into());
                }
                self.system_tx_phase_cursor = SystemTxPhase::EndBlock {
                    gas_capacity: capacity,
                };
            }
            _ => {}
        }
        Ok(Some(GasOutput::new(charged)))
    }

    fn commit_end_block_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        code: u16,
        reason: String,
        gas_used: u64,
    ) -> u64 {
        let log = crate::failure_receipt::build_outbe_failure_log(
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
        );
        self.inner.cumulative_tx_gas_used =
            self.inner.cumulative_tx_gas_used.saturating_add(gas_used);
        self.inner.block_regular_gas_used =
            self.inner.block_regular_gas_used.saturating_add(gas_used);
        self.inner.receipts.push(Receipt {
            tx_type: tx.tx_type(),
            success: false,
            cumulative_gas_used: self.inner.cumulative_tx_gas_used,
            logs: vec![log],
        });
        gas_used
    }

    pub(in crate::executor) fn check_visible_pre_end_gas(
        &self,
        additional: u64,
    ) -> Result<(), BlockExecutionError> {
        let available = self
            .block_gas_budget()?
            .remaining_pre_end_gas(self.accounted_block_gas())
            .map_err(ExecutorValidationError::from)?;
        check_capacity(additional, additional, available)
    }

    pub(in crate::executor) fn check_user_pre_end_gas(
        &self,
        gas_limit: u64,
    ) -> Result<(), BlockExecutionError>
    where
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        use revm::context_interface::Cfg;
        let budget = self.block_gas_budget()?;
        let amsterdam = self.inner.evm.cfg_env().enable_amsterdam_eip8037;
        let regular_used = if amsterdam {
            self.inner.block_regular_gas_used
        } else {
            self.inner.cumulative_tx_gas_used
        };
        let available = budget
            .remaining_pre_end_gas(regular_used)
            .map_err(ExecutorValidationError::from)?;
        let regular_limit = gas_limit.min(self.inner.evm.cfg_env().tx_gas_limit_cap());
        check_capacity(gas_limit, regular_limit, available)?;
        if amsterdam {
            let available = budget
                .remaining_pre_end_gas(self.inner.block_state_gas_used)
                .map_err(ExecutorValidationError::from)?;
            check_capacity(gas_limit, gas_limit, available)?;
        }
        Ok(())
    }
}

fn check_capacity(declared: u64, required: u64, available: u64) -> Result<(), BlockExecutionError> {
    if required > available {
        return Err(
            BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: declared,
                block_available_gas: available,
            }
            .into(),
        );
    }
    Ok(())
}
