//! `CtxStorageProvider` - sub-call-driver-aware `PrecompileStorageProvider`.
//!
//! The provider holds `&'a mut EthEvmContext<DB>` directly. Thus the `sub_call` body can hand
//! the full `&mut Context` to `revm_handler::EthFrame::make_call_frame` (through
//! the [`crate::sub_call`] driver) and keep access to journaled state between storage ops.
//!
//! For non-sub-call storage operations (sload/sstore/balance/log/...), the
//! provider constructs an `alloy_evm::EvmInternals` from `self.ctx` on each
//! call. The construction is cheap: one `Box<dyn EvmInternalsTr>` allocation
//! per op. It is the standard cost of the `EvmInternals` facade that revm's
//! `PrecompileInput.internals` exposes.
//!
//! Sub-call hands `self.ctx` to the driver in [`crate::sub_call`].
//!
//! ## Dispatch path
//!
//! The ctx-dispatch hook that [`crate::precompiles::extend_outbe_precompiles`]
//! installs builds a `CtxStorageProvider` for every Outbe precompile dispatch,
//! read-only or not, sub-call or not.

//! ## Storage gas schedule
//!
//! Each persistent read costs 100 gas. Each persistent write costs 5000 gas.
//! These flat costs do not depend on slot warmth or the old and new values.
//! Transient reads, transient writes, and `enclave_upgrade_id` do not charge this meter.
//! These prices define current behavior, not a resource-pricing benchmark.

use alloy_evm::{eth::EthEvmContext, EvmInternals};
use alloy_primitives::{Address, Log, LogData, B256, U256};
use core::fmt::Debug;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::{
        MetadosisMutationEntitlements, MetadosisMutationPurposeTag, PrecompileStorageProvider,
        SubCallError, SubCallInput, SubCallOutput, SubCallStatus,
    },
};
use revm::{
    context::journaled_state::JournalCheckpoint,
    context_interface::{
        cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST},
        Block as _, Cfg as _, ContextTr,
    },
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Database,
};
use std::cell::RefCell;

use crate::{gas::SubcallGasMeter, sub_call};

#[cfg(test)]
#[path = "subcall_tests.rs"]
mod subcall_tests;

thread_local! {
    /// Per-thread reentrancy stack. It records the outbe precompile addresses
    /// that are currently dispatched on this call chain. revm processes one
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
/// [`alloy_evm::eth::EthEvmContext`].
///
/// `DB: Debug` is required because [`alloy_evm::EvmInternals::from_context`]
/// has a `Journal: Debug` bound (Journal<DB> Debug-derives over DB).
pub struct CtxStorageProvider<'a, DB: Database + Debug> {
    /// Borrowed EVM context. Sub-call hands this to
    /// [`crate::sub_call::run`]. Non-sub-call ops derive an
    /// [`alloy_evm::EvmInternals`] facade on-the-fly per call.
    pub ctx: &'a mut EthEvmContext<DB>,
    /// Per-call gas meter (mirrors `revm::Gas`).
    pub gas: SubcallGasMeter,
    /// Admission OOG cannot be caught by a domain `try_call` handler.
    pub(crate) subcall_out_of_gas: bool,
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
    /// Least-authority off-chain body readers propagated to nested precompiles.
    pub(crate) abort_bridge: crate::precompiles::ExecutionAbortBridge,

    /// One-shot authority for the active Metadosis mutation frame.
    dispatch_frames: DispatchFrameAuthority,
    /// Active journal checkpoints `(token, raw_revm_checkpoint)` in LIFO order.
    checkpoints: Vec<(JournalCheckpoint, JournalCheckpoint, Option<i64>)>,
    trigger_budget: Option<outbe_primitives::storage::gas::TriggerBudget>,
    /// Remaining gas of the active resumable-unit child frame.
    gas_child_remaining: Option<u64>,
    /// Whether end-block gas metering is active for this storage dispatch.
    end_gas_metered: bool,
}

impl CtxStorageProviderConfig {
    pub(crate) const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }
}

pub(crate) struct CtxStorageProviderConfig {
    pub(crate) is_static: bool,
    pub(crate) self_address: Address,
    pub(crate) reentrancy_stack: ReentrancyStack,
    pub(crate) spec: SpecId,
    pub(crate) genesis_hash: B256,
    pub(crate) genesis_timestamp: u64,

    pub(crate) abort_bridge: crate::precompiles::ExecutionAbortBridge,

    pub(crate) metadosis_mutation_entitlements: MetadosisMutationEntitlements,
}

struct DispatchFrameAuthority {
    metadosis: MetadosisMutationFrameState,
}

#[derive(Debug)]
struct MetadosisMutationFrameState {
    entitlements: MetadosisMutationEntitlements,
    active: Option<(MetadosisMutationPurposeTag, B256, u64, u64)>,
}

impl MetadosisMutationFrameState {
    const fn new(entitlements: MetadosisMutationEntitlements) -> Self {
        Self {
            entitlements,
            active: None,
        }
    }

    fn begin(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.active.is_some() || !self.entitlements.consume(purpose, binding) {
            return Err(
                crate::errors::CtxStorageError::MetadosisMutationPurposeNotEntitled {
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
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
    ) -> outbe_primitives::error::Result<()> {
        let Some((active_purpose, active_binding, _, _)) = self.active.as_ref().copied() else {
            return Err(crate::errors::CtxStorageError::MetadosisMutationFrameNotActive.into());
        };
        if active_purpose != purpose || active_binding != binding {
            return Err(
                crate::errors::CtxStorageError::MetadosisMutationFrameIdentityMismatch.into(),
            );
        }
        self.active = None;
        Ok(())
    }
}

impl<'a, DB: Database + Debug> CtxStorageProvider<'a, DB> {
    /// Constructor.
    pub(crate) fn new(
        ctx: &'a mut EthEvmContext<DB>,
        gas: SubcallGasMeter,
        config: CtxStorageProviderConfig,
    ) -> Self {
        Self {
            ctx,
            gas,
            subcall_out_of_gas: false,
            is_static: config.is_static,
            self_address: config.self_address,
            reentrancy_stack: config.reentrancy_stack,
            spec: config.spec,
            genesis_hash: config.genesis_hash,
            genesis_timestamp: 0,

            abort_bridge: config.abort_bridge,

            dispatch_frames: DispatchFrameAuthority {
                metadosis: MetadosisMutationFrameState::new(config.metadosis_mutation_entitlements),
            },
            checkpoints: Vec::new(),
            trigger_budget: None,
            gas_child_remaining: None,
            end_gas_metered: false,
        }
        .with_genesis_timestamp(config.genesis_timestamp)
    }

    pub(crate) const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }

    /// Installs the exact route inventory after a provider-backed read
    /// resolved the consensus state needed to derive it. ProtocolCycle uses
    /// this. Its daily allocation decision depends on Cycle's persisted UTC
    /// cursor rather than on untrusted calldata.
    fn charge(&mut self, cost: u64) -> Result<()> {
        if self.trigger_budget.is_some() {
            self.trigger_budget_status()?;
            if cost > self.gas.remaining() {
                self.subcall_out_of_gas = true;
                self.gas.inner_mut().spend_all();
                return Err(PrecompileError::OutOfGas);
            }
            if let Some(budget) = &mut self.trigger_budget {
                budget.check(cost)?;
            }
        }
        if let Some(child) = self.gas_child_remaining {
            if cost > child {
                let _ = self.gas.record_regular_cost(child);
                if let Some(budget) = &mut self.trigger_budget {
                    budget.charge(child)?;
                }
                self.gas_child_remaining = Some(0);
                return Err(PrecompileError::OutOfGas);
            }
            self.gas_child_remaining = Some(child - cost);
        }
        if self.gas.record_regular_cost(cost) {
            if let Some(budget) = &mut self.trigger_budget {
                budget.charge(cost)?;
            }
            Ok(())
        } else {
            Err(PrecompileError::OutOfGas)
        }
    }

    pub(crate) fn replace_metadosis_mutation_entitlements(
        &mut self,
        entitlements: MetadosisMutationEntitlements,
    ) {
        self.dispatch_frames.metadosis = MetadosisMutationFrameState::new(entitlements);
    }

    fn charge_subcall(&mut self, cost: u64) -> std::result::Result<(), SubCallError> {
        if self.trigger_budget.is_some() {
            return self.charge(cost).map_err(|error| match error {
                PrecompileError::TriggerBudgetExceeded {
                    consumed_gas,
                    budget_gas,
                } => SubCallError::TriggerBudgetExceeded {
                    consumed_gas,
                    budget_gas,
                },
                PrecompileError::OutOfGas => SubCallError::ParentOutOfGas,
                other => SubCallError::Fatal(other.to_string()),
            });
        }
        if self.gas.record_regular_cost(cost) {
            Ok(())
        } else {
            self.subcall_out_of_gas = true;
            self.gas.inner_mut().spend_all();
            Err(SubCallError::ParentOutOfGas)
        }
    }
}

/// Reborrow the EVM context for one storage operation.
#[inline]
fn internals<DB: Database + Debug>(ctx: &mut EthEvmContext<DB>) -> EvmInternals<'_> {
    EvmInternals::from_context(ctx)
}

impl<'a, DB: Database + Debug> PrecompileStorageProvider for CtxStorageProvider<'a, DB> {
    fn begin_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.trigger_budget.is_some() {
            self.trigger_budget_status()?;
        }
        if ContextTr::cfg(self.ctx).chain_id() != chain_id
            || ContextTr::block(self.ctx).number().saturating_to::<u64>() != block_number
        {
            return Err(crate::errors::CtxStorageError::MetadosisMutationFrameScopeMismatch.into());
        }
        self.dispatch_frames
            .metadosis
            .begin(purpose, binding, chain_id, block_number)
    }

    fn require_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> outbe_primitives::error::Result<()> {
        if self.dispatch_frames.metadosis.active != Some((purpose, binding, chain_id, block_number))
        {
            return Err(
                crate::errors::CtxStorageError::MetadosisMutationFrameIdentityMismatch.into(),
            );
        }
        Ok(())
    }

    fn finish_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        _completed: bool,
    ) -> outbe_primitives::error::Result<()> {
        self.dispatch_frames.metadosis.finish(purpose, binding)
    }

    fn chain_id(&self) -> u64 {
        ContextTr::cfg(self.ctx).chain_id()
    }

    fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }

    fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    fn timestamp(&self) -> U256 {
        ContextTr::block(self.ctx).timestamp()
    }

    fn beneficiary(&self) -> Address {
        ContextTr::block(self.ctx).beneficiary()
    }

    fn block_number(&self) -> u64 {
        ContextTr::block(self.ctx).number().saturating_to::<u64>()
    }

    fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
        self.charge(0)?;
        let hash = internals(self.ctx)
            .db_mut()
            .block_hash(number)
            .map_err(|e| crate::errors::CtxStorageError::block_hash_failed(number, e))?;
        Ok((!hash.is_zero()).then_some(hash))
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        self.charge(0)?;
        internals(self.ctx)
            .set_code(address, code)
            .map_err(crate::errors::CtxStorageError::storage)
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        self.charge(0)?;
        let mut internals = internals(self.ctx);
        let account = internals
            .load_account_code(address)
            .map_err(crate::errors::CtxStorageError::storage)?;
        Ok(account.data.account().info.clone())
    }

    fn gas_remaining(&self) -> Option<u64> {
        self.end_gas_metered.then(|| self.gas.remaining())
    }

    fn metered_gas_remaining(&self) -> Option<u64> {
        Some(self.gas.remaining())
    }

    fn begin_trigger_budget(&mut self, budget: u64) -> Result<()> {
        use outbe_primitives::storage::gas::{TriggerBudget, TriggerScopeError};
        if self.trigger_budget.is_some() {
            return Err(TriggerScopeError::Nested.into());
        }
        if self.subcall_out_of_gas {
            return Err(PrecompileError::OutOfGas);
        }
        self.trigger_budget = Some(TriggerBudget::new(budget));
        Ok(())
    }

    fn trigger_budget_status(&self) -> Result<()> {
        if self.subcall_out_of_gas {
            return Err(PrecompileError::OutOfGas);
        }
        self.trigger_budget
            .as_ref()
            .ok_or(outbe_primitives::storage::gas::TriggerScopeError::Inactive)?
            .status()
    }

    fn end_trigger_budget(&mut self) -> Result<()> {
        self.trigger_budget
            .take()
            .ok_or(outbe_primitives::storage::gas::TriggerScopeError::Inactive)?;
        Ok(())
    }

    fn observe_trigger_error(&mut self, error: &PrecompileError) {
        if let Some(budget) = &mut self.trigger_budget {
            budget.observe_error(error);
        }
    }

    fn enable_end_gas_metering(&mut self) -> Result<()> {
        self.end_gas_metered = true;
        Ok(())
    }

    fn begin_gas_ceiling(&mut self, ceiling: u64) -> Result<()> {
        if self.gas.remaining() < ceiling {
            return Err(PrecompileError::OutOfGas);
        }
        self.gas_child_remaining = Some(ceiling);
        Ok(())
    }

    fn end_gas_ceiling(&mut self) -> Result<()> {
        self.gas_child_remaining = None;
        Ok(())
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.charge(WARM_STORAGE_READ_COST)?;
        let mut internals = internals(self.ctx);
        let value = internals
            .sload(address, key)
            .map_err(crate::errors::CtxStorageError::storage)?;
        Ok(value.data)
    }
    fn enclave_upgrade_id(&mut self) -> Result<U256> {
        self.charge(0)?;
        internals(self.ctx)
            .sload(
                outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
                U256::from(44),
            )
            .map(|value| value.data)
            .map_err(crate::errors::CtxStorageError::storage)
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.charge(0)?;
        Ok(internals(self.ctx).tload(address, key))
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.charge(SSTORE_RESET)?;
        let mut internals = internals(self.ctx);
        internals
            .sstore(address, key, value)
            .map_err(crate::errors::CtxStorageError::storage)?;
        Ok(())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.charge(0)?;
        internals(self.ctx).tstore(address, key, value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.charge(0)?;
        internals(self.ctx).log(Log {
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

    fn checkpoint(&mut self) -> JournalCheckpoint {
        let raw = internals(self.ctx).checkpoint();
        let depth = self.checkpoints.len();
        let token = JournalCheckpoint {
            selfdestructed_i: depth,
            ..raw
        };
        self.checkpoints.push((
            token,
            raw,
            self.trigger_budget.as_ref().map(|_| self.gas.refunded()),
        ));
        token
    }

    fn checkpoint_commit(&mut self, checkpoint: JournalCheckpoint) -> Result<()> {
        let expected = self
            .checkpoints
            .last()
            .map(|(token, _, _)| token.selfdestructed_i);
        if self.checkpoints.last().map(|(token, _, _)| *token) != Some(checkpoint) {
            return Err(
                outbe_primitives::error::PrimitivesStorageError::CheckpointOutOfOrder {
                    expected,
                    actual: checkpoint.selfdestructed_i,
                }
                .into(),
            );
        }
        self.checkpoints.pop();
        internals(self.ctx).checkpoint_commit();
        Ok(())
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) -> Result<()> {
        let Some(pos) = self
            .checkpoints
            .iter()
            .rposition(|(token, _, _)| *token == checkpoint)
        else {
            return Err(
                outbe_primitives::error::PrimitivesStorageError::InvalidCheckpointRevert {
                    checkpoint: checkpoint.selfdestructed_i,
                    depth: self.checkpoints.len(),
                }
                .into(),
            );
        };
        while let Some((_, raw, refund)) = (self.checkpoints.len() > pos)
            .then(|| self.checkpoints.pop())
            .flatten()
        {
            internals(self.ctx).checkpoint_revert(raw);
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
        match internals(self.ctx).transfer(from, to, amount) {
            Ok(None) => Ok(()),
            Ok(Some(_transfer_error)) => Err(
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
        internals(self.ctx)
            .balance_incr(address, amount)
            .map_err(crate::errors::CtxStorageError::balance_incr_failed)?;
        Ok(())
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        self.charge(0)?;
        if amount.is_zero() {
            return Ok(());
        }
        let balance = {
            let mut internals = internals(self.ctx);
            let account = internals
                .load_account(address)
                .map_err(crate::errors::CtxStorageError::burn_account_load_failed)?;
            account.data.info.balance
        };
        let new_balance = balance.checked_sub(amount).ok_or(
            crate::errors::CtxStorageError::InsufficientBalanceForBurn {
                address,
                balance,
                amount,
            },
        )?;
        let mut internals = internals(self.ctx);
        internals
            .set_balance(address, new_balance)
            .map_err(crate::errors::CtxStorageError::burn_set_balance_failed)?;
        Ok(())
    }

    fn sub_call(
        &mut self,
        mut input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        use revm::{
            context_interface::host::LoadError,
            interpreter::instructions::{contract::load_account_delegated, gas_table_spec},
        };
        self.abort_bridge.check_subcall()?;
        if self.trigger_budget.is_some() {
            self.charge_subcall(0)?;
        }
        // The Outbe precompile boundary currently has a single gas pool. Fail
        // closed rather than discarding an EIP-8037 reservoir/state-gas charge.
        if self.ctx.cfg().is_amsterdam_eip8037_enabled() {
            return Err(SubCallError::Fatal(
                "Outbe subcalls do not support EIP-8037".into(),
            ));
        }
        let is_static = self.is_static || input.is_static;
        if is_static && !input.value.is_zero() {
            return Ok(SubCallOutput {
                status: SubCallStatus::Halt(SubCallError::StateChangeDuringStaticCall),
                returndata: Default::default(),
                gas_used: 0,
                gas_refunded: 0,
            });
        }
        // Price the same account/delegation/value access as the pinned CALL
        // implementation, before computing EIP-150's cap. This explicit-budget
        // API intentionally does not add CALL's automatic value stipend.
        let opcode = if is_static { 0xfa } else { 0xf1 };
        self.charge_subcall(u64::from(gas_table_spec(self.spec)[opcode]))?;
        let transfers_value = !input.value.is_zero();
        if transfers_value {
            self.charge_subcall(self.ctx.cfg().gas_params().transfer_value_cost())?;
        }
        let (cost, state_cost, code, hash) = match load_account_delegated(
            self.ctx,
            self.spec,
            self.gas.remaining(),
            input.target,
            transfers_value,
            !is_static,
        ) {
            Ok(loaded) => loaded,
            Err(LoadError::ColdLoadSkipped) => {
                self.charge_subcall(u64::MAX)?;
                return Err(SubCallError::ParentOutOfGas);
            }
            Err(LoadError::DBError) => {
                return Err(crate::errors::SubCallDriverError::database_error(
                    self.ctx.error(),
                ));
            }
        };
        if state_cost != 0 {
            return Err(crate::errors::SubCallDriverError::UnsupportedStateGasCharge.into());
        }
        self.charge_subcall(cost)?;
        let remaining = self.gas.remaining();
        let cap = if self.spec.is_enabled_in(SpecId::TANGERINE) {
            self.ctx
                .cfg()
                .gas_params()
                .call_stipend_reduction(remaining)
        } else {
            remaining
        };
        input.gas_limit = input.gas_limit.min(cap);
        if self.trigger_budget.is_some() {
            if let Some(legacy_remaining) = self.gas_child_remaining {
                input.gas_limit = input.gas_limit.min(legacy_remaining);
            }
        }
        let locally_limited = self
            .trigger_budget
            .as_ref()
            .is_some_and(|budget| budget.remaining < input.gas_limit);
        if let Some(budget) = &self.trigger_budget {
            input.gas_limit = input.gas_limit.min(budget.remaining);
        }
        let allowance = input.gas_limit;
        self.charge_subcall(allowance)?;
        let outcome = sub_call::run_frame(
            self.ctx,
            sub_call::SubCallContext {
                self_address: self.self_address,
                outer_is_static: self.is_static,
                execution: crate::precompiles::OutbePrecompileExecutionContext::new(
                    self.spec,
                    self.genesis_hash,
                )
                .with_genesis_timestamp(self.genesis_timestamp),
                runtime: crate::precompiles::OutbePrecompileRuntime::new()
                    .with_abort_bridge(self.abort_bridge.clone()),
            },
            input,
            Some((hash, code)),
        );
        match outcome {
            Ok(outcome) => {
                // Settlement collapses fixed memory/operand failures into public OOG.
                // More allowance cannot fix those failures, so they are not local exhaustion.
                let allowance_exhausted = matches!(
                    outcome.result.result,
                    revm::interpreter::InstructionResult::OutOfGas
                        | revm::interpreter::InstructionResult::MemoryOOG
                        | revm::interpreter::InstructionResult::PrecompileOOG
                        | revm::interpreter::InstructionResult::ReentrancySentryOOG
                );
                let reserved_remaining = self.gas.remaining();
                let output = sub_call::settle_outcome(outcome, self.gas.inner_mut())?;
                if let Some(budget) = &mut self.trigger_budget {
                    // Use canonical settlement's actual return, not output.gas_used.
                    budget
                        .return_unused(self.gas.remaining() - reserved_remaining)
                        .map_err(|error| SubCallError::Fatal(error.to_string()))?;
                    if let Some(child) = &mut self.gas_child_remaining {
                        *child = child
                            .checked_add(self.gas.remaining() - reserved_remaining)
                            .ok_or_else(|| {
                                SubCallError::Fatal("legacy gas return overflow".into())
                            })?;
                    }
                    if locally_limited && allowance_exhausted {
                        budget.exhaust();
                        if let Err(PrecompileError::TriggerBudgetExceeded {
                            consumed_gas,
                            budget_gas,
                        }) = budget.status()
                        {
                            return Err(SubCallError::TriggerBudgetExceeded {
                                consumed_gas,
                                budget_gas,
                            });
                        }
                    }
                }
                Ok(output)
            }
            Err(error @ SubCallError::DepthLimitExceeded) => {
                self.gas.erase_cost(allowance);
                if let Some(budget) = &mut self.trigger_budget {
                    budget
                        .return_unused(allowance)
                        .map_err(|error| SubCallError::Fatal(error.to_string()))?;
                }
                if self.trigger_budget.is_some() {
                    if let Some(child) = &mut self.gas_child_remaining {
                        *child = child.checked_add(allowance).ok_or_else(|| {
                            SubCallError::Fatal("legacy gas return overflow".into())
                        })?;
                    }
                }
                Ok(SubCallOutput {
                    status: SubCallStatus::Halt(error),
                    returndata: Default::default(),
                    gas_used: 0,
                    gas_refunded: 0,
                })
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod metadosis_mutation_frame_tests {
    use super::MetadosisMutationFrameState;
    use alloy_primitives::B256;
    use outbe_primitives::storage::{
        metadosis_late_settlement_binding, MetadosisMutationEntitlements,
        MetadosisMutationPurposeTag as Purpose,
    };

    #[test]
    fn denied_or_wrong_purpose_cannot_open_frame() {
        let mut denied = MetadosisMutationFrameState::new(MetadosisMutationEntitlements::NONE);
        assert!(denied
            .begin(Purpose::CycleLifecycle, B256::repeat_byte(1), 1, 2)
            .is_err());

        let mut wrong = MetadosisMutationFrameState::new(MetadosisMutationEntitlements::only(
            Purpose::CertifiedFinality,
        ));
        assert!(wrong
            .begin(Purpose::CycleLifecycle, B256::repeat_byte(1), 1, 2)
            .is_err());
        wrong
            .begin(Purpose::CertifiedFinality, B256::repeat_byte(2), 1, 2)
            .unwrap();
    }

    #[test]
    fn nesting_and_wrong_close_preserve_the_active_frame() {
        let purpose = Purpose::CycleLifecycle;
        let binding = B256::repeat_byte(3);
        let mut frame =
            MetadosisMutationFrameState::new(MetadosisMutationEntitlements::only(purpose).union(
                MetadosisMutationEntitlements::only(Purpose::CertifiedFinality),
            ));
        frame.begin(purpose, binding, 7, 11).unwrap();

        assert!(frame
            .begin(Purpose::CertifiedFinality, B256::repeat_byte(4), 7, 11,)
            .is_err());
        assert!(frame.finish(purpose, B256::repeat_byte(5)).is_err());
        frame.finish(purpose, binding).unwrap();

        // A rejected nested attempt did not consume the other entitlement.
        frame
            .begin(Purpose::CertifiedFinality, B256::repeat_byte(6), 7, 11)
            .unwrap();
    }

    #[test]
    fn one_route_entitlement_cannot_be_reused() {
        let purpose = Purpose::CycleLifecycle;
        let binding = B256::repeat_byte(7);
        let mut frame =
            MetadosisMutationFrameState::new(MetadosisMutationEntitlements::only(purpose));
        frame.begin(purpose, binding, 7, 11).unwrap();
        frame.finish(purpose, binding).unwrap();

        assert!(frame.begin(purpose, binding, 7, 11).is_err());
    }

    #[test]
    fn exact_bounded_route_budget_is_consumed_one_lease_at_a_time() {
        let purpose = Purpose::CycleLifecycle;
        let mut frame =
            MetadosisMutationFrameState::new(MetadosisMutationEntitlements::repeated(purpose, 2));
        for binding in [B256::repeat_byte(8), B256::repeat_byte(9)] {
            frame.begin(purpose, binding, 7, 11).unwrap();
            frame.finish(purpose, binding).unwrap();
        }
        assert!(frame.begin(purpose, B256::repeat_byte(10), 7, 11).is_err());
    }

    #[test]
    fn exact_authority_binding_rejects_mismatch_without_consuming_grant() {
        let purpose = Purpose::CertifiedFinality;
        let expected = metadosis_late_settlement_binding(7, 11, 12);
        let wrong = metadosis_late_settlement_binding(7, 11, 13);
        assert_ne!(expected, wrong);
        let mut frame = MetadosisMutationFrameState::new(MetadosisMutationEntitlements::exact(
            purpose, expected,
        ));

        assert!(frame.begin(purpose, wrong, 7, 11).is_err());
        frame.begin(purpose, expected, 7, 11).unwrap();
        frame.finish(purpose, expected).unwrap();
        assert!(frame.begin(purpose, expected, 7, 11).is_err());
    }
}
