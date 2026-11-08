//! Terminal `EndBlock` system transaction execution phase (`END_BLOCK_ORDER`).

use super::*;
use crate::config::system_txs::sign_end_system_tx;
use crate::errors::ExecutorValidationError;
use outbe_primitives::system_tx::{
    build_unsigned_system_tx, SystemTx, SystemTxPhase, CREDIS_FACTORY_ORDINAL, GEM_CLEANUP_ORDINAL,
    GEM_FACTORY_ORDINAL, INTEX_FACTORY_ORDINAL, NOD_FACTORY_ORDINAL,
};
use reth_primitives_traits::Recovered;

struct AdmittedEndTx {
    input: SystemTx,
    ordinal: u8,
    next_phase: SystemTxPhase,
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
    let next_phase = expected_next_end_phase(&input, ordinal, check.phase)?;
    if tx.gas_limit() != 0 {
        return Err(ExecutorValidationError::EndBlockGasLimitMismatch {
            kind: input.name(),
            expected: 0,
            actual: tx.gas_limit(),
            capacity: 0,
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
    let unsigned = build_unsigned_system_tx(
        ordinal,
        check.block_number,
        check.chain_id,
        tx.input().clone(),
    )
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
        next_phase,
    })
}

fn expected_next_end_phase(
    input: &SystemTx,
    ordinal: u8,
    phase: SystemTxPhase,
) -> Result<SystemTxPhase, BlockExecutionError> {
    match (phase, input) {
        (SystemTxPhase::EndBlockNodFactory, SystemTx::NodFactory)
            if ordinal == NOD_FACTORY_ORDINAL =>
        {
            Ok(SystemTxPhase::EndBlockGemFactory)
        }
        (SystemTxPhase::EndBlockGemFactory, SystemTx::GemFactory)
            if ordinal == GEM_FACTORY_ORDINAL =>
        {
            Ok(SystemTxPhase::EndBlockIntexFactory)
        }
        (SystemTxPhase::EndBlockIntexFactory, SystemTx::IntexFactory)
            if ordinal == INTEX_FACTORY_ORDINAL =>
        {
            Ok(SystemTxPhase::EndBlockCredisFactory)
        }
        (SystemTxPhase::EndBlockCredisFactory, SystemTx::CredisFactory)
            if ordinal == CREDIS_FACTORY_ORDINAL =>
        {
            Ok(SystemTxPhase::EndBlockGemCleanup)
        }
        (SystemTxPhase::EndBlockGemCleanup, SystemTx::GemCleanup)
            if ordinal == GEM_CLEANUP_ORDINAL =>
        {
            Ok(SystemTxPhase::EndBlock)
        }
        _ => {
            let expected_phase = match phase {
                SystemTxPhase::EndBlockNodFactory => "NodFactory",
                SystemTxPhase::EndBlockGemFactory => "GemFactory",
                SystemTxPhase::EndBlockIntexFactory => "IntexFactory",
                SystemTxPhase::EndBlockCredisFactory => "CredisFactory",
                SystemTxPhase::EndBlockGemCleanup => "GemCleanup",
                _ => "EndBlock",
            };
            Err(ExecutorValidationError::SystemTxPhaseMismatch {
                body_index: usize::from(ordinal),
                expected_phase,
                actual_phase: input.name(),
            }
            .into())
        }
    }
}

impl<E: Evm> OutbeBlockExecutor<'_, E> {
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
    /// Repeated calls are idempotent and do not execute the suffix twice.
    pub fn apply_end_block_phase<DB>(&mut self) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        if matches!(self.system_tx_phase_cursor, SystemTxPhase::EndBlock) {
            return Ok(());
        }
        let phase = self.enter_end_block_phase_if_needed()?;
        if matches!(phase, SystemTxPhase::EndBlockNodFactory) {
            self.execute_built_end_suffix()?;
        }
        Ok(())
    }

    fn enter_end_block_phase_if_needed(&mut self) -> Result<SystemTxPhase, BlockExecutionError> {
        if matches!(
            self.system_tx_phase_cursor,
            SystemTxPhase::EndBlockNodFactory
                | SystemTxPhase::EndBlockGemFactory
                | SystemTxPhase::EndBlockIntexFactory
                | SystemTxPhase::EndBlockCredisFactory
                | SystemTxPhase::EndBlockGemCleanup
                | SystemTxPhase::EndBlock
        ) {
            return Ok(self.system_tx_phase_cursor);
        }
        if !matches!(self.system_tx_phase_cursor, SystemTxPhase::UserTxs) {
            return Err(ExecutorValidationError::ReservedSystemTxAfterBeginZoneConsumed.into());
        }
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let next = if BlockHeightRegime::for_block(block_number).has_end_zone_system_txs() {
            SystemTxPhase::EndBlockNodFactory
        } else {
            SystemTxPhase::EndBlock
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

    fn execute_built_end_suffix<DB>(&mut self) -> Result<(), BlockExecutionError>
    where
        E: Evm<DB = DB, Tx = TxEnv, HaltReason = HaltReason>,
        DB: StateDB,
        DB::Error: std::fmt::Display,
        E::Error: std::fmt::Display,
        E::Spec: Into<revm::primitives::hardfork::SpecId>,
    {
        let mut staged = Vec::with_capacity(5);
        for input in [
            SystemTx::NodFactory,
            SystemTx::GemFactory,
            SystemTx::IntexFactory,
            SystemTx::CredisFactory,
            SystemTx::GemCleanup,
        ] {
            let signed_tx = self.sign_end_tx(input)?;
            self.execute_end_block_system_tx(signed_tx.tx(), Address::from(*signed_tx.signer()))?;
            staged.push(signed_tx);
        }
        self.staged_end_block_txs = staged;
        Ok(())
    }

    fn sign_end_tx<DB>(
        &self,
        input: SystemTx,
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
        if self.execute_outbe_block_hooks {
            self.fsm
                .on_system_tx(input.kind())
                .map_err(ExecutorValidationError::from)?;
        }
        let block_gas_limit = self.inner.evm.block().gas_limit();
        let user_gas_used = self.accounted_block_gas();
        let transact_outcome = with_preloaded_system_tx_context(
            PreloadedSystemTxContext::for_end_block(proposer, block_gas_limit, user_gas_used),
            || {
                self.inner.evm.transact_system_call(
                    outbe_primitives::addresses::SYSTEM_ADDRESS,
                    outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
                    tx.input().clone(),
                )
            },
        );
        match transact_outcome {
            Ok(result) if result.result.is_success() => {
                let user_cumulative_tx_gas = self.inner.cumulative_tx_gas_used;
                let user_regular_gas = self.inner.block_regular_gas_used;
                let user_state_gas = self.inner.block_state_gas_used;
                let output = EthTxResult {
                    result,
                    blob_gas_used: 0,
                    tx_type: tx.tx_type(),
                };
                let _ = self.inner.commit_transaction(output);
                if let Some(receipt) = self.inner.receipts.last_mut() {
                    receipt.cumulative_gas_used = user_cumulative_tx_gas;
                }
                self.inner.cumulative_tx_gas_used = user_cumulative_tx_gas;
                self.inner.block_regular_gas_used = user_regular_gas;
                self.inner.block_state_gas_used = user_state_gas;
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
                drop(result);
                self.commit_end_block_failure_receipt(tx, code, reason);
            }
            Err(error) => {
                let reason = format!(
                    "system tx {} execution failed at body_index={ordinal}: {error}",
                    input.name()
                );
                if outbe_primitives::projection::ExecutionReadCancelled::find(&error).is_none()
                    && outbe_primitives::projection::NodeLocalExecutionAbort::find(&error).is_none()
                {
                    tracing::error!(target: "outbe::executor", %reason);
                }
                return Err(BlockExecutionError::other(error));
            }
        }
        self.system_tx_phase_cursor = admitted.next_phase;
        if matches!(admitted.next_phase, SystemTxPhase::EndBlock) && self.execute_outbe_block_hooks
        {
            self.fsm.finish().map_err(ExecutorValidationError::from)?;
        }
        Ok(Some(GasOutput::new(0)))
    }

    fn commit_end_block_failure_receipt(
        &mut self,
        tx: &TransactionSigned,
        code: u16,
        reason: String,
    ) {
        let log = crate::failure_receipt::build_outbe_failure_log(
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            code,
            reason,
        );
        self.inner.receipts.push(Receipt {
            tx_type: tx.tx_type(),
            success: false,
            cumulative_gas_used: self.inner.cumulative_tx_gas_used,
            logs: vec![log],
        });
    }
}
