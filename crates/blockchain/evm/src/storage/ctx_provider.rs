//! `CtxStorageProvider` — sub-call-driver-aware `PrecompileStorageProvider`.
//!
//! Holds `&'a mut EthEvmContext<DB>` directly so the `sub_call` body can hand
//! the full `&mut Context` to `revm_handler::EthFrame::make_call_frame` (via
//! the [`crate::sub_call`] driver) without losing access to journaled state
//! between storage ops.
//!
//! For non-sub-call storage operations (sload/sstore/balance/log/...) the
//! provider constructs an `alloy_evm::EvmInternals` from `self.ctx` on each
//! call. The construction is cheap (one `Box<dyn EvmInternalsTr>` allocation
//! per op) and is the standard cost of going through the `EvmInternals`
//! facade that revm's `PrecompileInput.internals` exposes.
//!
//! Sub-call hands `self.ctx` to the driver in [`crate::sub_call`].
//!
//! ## Coexistence with `EvmStorageProvider`
//!
//! The legacy [`outbe_primitives::storage::evm::EvmStorageProvider`] which
//! holds `EvmInternals<'a>` is still used by the read-only / non-sub-call
//! dispatch path inside [`crate::precompiles::extend_outbe_precompiles`].
//! `CtxStorageProvider` is constructed by the ctx-dispatch hook
//! whenever the dispatch needs sub-call. Both providers must agree
//! byte-for-byte on non-sub-call semantics — they share the same upstream
//! `EvmInternals` primitives.

use alloy_evm::{eth::EthEvmContext, EvmInternals};
use alloy_primitives::{Address, Log, LogData, B256, U256};
use core::fmt::Debug;
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::{PrecompileStorageProvider, SubCallError, SubCallInput, SubCallOutput},
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
use outbe_offchain_data::RuntimeBodyReaders;

thread_local! {
    /// Per-thread reentrancy stack tracking which outbe precompile addresses
    /// are currently dispatched on this call chain. revm processes one
    /// transaction synchronously per thread, so a thread-local stack is the
    /// natural scope: it lives exactly as long as the active tx execution
    /// and is reset (empty) at the end of every dispatch chain by RAII Drop
    /// on [`ReentrancyGuard`].
    static REENTRANCY_STACK: RefCell<Vec<Address>> = const { RefCell::new(Vec::new()) };
}

/// Zero-sized marker preserved as a [`CtxStorageProvider`] field so the
/// struct layout from is not disturbed. The actual stack lives
/// in the thread-local [`struct@REENTRANCY_STACK`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ReentrancyStack;

impl ReentrancyStack {
    /// Attempts to push `addr` onto the active reentrancy stack.
    ///
    /// Returns `Some(ReentrancyGuard)` on first entry; `None` if `addr` is
    /// already on the stack (caller must reject the dispatch).
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
    /// [`crate::sub_call::run`]; non-sub-call ops derive an
    /// [`alloy_evm::EvmInternals`] facade on-the-fly per call.
    pub ctx: &'a mut EthEvmContext<DB>,
    /// Per-call gas meter (mirrors `revm::Gas`).
    pub gas: SubcallGasMeter,
    /// STATICCALL flag forwarded from the outer dispatcher.
    pub is_static: bool,
    /// Address of the outbe precompile the dispatcher is currently running.
    pub self_address: Address,
    /// Reentrancy stack marker (real state lives in thread-local).
    pub reentrancy_stack: ReentrancyStack,
    /// EVM spec id, captured at provider construction time.
    pub spec: SpecId,
    /// Least-authority off-chain body readers propagated to nested precompiles.
    pub runtime_body_readers: Option<RuntimeBodyReaders>,
}

impl<'a, DB: Database + Debug> CtxStorageProvider<'a, DB> {
    /// Constructor.
    pub fn new(
        ctx: &'a mut EthEvmContext<DB>,
        gas: SubcallGasMeter,
        is_static: bool,
        self_address: Address,
        reentrancy_stack: ReentrancyStack,
        spec: SpecId,
        runtime_body_readers: Option<RuntimeBodyReaders>,
    ) -> Self {
        Self {
            ctx,
            gas,
            is_static,
            self_address,
            reentrancy_stack,
            spec,
            runtime_body_readers,
        }
    }

    /// Constructs a fresh `EvmInternals` view of `self.ctx` for one
    /// storage operation. Reborrows `self.ctx`; the returned facade is
    /// valid only within the calling method scope.
    #[inline]
    fn internals(&mut self) -> EvmInternals<'_> {
        EvmInternals::from_context(&mut *self.ctx)
    }
}

impl<'a, DB: Database + Debug> PrecompileStorageProvider for CtxStorageProvider<'a, DB> {
    fn chain_id(&self) -> u64 {
        ContextTr::cfg(self.ctx).chain_id()
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
        let hash =
            self.internals().db_mut().block_hash(number).map_err(|e| {
                PrecompileError::Storage(format!("block_hash({number}) failed: {e}"))
            })?;
        Ok((!hash.is_zero()).then_some(hash))
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        self.internals()
            .set_code(address, code)
            .map_err(|e| PrecompileError::Storage(e.to_string()))
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        let mut internals = self.internals();
        let account = internals
            .load_account_code(address)
            .map_err(|e| PrecompileError::Storage(e.to_string()))?;
        Ok(account.data.account().info.clone())
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        if !self.gas.record_regular_cost(WARM_STORAGE_READ_COST) {
            return Err(PrecompileError::OutOfGas);
        }
        let mut internals = self.internals();
        let value = internals
            .sload(address, key)
            .map_err(|e| PrecompileError::Storage(e.to_string()))?;
        Ok(value.data)
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        Ok(self.internals().tload(address, key))
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        if !self.gas.record_regular_cost(SSTORE_RESET) {
            return Err(PrecompileError::OutOfGas);
        }
        let mut internals = self.internals();
        internals
            .sstore(address, key, value)
            .map_err(|e| PrecompileError::Storage(e.to_string()))?;
        Ok(())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.internals().tstore(address, key, value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.internals().log(Log {
            address,
            data: event,
        });
        Ok(())
    }

    fn deduct_gas(&mut self, cost: u64) -> Result<()> {
        if self.gas.record_regular_cost(cost) {
            Ok(())
        } else {
            Err(PrecompileError::OutOfGas)
        }
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
        self.internals().checkpoint()
    }

    fn checkpoint_commit(&mut self) {
        self.internals().checkpoint_commit()
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        self.internals().checkpoint_revert(checkpoint)
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        match self.internals().transfer(from, to, amount) {
            Ok(None) => Ok(()),
            Ok(Some(_transfer_error)) => Err(PrecompileError::Fatal(format!(
                "insufficient balance for transfer from {from}: needs {amount}"
            ))),
            Err(e) => Err(PrecompileError::Storage(format!("transfer failed: {e}"))),
        }
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        self.internals()
            .balance_incr(address, amount)
            .map_err(|e| PrecompileError::Storage(format!("balance_incr failed: {e}")))?;
        Ok(())
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let balance = {
            let mut internals = self.internals();
            let account = internals
                .load_account(address)
                .map_err(|e| PrecompileError::Storage(format!("burn account load failed: {e}")))?;
            account.data.info.balance
        };
        let new_balance = balance.checked_sub(amount).ok_or_else(|| {
            PrecompileError::Fatal(format!(
                "insufficient balance for burn from {address}: has {balance} but needs {amount}"
            ))
        })?;
        let mut internals = self.internals();
        internals
            .set_balance(address, new_balance)
            .map_err(|e| PrecompileError::Storage(format!("burn set_balance failed: {e}")))?;
        Ok(())
    }

    fn sub_call(
        &mut self,
        input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        sub_call::run(
            self.ctx,
            self.self_address,
            self.is_static,
            self.spec,
            self.runtime_body_readers.clone(),
            input,
        )
    }
}
