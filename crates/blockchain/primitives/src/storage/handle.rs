use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use revm::{
    context::journaled_state::JournalCheckpoint,
    state::{AccountInfo, Bytecode},
};
use std::{cell::RefCell, rc::Rc};

use crate::{
    error::{PrecompileError, Result},
    storage::{
        CheckpointGuard, PrecompileStorageProvider, StorageBacked, SubCallError, SubCallInput,
        SubCallOutput, SubCallStatus,
    },
};

/// Scoped handle to the current execution storage provider.
///
/// The handle is tied to the lifetime of one precompile, transaction, or block
/// lifecycle execution scope. It is intentionally single-threaded through
/// `Rc<RefCell<_>>`. Do not store contract/storage facades beyond that scope.
///
/// `StorageHandle` is invariant in `'storage` because it wraps `&mut` provider
/// access. It cannot be reborrowed into a shorter independent execution scope.
#[derive(Clone)]
pub struct StorageHandle<'storage> {
    pub(crate) inner: Rc<RefCell<&'storage mut dyn PrecompileStorageProvider>>,
}

impl<'storage> StorageHandle<'storage> {
    pub fn new(provider: &'storage mut dyn PrecompileStorageProvider) -> Self {
        Self {
            inner: Rc::new(RefCell::new(provider)),
        }
    }

    pub fn enter<T, R>(provider: &'storage mut T, f: impl FnOnce(StorageHandle<'storage>) -> R) -> R
    where
        T: PrecompileStorageProvider + 'storage,
    {
        let storage = Self::new(provider);
        f(storage)
    }

    pub fn contract<C: StorageBacked<'storage>>(&self) -> C {
        C::new(self.clone())
    }

    pub fn contract_at<C: StorageBacked<'storage>>(&self, address: Address) -> C {
        C::at(self.clone(), address)
    }

    pub(super) fn with_provider<R>(
        &self,
        f: impl FnOnce(&mut dyn PrecompileStorageProvider) -> Result<R>,
    ) -> Result<R> {
        let mut provider = self
            .inner
            .try_borrow_mut()
            .map_err(|_| crate::error::PrimitivesStorageError::ProviderAlreadyMutablyBorrowed)?;
        let result = f(&mut **provider);
        if let Err(error) = &result {
            provider.observe_trigger_error(error);
        }
        result
    }

    pub fn require_metadosis_process_ready(&self) -> Result<()> {
        self.require_cycle_lifecycle(super::metadosis_process_ready_binding)
    }

    pub fn require_metadosis_init_genesis(&self) -> Result<()> {
        self.require_cycle_lifecycle(super::metadosis_init_genesis_binding)
    }

    /// Allocation authority is bound to the target UTC day, not execution time.
    pub fn require_metadosis_cycle_allocation(&self, timestamp: u64) -> Result<()> {
        self.require_cycle_lifecycle_at(super::metadosis_cycle_allocation_binding, timestamp)
    }

    pub fn require_metadosis_advance_due(&self) -> Result<()> {
        self.require_cycle_lifecycle(super::metadosis_advance_due_binding)
    }

    fn require_cycle_lifecycle(&self, binding_for: fn(u64, u64, u64) -> B256) -> Result<()> {
        self.require_cycle_lifecycle_at(binding_for, self.now_secs()?)
    }

    fn require_cycle_lifecycle_at(
        &self,
        binding_for: fn(u64, u64, u64) -> B256,
        timestamp: u64,
    ) -> Result<()> {
        let chain_id = self.chain_id()?;
        let block_number = self.block_number()?;
        let binding = binding_for(chain_id, block_number, timestamp);
        self.with_provider(|provider| {
            provider.require_metadosis_mutation_frame(
                super::MetadosisMutationPurposeTag::CycleLifecycle,
                binding,
                chain_id,
                block_number,
            )
        })
    }

    pub fn chain_id(&self) -> Result<u64> {
        self.with_provider(|provider| Ok(provider.chain_id()))
    }

    pub fn genesis_hash(&self) -> Result<B256> {
        self.with_provider(|provider| Ok(provider.genesis_hash()))
    }

    pub fn genesis_timestamp(&self) -> Result<u64> {
        self.with_provider(|provider| Ok(provider.genesis_timestamp()))
    }

    pub fn timestamp(&self) -> Result<U256> {
        self.with_provider(|provider| Ok(provider.timestamp()))
    }

    /// Read execution time in Unix seconds, rejecting values above `u64::MAX`.
    pub fn now_secs(&self) -> Result<u64> {
        Ok(crate::time::timestamp_secs(self.timestamp()?)?)
    }

    /// Test fixtures only: advance the provider's reported block timestamp.
    /// Production providers (EVM) ignore this. See
    /// [`PrecompileStorageProvider::set_block_timestamp`].
    pub fn set_block_timestamp(&self, timestamp: U256) -> Result<()> {
        self.with_provider(|provider| {
            provider.set_block_timestamp(timestamp);
            Ok(())
        })
    }

    pub fn beneficiary(&self) -> Result<Address> {
        self.with_provider(|provider| Ok(provider.beneficiary()))
    }

    pub fn block_number(&self) -> Result<u64> {
        self.with_provider(|provider| Ok(provider.block_number()))
    }

    /// Returns the canonical block hash for `number`, or `None` if `number`
    /// is outside the chain's canonical-history window.
    ///
    /// Backed by
    /// [`PrecompileStorageProvider::canonical_block_hash`].
    pub fn canonical_block_hash(&self, number: u64) -> Result<Option<B256>> {
        self.with_provider(|provider| provider.canonical_block_hash(number))
    }

    pub fn with_account_info<T>(
        &self,
        address: Address,
        mut f: impl FnMut(&AccountInfo) -> Result<T>,
    ) -> Result<T> {
        let info = self.with_provider(|provider| provider.account_info(address))?;
        f(&info)
    }

    pub fn balance(&self, address: Address) -> Result<U256> {
        self.with_account_info(address, |info| Ok(info.balance))
    }

    pub fn set_balance(&self, address: Address, balance: U256) -> Result<()> {
        // Refuse balance mutation in STATICCALL
        // context up front, so the underlying `balance(..)` read does
        // not run a wasted journal lookup before the inner gate fires.
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            Ok(())
        })?;
        let current = self.balance(address)?;
        match balance.cmp(&current) {
            std::cmp::Ordering::Greater => self.increase_balance(address, balance - current),
            std::cmp::Ordering::Less => self.decrease_balance(address, current - balance),
            std::cmp::Ordering::Equal => Ok(()),
        }
    }

    pub fn sload(&self, address: Address, key: U256) -> Result<U256> {
        self.with_provider(|provider| provider.sload(address, key))
    }
    pub fn enclave_upgrade_id(&self) -> Result<U256> {
        self.with_provider(|provider| provider.enclave_upgrade_id())
    }

    pub fn tload(&self, address: Address, key: U256) -> Result<U256> {
        self.with_provider(|provider| provider.tload(address, key))
    }

    pub fn sstore(&self, address: Address, key: U256, value: U256) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.sstore(address, key, value)
        })
    }

    pub fn tstore(&self, address: Address, key: U256, value: U256) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.tstore(address, key, value)
        })
    }

    pub fn emit_event(&self, address: Address, event: LogData) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.emit_event(address, event)
        })
    }

    pub fn transfer_balance(&self, from: Address, to: Address, amount: U256) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.transfer_balance(from, to, amount)
        })
    }

    pub fn increase_balance(&self, address: Address, amount: U256) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.increase_balance(address, amount)
        })
    }

    pub fn decrease_balance(&self, address: Address, amount: U256) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.decrease_balance(address, amount)
        })
    }

    /// Deploys bytecode at `address`.
    ///
    /// Honors the STATICCALL static gate: returns
    /// [`PrecompileError::WriteProtection`] if the provider is in a
    /// static context. A provider that does not support code deployment
    /// returns an error (the read-only provider returns `Fatal`).
    pub fn set_code(&self, address: Address, code: Bytecode) -> Result<()> {
        self.with_provider(|provider| {
            if provider.is_static() {
                return Err(PrecompileError::WriteProtection);
            }
            provider.set_code(address, code)
        })
    }

    pub fn checkpoint(&self) -> Result<JournalCheckpoint> {
        self.with_provider(|provider| Ok(provider.checkpoint()))
    }

    pub fn checkpoint_guard(&self) -> Result<CheckpointGuard<'storage>> {
        Ok(CheckpointGuard::new(self.clone(), self.checkpoint()?))
    }

    /// Runs storage mutations under a journal checkpoint.
    ///
    /// This function commits the checkpoint only when the closure returns `Ok`.
    /// On `Err` or early return, [`CheckpointGuard`] drops without commit and
    /// reverts all writes made in this scope.
    pub fn with_checkpoint<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        let checkpoint = self.checkpoint_guard()?;
        let result = f()?;
        checkpoint.commit()?;
        Ok(result)
    }

    pub fn checkpoint_commit(&self, checkpoint: JournalCheckpoint) -> Result<()> {
        self.with_provider(|provider| provider.checkpoint_commit(checkpoint))
    }

    pub fn checkpoint_revert(&self, checkpoint: JournalCheckpoint) -> Result<()> {
        self.with_provider(|provider| provider.checkpoint_revert(checkpoint))
    }

    pub fn deduct_gas(&self, gas: u64) -> Result<()> {
        self.with_provider(|provider| provider.deduct_gas(gas))
    }

    pub fn enable_end_gas_metering(&self) -> Result<()> {
        self.with_provider(|provider| provider.enable_end_gas_metering())
    }

    pub fn gas_remaining(&self) -> Result<Option<u64>> {
        self.with_provider(|provider| Ok(provider.gas_remaining()))
    }

    pub fn metered_gas_remaining(&self) -> Result<Option<u64>> {
        self.with_provider(|provider| Ok(provider.metered_gas_remaining()))
    }

    /// Runs one atomic handler under a gross work allowance. Spent gas is never reverted.
    pub fn with_trigger_budget<R>(
        &self,
        budget_gas: u64,
        f: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        self.with_provider(|provider| provider.begin_trigger_budget(budget_gas))?;
        // Declaration order guarantees checkpoint rollback before scope cleanup on unwind.
        struct ScopeCleanup<'a, 'storage> {
            storage: &'a StorageHandle<'storage>,
            active: bool,
        }
        impl Drop for ScopeCleanup<'_, '_> {
            fn drop(&mut self) {
                if self.active {
                    if let Err(error) = self
                        .storage
                        .with_provider(|provider| provider.end_trigger_budget())
                    {
                        tracing::error!(%error, "failed to clear trigger scope during unwind");
                    }
                }
            }
        }
        let mut scope = ScopeCleanup {
            storage: self,
            active: true,
        };
        let token = match self.checkpoint() {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                let cleanup = self.with_provider(|provider| provider.end_trigger_budget());
                scope.active = false;
                cleanup.map_err(|error| PrecompileError::FatalWithCause(Box::new(error)))?;
                return Err(PrecompileError::FatalWithCause(Box::new(error)));
            }
        };
        let checkpoint = CheckpointGuard::new(self.clone(), token);
        let result = f();
        let status = self.with_provider(|provider| provider.trigger_budget_status());
        // Infrastructure failures and genuine enclosing OOG outrank local exhaustion.
        let result = match result {
            Err(
                error @ (PrecompileError::Storage(_)
                | PrecompileError::Fatal(_)
                | PrecompileError::FatalWithCause(_)
                | PrecompileError::SubCall(_)
                | PrecompileError::BodyReadCorruption(_)
                | PrecompileError::Unsupported
                | PrecompileError::TriggerScope(_)
                | PrecompileError::EnclaveUnavailable(_)),
            ) => Err(error),
            result @ (Ok(_)
            | Err(
                PrecompileError::OutOfGas
                | PrecompileError::Halt(_)
                | PrecompileError::WriteProtection
                | PrecompileError::Revert(_)
                | PrecompileError::RevertBytes(_)
                | PrecompileError::TriggerBudgetExceeded { .. },
            )) => status.and(result),
        };
        let checkpoint_result = if result.is_ok() {
            match checkpoint.commit() {
                Ok(()) => Ok(()),
                Err(error) => {
                    // A broken checkpoint stack must not leave handler writes committed.
                    let rollback = self.checkpoint_revert(token);
                    rollback.and(Err(error))
                }
            }
        } else {
            checkpoint.revert()
        };
        let cleanup = self.with_provider(|provider| provider.end_trigger_budget());
        scope.active = false;
        checkpoint_result.map_err(|error| PrecompileError::FatalWithCause(Box::new(error)))?;
        cleanup.map_err(|error| PrecompileError::FatalWithCause(Box::new(error)))?;
        result
    }

    /// Runs `f` with storage charges capped at `ceiling`. The caller checkpoints
    /// state. A child out-of-gas rolls that checkpoint back and does not fail the parent.
    pub fn with_gas_ceiling<R>(&self, ceiling: u64, f: impl FnOnce() -> Result<R>) -> Result<R> {
        self.with_provider(|provider| provider.begin_gas_ceiling(ceiling))?;
        let result = f();
        self.with_provider(|provider| provider.end_gas_ceiling())?;
        result
    }

    /// Runs one resumable end-zone unit inside a checkpoint, applying `kind`'s gas ceiling
    /// when end-zone gas metering is active on this storage provider.
    pub fn run_gas_bounded_unit<R>(
        &self,
        kind: crate::end_gas::UnitKind,
        f: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let ceiling = crate::end_gas::unit_ceiling(kind);
        if let Some(remaining) = self.gas_remaining()? {
            if remaining < ceiling {
                return Err(PrecompileError::OutOfGas);
            }
            self.with_gas_ceiling(ceiling, || self.with_checkpoint(f))
        } else {
            self.with_checkpoint(f)
        }
    }

    pub fn refund_gas(&self, gas: i64) -> Result<()> {
        self.with_provider(|provider| {
            provider.refund_gas(gas);
            Ok(())
        })
    }

    pub fn gas_used(&self) -> Result<u64> {
        self.with_provider(|provider| Ok(provider.gas_used()))
    }

    pub fn gas_refunded(&self) -> Result<i64> {
        self.with_provider(|provider| Ok(provider.gas_refunded()))
    }

    pub fn is_static(&self) -> Result<bool> {
        self.with_provider(|provider| Ok(provider.is_static()))
    }

    // === Sub-call API ===
    //
    // These methods forward to the provider's `sub_call`. A provider wired to
    // the sub-call driver (`outbe_evm::storage::CtxStorageProvider`) runs a
    // real child frame. The test provider (`HashMapStorageProvider`) can return
    // stub outputs. Other providers keep the default and return
    // `SubCallError::NotAvailable`. The signatures here are the public contract.

    /// Invokes a child CALL frame and returns the raw returndata.
    ///
    /// Maps `SubCallStatus::Success -> Ok(bytes)`,
    /// `Revert(bytes) -> Err(RevertBytes(bytes))`,
    /// `Halt(err) -> Err(OutOfGas/WriteProtection/Halt)` for VM failures; provider errors stay fatal.
    pub fn call(
        &self,
        target: Address,
        value: U256,
        calldata: Bytes,
    ) -> std::result::Result<Bytes, PrecompileError> {
        self.call_with_gas(target, value, calldata, u64::MAX)
    }

    /// Invokes a child STATICCALL frame.
    pub fn staticcall(
        &self,
        target: Address,
        calldata: Bytes,
    ) -> std::result::Result<Bytes, PrecompileError> {
        self.staticcall_with_gas(target, calldata, u64::MAX)
    }

    /// Invokes a child CALL frame with an explicit gas cap.
    pub fn call_with_gas(
        &self,
        target: Address,
        value: U256,
        calldata: Bytes,
        gas_limit: u64,
    ) -> std::result::Result<Bytes, PrecompileError> {
        let output = self.try_call_with_gas(target, value, calldata, gas_limit)?;
        subcall_status_to_bytes(output)
    }

    /// Invokes a child STATICCALL frame with an explicit gas cap.
    pub fn staticcall_with_gas(
        &self,
        target: Address,
        calldata: Bytes,
        gas_limit: u64,
    ) -> std::result::Result<Bytes, PrecompileError> {
        let output = self.try_staticcall_with_gas(target, calldata, gas_limit)?;
        subcall_status_to_bytes(output)
    }

    /// Lower-level variant that returns the full [`SubCallOutput`] (gas
    /// accounting, status, returndata).
    pub fn try_call(
        &self,
        target: Address,
        value: U256,
        calldata: Bytes,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.try_call_with_gas(target, value, calldata, u64::MAX)
    }

    /// Lower-level STATICCALL variant.
    pub fn try_staticcall(
        &self,
        target: Address,
        calldata: Bytes,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.try_staticcall_with_gas(target, calldata, u64::MAX)
    }

    /// Lower-level CALL with an explicit gas cap.
    pub fn try_call_with_gas(
        &self,
        target: Address,
        value: U256,
        calldata: Bytes,
        gas_limit: u64,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.do_sub_call(SubCallInput {
            target,
            value,
            calldata,
            gas_limit,
            is_static: false,
        })
    }

    /// Lower-level STATICCALL with an explicit gas cap.
    pub fn try_staticcall_with_gas(
        &self,
        target: Address,
        calldata: Bytes,
        gas_limit: u64,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.do_sub_call(SubCallInput {
            target,
            value: U256::ZERO,
            calldata,
            gas_limit,
            is_static: true,
        })
    }

    /// Internal helper: invokes the provider's [`PrecompileStorageProvider::sub_call`]
    /// via the `Rc<RefCell<...>>` interior-mut handle.
    ///
    /// Re-entry into `do_sub_call` during the dispatch closure (i.e. the
    /// callback already holds the inner borrow) returns
    /// `SubCallError::ProviderBorrowed` rather than panicking. In practice,
    /// the production sub-call provider (`outbe_evm::storage::CtxStorageProvider`)
    /// releases the borrow only after `sub_call` returns. As a result, a hostile
    /// re-entry from inside the child frame is observable as a structured error.
    fn do_sub_call(&self, input: SubCallInput) -> std::result::Result<SubCallOutput, SubCallError> {
        let mut guard = self
            .inner
            .try_borrow_mut()
            .map_err(|_| SubCallError::ProviderBorrowed)?;
        let result = guard.sub_call(input);
        if let Err(error) = &result {
            guard.observe_trigger_error(&PrecompileError::from(error.clone()));
        }
        result
    }
}

fn subcall_status_to_bytes(output: SubCallOutput) -> std::result::Result<Bytes, PrecompileError> {
    match output.status {
        SubCallStatus::Success => Ok(output.returndata),
        SubCallStatus::Revert(bytes) => Err(PrecompileError::RevertBytes(bytes)),
        SubCallStatus::Halt(err) => Err(err.into()),
    }
}

impl<'storage, T> From<&'storage mut T> for StorageHandle<'storage>
where
    T: PrecompileStorageProvider + 'storage,
{
    fn from(provider: &'storage mut T) -> Self {
        Self::new(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::hashmap::HashMapStorageProvider;

    fn metered_provider(gas: u64) -> HashMapStorageProvider {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_gas_limit(gas);
        provider.enable_production_storage_gas_metering();
        provider
    }

    #[test]
    fn trigger_budget_swallowed_exhaustion_reverts_nested_state_and_refunds() {
        let mut provider = metered_provider(50_000);
        provider.enter(|storage| {
            storage.refund_gas(7).unwrap();
            let result = storage.with_trigger_budget(5100, || {
                storage.sstore(Address::ZERO, U256::ZERO, U256::ONE)?;
                storage.tstore(Address::ZERO, U256::ZERO, U256::ONE)?;
                storage.refund_gas(900)?;
                let inner = storage.checkpoint_guard()?;
                assert!(matches!(
                    storage.deduct_gas(101),
                    Err(PrecompileError::TriggerBudgetExceeded { .. })
                ));
                inner.revert()?;
                // Rollback cannot reset exhaustion, including for uncharged mutations.
                assert!(storage.increase_balance(Address::ZERO, U256::ONE).is_err());
                assert!(storage.emit_event(Address::ZERO, LogData::empty()).is_err());
                Ok(())
            });
            assert!(matches!(
                result,
                Err(PrecompileError::TriggerBudgetExceeded {
                    consumed_gas: 5000,
                    budget_gas: 5100
                })
            ));
            assert_eq!(storage.gas_used().unwrap(), 5000);
            assert_eq!(storage.gas_refunded().unwrap(), 7);
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
            assert_eq!(
                storage.tload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
            // Scope cleanup permits independent work with an exact successful boundary.
            storage
                .with_trigger_budget(5000, || {
                    storage.sstore(Address::ZERO, U256::ZERO, U256::from(2))
                })
                .unwrap();
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::from(2)
            );
            assert_eq!(storage.gas_remaining().unwrap(), None);
            assert_eq!(storage.metered_gas_remaining().unwrap(), Some(39_800));
        });
    }

    #[test]
    fn trigger_budget_parent_oog_is_sticky_and_separate() {
        let mut provider = metered_provider(100);
        provider.enter(|storage| {
            let result = storage.with_trigger_budget(u64::MAX, || {
                assert!(matches!(
                    storage.deduct_gas(101),
                    Err(PrecompileError::OutOfGas)
                ));
                Ok(())
            });
            assert!(matches!(result, Err(PrecompileError::OutOfGas)));
            assert_eq!(storage.metered_gas_remaining().unwrap(), Some(0));
            assert!(matches!(
                storage.with_trigger_budget(0, || Ok(())),
                Err(PrecompileError::OutOfGas)
            ));
        });
    }

    #[test]
    fn trigger_budget_unwind_rolls_back_before_clearing_scope() {
        let mut provider = metered_provider(50_000);
        provider.enter(|storage| {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _: Result<()> = storage.with_trigger_budget(5000, || {
                    storage.sstore(Address::ZERO, U256::ZERO, U256::ONE)?;
                    panic!("injected handler unwind");
                });
            }));
            assert!(panic.is_err());
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
            assert_eq!(storage.gas_used().unwrap(), 5100);
            storage.with_trigger_budget(0, || Ok(())).unwrap();
        });
    }

    #[test]
    fn trigger_budget_caught_backend_failure_cannot_become_local_deferral() {
        let mut provider = metered_provider(50_000);
        provider.fail_mutation_at(0);
        provider.enter(|storage| {
            let result = storage.with_trigger_budget(5000, || {
                assert!(matches!(
                    storage.sstore(Address::ZERO, U256::ZERO, U256::ONE),
                    Err(PrecompileError::Storage(_))
                ));
                // Even a caught failure followed by another rejected charge remains fatal.
                let _ = storage.deduct_gas(1);
                Ok(())
            });
            assert!(matches!(result, Err(PrecompileError::Fatal(_))));
            storage.with_trigger_budget(0, || Ok(())).unwrap();
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
        });
    }

    #[test]
    fn trigger_budget_rollback_closes_authority_but_does_not_restore_entitlement() {
        use crate::storage::{MetadosisCycleLifecycle, MetadosisMutationPurposeTag};
        let purpose = MetadosisMutationPurposeTag::CycleLifecycle;
        let binding = B256::repeat_byte(9);
        let mut provider = metered_provider(50_000);
        provider.enable_metadosis_mutation_frame(purpose);
        provider.enter(|storage| {
            let result = storage.with_trigger_budget(5000, || {
                storage.with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                    storage.sstore(Address::ZERO, U256::ZERO, U256::ONE)?;
                    storage.deduct_gas(1)
                })
            });
            assert!(matches!(
                result,
                Err(PrecompileError::TriggerBudgetExceeded { .. })
            ));
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
        });
        // Cleanup removed the active lease, but the one-shot grant remains consumed.
        assert!(provider
            .finish_metadosis_mutation_frame(purpose, binding, false)
            .is_err());
        assert!(provider
            .begin_metadosis_mutation_frame(purpose, binding, 1, 0)
            .is_err());
    }

    #[test]
    fn trigger_budget_direct_hook_provider_is_explicitly_unsupported() {
        use crate::{block::BlockContext, storage::direct::DirectStorageProvider};
        use revm::database::{CacheDB, EmptyDB};
        let mut db = CacheDB::new(EmptyDB::default());
        let mut provider = DirectStorageProvider::new(
            &mut db,
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: 1,
                chain_id: 1,
                genesis_hash: B256::ZERO,
                genesis_timestamp: 1,
                proposer: Address::ZERO,
                validators: Vec::new(),
            },
        );
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(storage.metered_gas_remaining().unwrap(), None);
            assert!(matches!(
                storage.with_trigger_budget(100, || Ok(())),
                Err(PrecompileError::TriggerScope(
                    crate::storage::gas::TriggerScopeError::Unsupported
                ))
            ));
        });
    }

    #[test]
    fn trigger_budget_rejects_unmetered_and_nested_scopes_without_overwriting() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            assert!(matches!(
                storage.with_trigger_budget(1, || Ok(())),
                Err(PrecompileError::TriggerScope(
                    crate::storage::gas::TriggerScopeError::Unsupported
                ))
            ))
        });
        let mut provider = metered_provider(u64::MAX);
        provider.enter(|storage| {
            storage
                .with_trigger_budget(100, || {
                    assert!(matches!(
                        storage.with_trigger_budget(u64::MAX, || Ok(())),
                        Err(PrecompileError::TriggerScope(
                            crate::storage::gas::TriggerScopeError::Nested
                        ))
                    ));
                    storage.deduct_gas(100)
                })
                .unwrap();
            assert_eq!(storage.gas_used().unwrap(), 100);
            assert!(matches!(
                storage.with_trigger_budget(0, || storage.deduct_gas(u64::MAX)),
                Err(PrecompileError::OutOfGas)
            ));
        });
    }

    #[test]
    fn trigger_budget_infrastructure_and_checkpoint_failures_outrank_exhaustion() {
        let mut provider = metered_provider(50_000);
        provider.enter(|storage| {
            for error in [
                PrecompileError::Storage("backend unavailable".into()),
                PrecompileError::BodyReadCorruption("corrupt record".into()),
                PrecompileError::Unsupported,
            ] {
                let expected = error.to_string();
                let result: Result<()> = storage.with_trigger_budget(0, || {
                    let _ = storage.deduct_gas(1);
                    Err(error)
                });
                assert_eq!(result.unwrap_err().to_string(), expected);
            }
            let result = storage.with_trigger_budget(5000, || {
                storage.sstore(Address::ZERO, U256::ZERO, U256::ONE)?;
                // An abandoned nested checkpoint must make commit fail, not defer successfully.
                let _abandoned = storage.checkpoint()?;
                Ok(())
            });
            assert!(matches!(result, Err(PrecompileError::FatalWithCause(_))));
            assert_eq!(
                storage.sload(Address::ZERO, U256::ZERO).unwrap(),
                U256::ZERO
            );
            storage.with_trigger_budget(0, || Ok(())).unwrap();
        });
    }

    #[test]
    fn checked_timestamp_preserves_bounds_and_rejects_overflow() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        for timestamp in [0, 1_734_706_800, u64::MAX] {
            handle.set_block_timestamp(U256::from(timestamp)).unwrap();
            assert_eq!(handle.now_secs().unwrap(), timestamp);
        }
        for timestamp in [U256::from(u64::MAX) + U256::from(1), U256::MAX] {
            handle.set_block_timestamp(timestamp).unwrap();
            assert!(
                matches!(handle.now_secs(), Err(PrecompileError::Revert(message))
                if message == "timestamp exceeds u64")
            );
        }
    }

    #[test]
    fn checked_timestamp_preserves_provider_borrow_failure() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        let _borrow = handle.inner.borrow_mut();
        assert!(
            matches!(handle.now_secs(), Err(PrecompileError::Revert(message))
            if message == "storage provider is already mutably borrowed")
        );
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn nested_borrow_within_single_scope_panics() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        let _outer = handle.inner.borrow_mut();
        let _inner = handle.inner.borrow_mut();
    }

    #[test]
    fn sequential_borrows_across_handles_work() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        let other = handle.clone();

        assert_eq!(handle.chain_id().unwrap(), 1);
        assert_eq!(other.chain_id().unwrap(), 1);
    }

    #[test]
    fn handle_borrow_is_fallible() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        let cp = handle.checkpoint().expect("initial checkpoint succeeds");

        let _active_borrow = handle.inner.borrow_mut();
        for err in [
            handle.checkpoint().unwrap_err(),
            handle.checkpoint_guard().map(|_| ()).unwrap_err(),
            handle.checkpoint_commit(cp).unwrap_err(),
            handle.checkpoint_revert(cp).unwrap_err(),
            handle.refund_gas(10).unwrap_err(),
        ] {
            assert_eq!(
                err.to_string(),
                crate::error::PrecompileError::from(
                    crate::error::PrimitivesStorageError::ProviderAlreadyMutablyBorrowed
                )
                .to_string()
            );
        }
    }

    #[test]
    fn out_of_order_commit_is_rejected() {
        let mut provider = HashMapStorageProvider::new(1);
        let handle = StorageHandle::new(&mut provider);
        let account = Address::repeat_byte(0x42);
        let slot = U256::from(7u64);

        let outer = handle.checkpoint_guard().unwrap();
        handle.sstore(account, slot, U256::from(10u64)).unwrap();

        let inner = handle.checkpoint_guard().unwrap();
        handle.sstore(account, slot, U256::from(20u64)).unwrap();

        // Committing the outer checkpoint while the inner checkpoint is still active
        // must fail with a typed LIFO error and leave the inner snapshot intact.
        let err = outer.commit().unwrap_err();
        assert_eq!(
            err.to_string(),
            crate::error::PrecompileError::from(
                crate::error::PrimitivesStorageError::CheckpointOutOfOrder {
                    expected: Some(1),
                    actual: 0,
                }
            )
            .to_string()
        );

        // Dropping the inner guard without committing must still revert the inner write.
        drop(inner);
        assert_eq!(handle.sload(account, slot).unwrap(), U256::from(10u64));

        // Out-of-bounds revert index must return a typed error rather than panicking.
        let bad_cp = JournalCheckpoint {
            log_i: 0,
            journal_i: 99,
            selfdestructed_i: 0,
        };
        assert_eq!(
            handle.checkpoint_revert(bad_cp).unwrap_err().to_string(),
            crate::error::PrecompileError::from(
                crate::error::PrimitivesStorageError::InvalidCheckpointRevert {
                    checkpoint: 99,
                    depth: 1,
                }
            )
            .to_string()
        );
    }
}
