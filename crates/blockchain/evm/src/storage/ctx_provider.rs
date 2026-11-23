//! `CtxStorageProvider` - sub-call-driver-aware `PrecompileStorageProvider`.
//!
//! The provider holds `&'a mut evm2::Evm<'evm, OutbeEvmTypes>` directly so both
//! journaled storage operations and nested sub-calls run against the live EVM
//! instance with full checkpoint and rollback semantics.
//!
//! ## Storage gas schedule
//!
//! Each persistent read costs 100 gas. Each persistent write costs 5000 gas.
//! These flat costs do not depend on slot warmth or the old and new values.
//! Transient reads, transient writes, and `enclave_upgrade_id` do not charge this meter.
//! These prices define current behavior, not a resource-pricing benchmark.

use alloy_primitives::{Address, Log, LogData, B256, U256};
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, StateCheckpoint},
    interpreter::{
        gas::{SSTORE_RESET, WARM_STORAGE_READ_COST},
        Host as _,
    },
    Evm, SpecId,
};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::{
        MutationEntitlements, MutationPurposeTag, PrecompileStorageProvider, SubCallError,
        SubCallInput, SubCallOutput, SubCallStatus,
    },
};
use std::cell::RefCell;

use crate::{factory::OutbeEvmTypes, gas::SubcallGasMeter};

mod subcall;
mod trigger_gas;

thread_local! {
    /// Per-thread reentrancy stack. It records the outbe precompile addresses
    /// that are currently dispatched on this call chain. The EVM processes one
    /// transaction synchronously per thread. Thus a thread-local stack is the
    /// natural scope: it lives exactly as long as the active tx execution.
    /// RAII Drop on [`ReentrancyGuard`] resets it (empty) at the end of every
    /// dispatch chain.
    static REENTRANCY_STACK: RefCell<Vec<Address>> = const { RefCell::new(Vec::new()) };
}

/// Zero-sized marker that stays a [`CtxStorageProvider`] field, so the
/// struct layout does not change. The actual stack lives
/// in the thread-local [`struct@REENTRANCY_STACK`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ReentrancyStack;

impl ReentrancyStack {
    /// Attempts to push `addr` onto the active reentrancy stack.
    ///
    /// Returns `Some(ReentrancyGuard)` on first entry. Returns `None` if `addr`
    /// is already on the stack (the caller must reject the dispatch).
    pub fn try_enter(addr: Address) -> Option<ReentrancyGuard> {
        REENTRANCY_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.contains(&addr) {
                None
            } else {
                stack.push(addr);
                Some(ReentrancyGuard { addr })
            }
        })
    }

    /// Returns true if `addr` is currently on the active reentrancy stack
    /// for this thread.
    #[doc(hidden)]
    pub fn contains(addr: Address) -> bool {
        REENTRANCY_STACK.with(|s| s.borrow().contains(&addr))
    }

    /// Returns the current stack depth for this thread.
    #[doc(hidden)]
    pub fn depth() -> usize {
        REENTRANCY_STACK.with(|s| s.borrow().len())
    }
}

/// RAII guard returned by [`ReentrancyStack::try_enter`]. Pops the registered
/// address from the thread-local reentrancy stack on Drop.
#[derive(Debug)]
pub struct ReentrancyGuard {
    addr: Address,
}

impl Drop for ReentrancyGuard {
    fn drop(&mut self) {
        REENTRANCY_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if let Some(pos) = stack.iter().rposition(|a| a == &self.addr) {
                stack.remove(pos);
            }
        });
    }
}

/// Sub-call-driver-aware [`PrecompileStorageProvider`] over a borrowed
/// [`evm2::Evm`].
pub struct CtxStorageProvider<'a, 'evm> {
    /// Borrowed EVM instance. Sub-call hands this directly to [`crate::sub_call`].
    pub evm: &'a mut Evm<'evm, OutbeEvmTypes>,
    /// Per-call gas meter (mirrors `evm2::interpreter::Gas`).
    pub gas: SubcallGasMeter,
    /// Admission OOG cannot be caught by a domain `try_call` handler.
    pub subcall_out_of_gas: bool,
    /// STATICCALL flag forwarded from the outer dispatcher.
    pub is_static: bool,
    /// Address of the outbe precompile the dispatcher is currently running.
    pub self_address: Address,
    /// Reentrancy stack marker (real state lives in thread-local).
    pub reentrancy_stack: ReentrancyStack,
    /// EVM spec id, captured at provider construction time.
    pub spec: SpecId,
    /// Immutable genesis hash sourced from the canonical ChainSpec.
    pub genesis_hash: B256,
    /// Immutable genesis timestamp sourced from the canonical ChainSpec.
    pub genesis_timestamp: u64,
    /// Node-local failure bridge propagated to nested precompiles.
    pub abort_bridge: crate::precompiles::ExecutionAbortBridge,
    /// Runtime host for nested precompile dispatch.
    host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
    /// One-shot authority for the active scoped mutation frame.
    dispatch_frames: DispatchFrameAuthority,
    /// Active journal checkpoints `(token, raw_evm_checkpoint, trigger_refund)` in LIFO order.
    checkpoints: Vec<(StateCheckpoint, StateCheckpoint, Option<i64>)>,
    pub trigger_budget: Option<outbe_primitives::storage::gas::TriggerBudget>,
    /// Remaining gas of the active resumable-unit child frame.
    pub gas_child_remaining: Option<u64>,
}

impl CtxStorageProviderConfig {
    pub const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }
}

pub struct CtxStorageProviderConfig {
    pub is_static: bool,
    pub self_address: Address,
    pub reentrancy_stack: ReentrancyStack,
    pub spec: SpecId,
    pub genesis_hash: B256,
    pub genesis_timestamp: u64,
    pub abort_bridge: crate::precompiles::ExecutionAbortBridge,
    pub scoped_mutation_entitlements: MutationEntitlements,
}

struct DispatchFrameAuthority {
    scoped: ScopedMutationFrameState,
}

#[derive(Debug)]
struct ScopedMutationFrameState {
    entitlements: MutationEntitlements,
    active: Option<(MutationPurposeTag, B256, u64, u64)>,
}

impl ScopedMutationFrameState {
    const fn new(entitlements: MutationEntitlements) -> Self {
        Self {
            entitlements,
            active: None,
        }
    }

    fn begin(
        &mut self,
        purpose: MutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.active.is_some() || !self.entitlements.consume(purpose, binding) {
            return Err(
                crate::errors::CtxStorageError::ScopedMutationPurposeNotEntitled {
                    purpose,
                    binding,
                    remaining: Box::new(self.entitlements),
                }
                .into(),
            );
        }
        self.active = Some((purpose, binding, chain_id, block_number));
        Ok(())
    }

    fn finish(
        &mut self,
        purpose: MutationPurposeTag,
        binding: B256,
    ) -> outbe_primitives::error::Result<()> {
        let Some((active_purpose, active_binding, _, _)) = self.active.as_ref().copied() else {
            return Err(crate::errors::CtxStorageError::ScopedMutationFrameNotActive.into());
        };
        if active_purpose != purpose || active_binding != binding {
            return Err(crate::errors::CtxStorageError::ScopedMutationFrameIdentityMismatch.into());
        }
        self.active = None;
        Ok(())
    }
}

impl<'a, 'evm> CtxStorageProvider<'a, 'evm> {
    /// Constructor.
    pub fn new(
        evm: &'a mut Evm<'evm, OutbeEvmTypes>,
        gas: SubcallGasMeter,
        config: CtxStorageProviderConfig,
        host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
    ) -> Self {
        Self {
            evm,
            gas,
            subcall_out_of_gas: false,
            is_static: config.is_static,
            self_address: config.self_address,
            reentrancy_stack: config.reentrancy_stack,
            spec: config.spec,
            genesis_hash: config.genesis_hash,
            genesis_timestamp: 0,
            abort_bridge: config.abort_bridge,
            host,
            dispatch_frames: DispatchFrameAuthority {
                scoped: ScopedMutationFrameState::new(config.scoped_mutation_entitlements),
            },
            checkpoints: Vec::new(),
            trigger_budget: None,
            gas_child_remaining: None,
        }
        .with_genesis_timestamp(config.genesis_timestamp)
    }

    pub const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }

    pub fn set_runtime_host(&mut self, host: std::sync::Arc<dyn crate::EvmRuntimeHost>) {
        self.host = host;
    }

    pub fn replace_scoped_mutation_entitlements(&mut self, entitlements: MutationEntitlements) {
        self.dispatch_frames.scoped = ScopedMutationFrameState::new(entitlements);
    }
}

impl PrecompileStorageProvider for CtxStorageProvider<'_, '_> {
    fn begin_scoped_mutation_frame(
        &mut self,
        purpose: MutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.trigger_budget.is_some() {
            self.trigger_budget_status()?;
        }
        if self.evm.version().chain_id != chain_id
            || self.evm.block().number.saturating_to::<u64>() != block_number
        {
            return Err(crate::errors::CtxStorageError::ScopedMutationFrameScopeMismatch.into());
        }
        self.dispatch_frames
            .scoped
            .begin(purpose, binding, chain_id, block_number)
    }

    fn require_scoped_mutation_frame(
        &mut self,
        purpose: MutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.dispatch_frames.scoped.active != Some((purpose, binding, chain_id, block_number)) {
            return Err(crate::errors::CtxStorageError::ScopedMutationFrameIdentityMismatch.into());
        }
        Ok(())
    }

    fn finish_scoped_mutation_frame(
        &mut self,
        purpose: MutationPurposeTag,
        binding: B256,
        _completed: bool,
    ) -> outbe_primitives::error::Result<()> {
        self.dispatch_frames.scoped.finish(purpose, binding)
    }

    fn chain_id(&self) -> u64 {
        self.evm.version().chain_id
    }

    fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }

    fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    fn timestamp(&self) -> U256 {
        self.evm.block().timestamp
    }

    fn beneficiary(&self) -> Address {
        self.evm.block().beneficiary
    }

    fn block_number(&self) -> u64 {
        self.evm.block().number.saturating_to::<u64>()
    }

    fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
        self.charge(0)?;
        let hash = self
            .evm
            .block_hash(&U256::from(number))
            .map_err(|e| crate::errors::CtxStorageError::block_hash_failed(number, e))?;
        Ok((!hash.is_zero()).then_some(hash))
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        self.charge(0)?;
        self.evm
            .state_mut()
            .account(&address)
            .map_err(crate::errors::CtxStorageError::storage)?
            .set_code_slow(code);
        Ok(())
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        self.charge(0)?;
        let mut account = self
            .evm
            .state_mut()
            .account(&address)
            .map_err(crate::errors::CtxStorageError::storage)?;
        let mut info = account.get().cloned().unwrap_or_default();
        // Hydrate bytecode when absent on the account overlay: callers such as
        // stablecoinfactory inspect `AccountInfo.code` to authenticate deployed
        // contracts and reject `None`.
        if info.code.is_none() {
            info.code = Some(if info.is_empty_code_hash() {
                Bytecode::default()
            } else {
                account
                    .load_code()
                    .map_err(crate::errors::CtxStorageError::storage)?
            });
        }
        Ok(info)
    }

    fn metered_gas_remaining(&self) -> Option<u64> {
        Some(self.gas.remaining())
    }

    fn trigger_budget_remaining(&self) -> Option<u64> {
        self.trigger_budget.as_ref().map(|budget| budget.remaining)
    }

    fn begin_trigger_budget(&mut self, budget: u64) -> Result<()> {
        self.start_trigger_budget(budget)
    }

    fn trigger_budget_status(&self) -> Result<()> {
        self.active_trigger_status()
    }

    fn end_trigger_budget(&mut self) -> Result<()> {
        self.finish_trigger_budget()
    }

    fn observe_trigger_error(&mut self, error: &PrecompileError) {
        self.observe_trigger_failure(error);
    }

    fn begin_gas_ceiling(&mut self, ceiling: u64) -> Result<()> {
        if self.gas.remaining() < ceiling {
            return Err(PrecompileError::OutOfGas);
        }
        self.gas_child_remaining = Some(ceiling);
        Ok(())
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.charge(u64::from(WARM_STORAGE_READ_COST))?;
        let slot = self
            .evm
            .state_mut()
            .storage_slot(&address, key)
            .map_err(crate::errors::CtxStorageError::storage)?;
        Ok(slot.current())
    }

    fn enclave_upgrade_id(&mut self) -> Result<U256> {
        self.charge(0)?;
        let slot = self
            .evm
            .state_mut()
            .storage_slot(
                &outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
                U256::from(44),
            )
            .map_err(crate::errors::CtxStorageError::storage)?;
        Ok(slot.current())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.charge(0)?;
        Ok(self.evm.state_mut().tload(&address, &key))
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.charge(u64::from(SSTORE_RESET))?;
        self.evm
            .state_mut()
            .storage_slot(&address, key)
            .map_err(crate::errors::CtxStorageError::storage)?
            .set(value);
        Ok(())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.charge(0)?;
        self.evm.state_mut().tstore(&address, &key, &value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.charge(0)?;
        self.evm.log(Log {
            address,
            data: event,
        });
        Ok(())
    }

    fn deduct_gas(&mut self, cost: u64) -> Result<()> {
        self.charge(cost)
    }

    fn refund_gas(&mut self, refund: i64) {
        self.gas.record_refund(refund);
    }

    fn gas_used(&self) -> u64 {
        self.gas.limit().saturating_sub(self.gas.remaining())
    }

    fn gas_refunded(&self) -> i64 {
        self.gas.refunded()
    }

    fn is_static(&self) -> bool {
        self.is_static
    }

    fn checkpoint(&mut self) -> StateCheckpoint {
        let raw = self.evm.state().checkpoint();
        let depth = self.checkpoints.len();
        let token = StateCheckpoint::new(depth, raw.logs_len());
        self.checkpoints.push((
            token.clone(),
            raw,
            self.trigger_budget.as_ref().map(|_| self.gas.refunded()),
        ));
        token
    }

    fn checkpoint_commit(&mut self, checkpoint: StateCheckpoint) -> Result<()> {
        let expected = self
            .checkpoints
            .last()
            .map(|(token, _, _)| token.journal_len());
        if self.checkpoints.last().map(|(token, _, _)| token) != Some(&checkpoint) {
            return Err(
                outbe_primitives::error::PrimitivesStorageError::CheckpointOutOfOrder {
                    expected,
                    actual: checkpoint.journal_len(),
                }
                .into(),
            );
        }
        self.checkpoints.pop();
        Ok(())
    }

    fn checkpoint_revert(&mut self, checkpoint: StateCheckpoint) -> Result<()> {
        let Some(pos) = self
            .checkpoints
            .iter()
            .rposition(|(token, _, _)| *token == checkpoint)
        else {
            return Err(
                outbe_primitives::error::PrimitivesStorageError::InvalidCheckpointRevert {
                    checkpoint: checkpoint.journal_len(),
                    depth: self.checkpoints.len(),
                }
                .into(),
            );
        };
        let features = self.evm.version().features;
        while let Some((_, raw, refund)) = (self.checkpoints.len() > pos)
            .then(|| self.checkpoints.pop())
            .flatten()
        {
            self.evm.state_mut().rollback(raw, features);
            if let Some(refund) = refund {
                self.gas.inner_mut().set_refund(refund);
            }
        }
        Ok(())
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        self.charge(0)?;
        if amount.is_zero() {
            return Ok(());
        }
        match self.evm.state_mut().transfer(&from, &to, &amount) {
            Ok(true) => Ok(()),
            Ok(false) => Err(
                crate::errors::CtxStorageError::InsufficientBalanceForTransfer { from, amount }
                    .into(),
            ),
            Err(e) => Err(crate::errors::CtxStorageError::transfer_failed(e)),
        }
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        self.charge(0)?;
        if amount.is_zero() {
            return Ok(());
        }
        self.evm
            .state_mut()
            .account(&address)
            .map_err(crate::errors::CtxStorageError::balance_incr_failed)?
            .add_balance(amount);
        Ok(())
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        self.charge(0)?;
        if amount.is_zero() {
            return Ok(());
        }
        let mut account = self
            .evm
            .state_mut()
            .account(&address)
            .map_err(crate::errors::CtxStorageError::burn_account_load_failed)?;
        let balance = account.balance();
        let new_balance = balance.checked_sub(amount).ok_or(
            crate::errors::CtxStorageError::InsufficientBalanceForBurn {
                address,
                balance,
                amount,
            },
        )?;
        account.set_balance(new_balance);
        Ok(())
    }

    fn sub_call(
        &mut self,
        input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.execute_subcall(input)
    }
}

#[cfg(test)]
mod scoped_mutation_frame_tests {
    use super::ScopedMutationFrameState;
    use alloy_primitives::B256;
    use outbe_primitives::storage::{
        scoped_block_binding, MutationEntitlements, MutationPurposeTag as Purpose,
    };

    #[test]
    fn denied_route_cannot_open_frame() {
        let mut denied = ScopedMutationFrameState::new(MutationEntitlements::NONE);
        assert!(denied
            .begin(Purpose::CycleLifecycle, B256::repeat_byte(1), 1, 2)
            .is_err());
    }

    #[test]
    fn nesting_and_wrong_close_preserve_the_active_frame() {
        let purpose = Purpose::CycleLifecycle;
        let binding = B256::repeat_byte(3);
        let mut frame = ScopedMutationFrameState::new(MutationEntitlements::repeated(purpose, 2));
        frame.begin(purpose, binding, 7, 11).unwrap();

        assert!(frame.begin(purpose, B256::repeat_byte(4), 7, 11).is_err());
        assert!(frame.finish(purpose, B256::repeat_byte(5)).is_err());
        frame.finish(purpose, binding).unwrap();

        // A rejected nested attempt did not consume the other entitlement.
        frame.begin(purpose, B256::repeat_byte(6), 7, 11).unwrap();
    }

    #[test]
    fn one_route_entitlement_cannot_be_reused() {
        let purpose = Purpose::CycleLifecycle;
        let binding = B256::repeat_byte(7);
        let mut frame = ScopedMutationFrameState::new(MutationEntitlements::only(purpose));
        frame.begin(purpose, binding, 7, 11).unwrap();
        frame.finish(purpose, binding).unwrap();

        assert!(frame.begin(purpose, binding, 7, 11).is_err());
    }

    #[test]
    fn exact_bounded_route_budget_is_consumed_one_lease_at_a_time() {
        let purpose = Purpose::CycleLifecycle;
        let mut frame = ScopedMutationFrameState::new(MutationEntitlements::repeated(purpose, 2));
        for binding in [B256::repeat_byte(8), B256::repeat_byte(9)] {
            frame.begin(purpose, binding, 7, 11).unwrap();
            frame.finish(purpose, binding).unwrap();
        }
        assert!(frame.begin(purpose, B256::repeat_byte(10), 7, 11).is_err());
    }

    #[test]
    fn exact_authority_binding_rejects_mismatch_without_consuming_grant() {
        let purpose = Purpose::CycleLifecycle;
        let expected = scoped_block_binding(b"TEST_STEP_V1", 7, 11, 12);
        let wrong = scoped_block_binding(b"TEST_STEP_V1", 7, 11, 13);
        assert_ne!(expected, wrong);
        let mut frame =
            ScopedMutationFrameState::new(MutationEntitlements::exact(purpose, expected));

        assert!(frame.begin(purpose, wrong, 7, 11).is_err());
        frame.begin(purpose, expected, 7, 11).unwrap();
        frame.finish(purpose, expected).unwrap();
        assert!(frame.begin(purpose, expected, 7, 11).is_err());
    }
}
