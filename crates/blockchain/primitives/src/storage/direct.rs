use std::collections::HashMap;

use alloy_evm::block::StateDB;
use alloy_primitives::{map::AddressMap, Address, Log, LogData, B256, U256};
use revm::{
    context::journaled_state::JournalCheckpoint,
    database::Database,
    state::{Account, AccountInfo, Bytecode, EvmStorage, EvmStorageSlot},
};

use crate::block::BlockContext;
use crate::error::{PrimitivesStorageError, Result};
use crate::storage::PrecompileStorageProvider;
#[cfg(feature = "test-utils")]
use crate::storage::{MetadosisMutationEntitlements, MetadosisMutationPurposeTag};

#[cfg(feature = "test-utils")]
std::thread_local! {
    static NEXT_DIRECT_FRAME_BUDGET: std::cell::Cell<
        Option<(MetadosisMutationPurposeTag, u8)>,
    > = const { std::cell::Cell::new(None) };
}

/// Installs one lease budget for the next [`DirectStorageProvider::new`] on
/// this thread. The constructor takes it. A later constructor stays denied.
#[cfg(feature = "test-utils")]
pub fn arm_next_direct_metadosis_frame_budget(purpose: MetadosisMutationPurposeTag, count: u8) {
    NEXT_DIRECT_FRAME_BUDGET.with(|slot| slot.set(Some((purpose, count))));
}

/// Storage provider for block-level hooks, wrapping a mutable [`StateDB`].
///
/// Reads go directly to the provided database.
/// Writes accumulate in a pending [`HashMap`].
/// [`DirectStorageProvider::flush`] commits them to the underlying state.
///
/// The provider tracks all state mutations (storage writes and balance
/// transfers). Callers can then retrieve the complete [`EvmState`] change set
/// via [`DirectStorageProvider::take_committed_changes`] to notify reth's
/// parallel state root task.
///
/// This is the block-hook counterpart to `outbe_evm::storage::CtxStorageProvider`,
/// which requires the revm context (only available inside a precompile call).
/// Block-level hooks such as `apply_pre_execution_changes()` can use any
/// mutable database that supports reads plus `commit()`.
pub struct DirectStorageProvider<'a, DB: StateDB> {
    state: &'a mut DB,
    /// Pending storage writes: address -> (slot -> new_value).
    pending: HashMap<Address, HashMap<U256, U256>>,
    /// Pending account-info updates such as hook-level balance transfers/burns.
    pending_accounts: AddressMap<AccountInfo>,
    /// Accumulated state changes committed by `flush()`.
    /// Used to notify reth's state root hook after all hooks complete.
    committed_changes: AddressMap<Account>,
    /// Events emitted by block-level hooks. They are not journaled into EVM
    /// receipts. The provider collects them so the executor can log them via
    /// tracing for observability.
    events: Vec<Log>,
    snapshots: Vec<DirectSnapshot>,
    ctx: BlockContext,
    /// Explicit fixture opt-in. Construction leaves this false, so begin,
    /// require, and finish keep the default denial.
    #[cfg(feature = "test-utils")]
    metadosis_frames_enabled: bool,
    #[cfg(feature = "test-utils")]
    metadosis_mutation_entitlements: MetadosisMutationEntitlements,
    #[cfg(feature = "test-utils")]
    metadosis_mutation_active: Option<(MetadosisMutationPurposeTag, B256, u64, u64)>,
}

struct DirectSnapshot {
    pending: HashMap<Address, HashMap<U256, U256>>,
    pending_accounts: AddressMap<AccountInfo>,
    events: Vec<Log>,
}

impl<'a, DB: StateDB> DirectStorageProvider<'a, DB> {
    /// Creates a new provider wrapping the given state and block context.
    pub fn new(state: &'a mut DB, ctx: BlockContext) -> Self {
        #[cfg(feature = "test-utils")]
        let (metadosis_frames_enabled, metadosis_mutation_entitlements) = NEXT_DIRECT_FRAME_BUDGET
            .with(|slot| match slot.take() {
                Some((purpose, count)) => (
                    true,
                    MetadosisMutationEntitlements::repeated(purpose, count),
                ),
                None => (false, MetadosisMutationEntitlements::NONE),
            });
        Self {
            state,
            pending: HashMap::new(),
            pending_accounts: AddressMap::default(),
            committed_changes: AddressMap::default(),
            events: Vec::new(),
            snapshots: Vec::new(),
            ctx,
            #[cfg(feature = "test-utils")]
            metadosis_frames_enabled,
            #[cfg(feature = "test-utils")]
            metadosis_mutation_entitlements,
            #[cfg(feature = "test-utils")]
            metadosis_mutation_active: None,
        }
    }

    /// Replaces the fixture frame budget with one purpose and `count` leases.
    /// This does not open a frame. A caller that never uses this method keeps
    /// the default denial.
    #[cfg(feature = "test-utils")]
    pub fn enable_metadosis_mutation_frames(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        count: u8,
    ) {
        assert!(
            self.metadosis_mutation_active.is_none(),
            "cannot replace an active Metadosis mutation lease"
        );
        self.metadosis_frames_enabled = true;
        self.metadosis_mutation_entitlements =
            MetadosisMutationEntitlements::repeated(purpose, count);
    }

    fn effective_account_info(&mut self, address: Address) -> Result<AccountInfo>
    where
        <DB as Database>::Error: std::fmt::Display,
    {
        if let Some(info) = self.pending_accounts.get(&address) {
            return Ok(info.clone());
        }

        self.state
            .basic(address)
            .map_err(|e| {
                PrimitivesStorageError::BasicRead {
                    address,
                    reason: e.to_string(),
                }
                .into()
            })
            .map(|info| info.unwrap_or_default())
    }

    /// Returns all events emitted by block-level hooks.
    ///
    /// These are not part of EVM transaction receipts but can be logged
    /// via tracing for operator observability (tally results, slashing, etc.).
    pub fn take_events(&mut self) -> Vec<Log> {
        std::mem::take(&mut self.events)
    }

    /// Commits all pending writes accumulated via [`sstore`](Self::sstore) to
    /// the underlying [`State<DB>`].
    ///
    /// Call it after the contract logic completes, so that writes are visible
    /// to later steps of block processing.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::PrecompileError::Storage`] if reading the original slot value
    /// from the database fails.
    pub fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() && self.pending_accounts.is_empty() {
            return Ok(());
        }

        // Collect writes first to release borrows before calling into `state`.
        let mut writes: HashMap<Address, Vec<(U256, U256)>> = self
            .pending
            .drain()
            .map(|(addr, slots)| (addr, slots.into_iter().collect()))
            .collect();
        let pending_accounts = std::mem::take(&mut self.pending_accounts);

        for addr in pending_accounts.keys() {
            writes.entry(*addr).or_default();
        }

        let mut changes: AddressMap<Account> =
            AddressMap::with_capacity_and_hasher(writes.len(), Default::default());

        for (addr, slots) in writes {
            // Load existing account info so the commit does not accidentally
            // wipe the nonce / balance / code of the contract.
            let info = match pending_accounts.get(&addr) {
                Some(info) => info.clone(),
                None => self
                    .state
                    .basic(addr)
                    .map_err(|e| PrimitivesStorageError::BasicRead {
                        address: addr,
                        reason: e.to_string(),
                    })?
                    .unwrap_or_default(),
            };

            // Build the EvmStorage map, reading the original slot value for
            // each write so that the journal can produce correct reverts.
            let mut storage = EvmStorage::default();
            for (key, new_value) in slots {
                let original = self.state.storage(addr, key).map_err(|e| {
                    PrimitivesStorageError::StorageSlotRead {
                        address: addr,
                        key,
                        reason: e.to_string(),
                    }
                })?;
                storage.insert(
                    key,
                    EvmStorageSlot::new_changed(
                        original,
                        new_value,
                        revm::state::TransactionId::ZERO,
                    ),
                );
            }

            let mut account = Account::from(info);
            account.storage = storage;
            account.mark_touch();

            changes.insert(addr, account);
        }

        // Track for state root hook notification.
        self.merge_into_committed(&changes);
        self.state.commit(changes);
        Ok(())
    }

    /// Returns all accumulated state changes from `flush()`. The caller uses
    /// them to notify reth's parallel state root task via the `OnStateHook`.
    /// Call this after `flush()`.
    pub fn take_committed_changes(&mut self) -> AddressMap<Account> {
        std::mem::take(&mut self.committed_changes)
    }

    /// Merge account changes into the accumulated committed_changes,
    /// combining storage slots when the same address appears multiple times.
    fn merge_into_committed(&mut self, changes: &AddressMap<Account>) {
        for (addr, account) in changes {
            match self.committed_changes.entry(*addr) {
                revm::primitives::map::hash_map::Entry::Vacant(e) => {
                    e.insert(account.clone());
                }
                revm::primitives::map::hash_map::Entry::Occupied(mut e) => {
                    let existing = e.get_mut();
                    // Merge storage: new slots override existing
                    for (key, slot) in &account.storage {
                        existing.storage.insert(*key, slot.clone());
                    }
                    // Update account info to latest
                    existing.info = account.info.clone();
                }
            }
        }
    }
}

impl<DB: StateDB> PrecompileStorageProvider for DirectStorageProvider<'_, DB>
where
    <DB as Database>::Error: std::fmt::Display,
{
    #[cfg(feature = "test-utils")]
    fn begin_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> Result<()> {
        if !self.metadosis_frames_enabled {
            return Err(PrimitivesStorageError::MetadosisMutationFrameUnavailable.into());
        }
        if self.metadosis_mutation_active.is_some()
            || self.ctx.chain_id != chain_id
            || self.ctx.block_number != block_number
            || !self
                .metadosis_mutation_entitlements
                .consume(purpose, binding)
        {
            return Err(PrimitivesStorageError::TestProviderNoMatchingMetadosisLease.into());
        }
        self.metadosis_mutation_active = Some((purpose, binding, chain_id, block_number));
        Ok(())
    }

    #[cfg(feature = "test-utils")]
    fn require_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> Result<()> {
        if !self.metadosis_frames_enabled {
            return Err(PrimitivesStorageError::MetadosisMutationFrameUnavailable.into());
        }
        if self.metadosis_mutation_active != Some((purpose, binding, chain_id, block_number)) {
            return Err(PrimitivesStorageError::TestProviderMetadosisLeaseIdentityMismatch.into());
        }
        Ok(())
    }

    #[cfg(feature = "test-utils")]
    fn finish_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        _completed: bool,
    ) -> Result<()> {
        if !self.metadosis_frames_enabled {
            return Err(PrimitivesStorageError::MetadosisMutationFrameUnavailable.into());
        }
        let Some((active_purpose, active_binding, _, _)) =
            self.metadosis_mutation_active.as_ref().copied()
        else {
            return Err(PrimitivesStorageError::TestProviderNoActiveMetadosisLease.into());
        };
        if active_purpose != purpose || active_binding != binding {
            return Err(PrimitivesStorageError::TestProviderMetadosisLeaseIdentityMismatch.into());
        }
        self.metadosis_mutation_active = None;
        Ok(())
    }

    fn chain_id(&self) -> u64 {
        self.ctx.chain_id
    }

    fn genesis_hash(&self) -> B256 {
        self.ctx.genesis_hash
    }

    fn genesis_timestamp(&self) -> u64 {
        self.ctx.genesis_timestamp
    }

    fn timestamp(&self) -> U256 {
        U256::from(self.ctx.timestamp)
    }

    fn beneficiary(&self) -> Address {
        self.ctx.proposer
    }

    fn block_number(&self) -> u64 {
        self.ctx.block_number
    }

    fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
        // Reth's `StateProviderDatabase::block_hash` returns the canonical
        // hash for any in-window block and `B256::ZERO` otherwise. Map ZERO
        // to `None` so callers see a clean "unknown" signal.
        let hash =
            self.state
                .block_hash(number)
                .map_err(|e| PrimitivesStorageError::BlockHashRead {
                    number,
                    reason: e.to_string(),
                })?;
        Ok((!hash.is_zero()).then_some(hash))
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        let mut info = self.effective_account_info(address)?;
        info.code_hash = code.hash_slow();
        info.code = Some(code);
        self.pending_accounts.insert(address, info);
        Ok(())
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        self.effective_account_info(address)
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        // Pending writes shadow the committed state.
        if let Some(value) = self.pending.get(&address).and_then(|s| s.get(&key)) {
            return Ok(*value);
        }
        self.state.storage(address, key).map_err(|e| {
            PrimitivesStorageError::StorageSlotRead {
                address,
                key,
                reason: e.to_string(),
            }
            .into()
        })
    }

    fn tload(&mut self, _address: Address, _key: U256) -> Result<U256> {
        // Transient storage does not apply in block-level hooks.
        Ok(U256::ZERO)
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.pending.entry(address).or_default().insert(key, value);
        Ok(())
    }

    fn tstore(&mut self, _address: Address, _key: U256, _value: U256) -> Result<()> {
        // No-op: transient storage does not apply in block-level hooks.
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.events.push(Log {
            address,
            data: event,
        });
        Ok(())
    }

    fn deduct_gas(&mut self, _gas: u64) -> Result<()> {
        // No gas accounting in block-level hooks.
        Ok(())
    }

    fn refund_gas(&mut self, _gas: i64) {}

    fn gas_used(&self) -> u64 {
        0
    }

    fn gas_refunded(&self) -> i64 {
        0
    }

    fn is_static(&self) -> bool {
        // DirectStorageProvider serves block-level hooks (begin_block /
        // end_block) that are by construction non-STATICCALL contexts.
        // Always `false`. If a hook were ever invoked from a static
        // frame, the executor would route through a different provider.
        false
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        let idx = self.snapshots.len();
        self.snapshots.push(DirectSnapshot {
            pending: self.pending.clone(),
            pending_accounts: self.pending_accounts.clone(),
            events: self.events.clone(),
        });
        JournalCheckpoint {
            log_i: 0,
            journal_i: idx,
            selfdestructed_i: 0,
        }
    }

    fn checkpoint_commit(&mut self, checkpoint: JournalCheckpoint) -> Result<()> {
        super::require_innermost_checkpoint(self.snapshots.len(), checkpoint)?;
        self.snapshots.pop();
        Ok(())
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) -> Result<()> {
        super::require_live_checkpoint(self.snapshots.len(), checkpoint)?;
        if let Some(snapshot) = self.snapshots.drain(checkpoint.journal_i..).next() {
            self.pending = snapshot.pending;
            self.pending_accounts = snapshot.pending_accounts;
            self.events = snapshot.events;
        }
        Ok(())
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        // Self-transfer is a no-op. This prevents a double-insert overwrite
        // that would create tokens out of nothing.
        if from == to {
            return Ok(());
        }

        let mut from_info = self.effective_account_info(from)?;

        if from_info.balance < amount {
            return Err(PrimitivesStorageError::InsufficientBalance {
                from,
                balance: from_info.balance,
                needed: amount,
            }
            .into());
        }

        let mut to_info = self.effective_account_info(to)?;
        let credited = to_info.balance.checked_add(amount).ok_or(
            PrimitivesStorageError::CreditBalanceOverflow {
                amount,
                to,
                balance: to_info.balance,
            },
        )?;
        from_info.balance -= amount;
        to_info.balance = credited;
        self.pending_accounts.insert(from, from_info);
        self.pending_accounts.insert(to, to_info);
        Ok(())
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let mut info = self.effective_account_info(address)?;
        info.balance = info.balance.checked_add(amount).ok_or(
            PrimitivesStorageError::IncreaseBalanceOverflow {
                address,
                amount,
                balance: info.balance,
            },
        )?;
        self.pending_accounts.insert(address, info);
        Ok(())
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let mut info = self.effective_account_info(address)?;

        if info.balance < amount {
            return Err(PrimitivesStorageError::InsufficientBalanceForBurn {
                address,
                balance: info.balance,
                needed: amount,
            }
            .into());
        }

        info.balance -= amount;
        self.pending_accounts.insert(address, info);
        Ok(())
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod metadosis_frame_tests {
    use alloy_primitives::{address, Address, B256, U256};
    use revm::database::{CacheDB, EmptyDB};

    use super::*;
    use crate::error::PrecompileError;
    use crate::storage::{
        metadosis_process_ready_binding, MetadosisCycleLifecycle, MetadosisMutationPurposeTag,
        StorageHandle,
    };

    const SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);
    const ACCOUNT: Address = address!("0x00000000000000000000000000000000000000d1");

    fn provider(db: &mut CacheDB<EmptyDB>) -> DirectStorageProvider<'_, CacheDB<EmptyDB>> {
        DirectStorageProvider::new(db, BlockContext::empty_for_tests(9, 1_700_000_000, 42))
    }

    fn denial(result: Result<()>) -> String {
        result.expect_err("frame call must fail").to_string()
    }

    #[test]
    fn default_direct_denies_a_frame_without_an_opt_in() {
        let mut db = CacheDB::new(EmptyDB::default());
        let mut direct = provider(&mut db);
        let binding = B256::repeat_byte(0x11);
        let unavailable = "Metadosis mutation frame is not available";
        assert!(denial(direct.begin_metadosis_mutation_frame(
            MetadosisMutationPurposeTag::CycleLifecycle,
            binding,
            42,
            9,
        ))
        .contains(unavailable));
        assert!(denial(direct.require_metadosis_mutation_frame(
            MetadosisMutationPurposeTag::CycleLifecycle,
            binding,
            42,
            9,
        ))
        .contains(unavailable));
        assert!(denial(direct.finish_metadosis_mutation_frame(
            MetadosisMutationPurposeTag::CycleLifecycle,
            binding,
            true,
        ))
        .contains(unavailable));
    }

    #[test]
    fn opted_in_frame_checks_scope_nesting_binding_and_rolls_back() {
        let mut db = CacheDB::new(EmptyDB::default());
        let mut direct = provider(&mut db);
        direct.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 1);
        let wrong_chain = denial(direct.begin_metadosis_mutation_frame(
            MetadosisMutationPurposeTag::CycleLifecycle,
            B256::repeat_byte(0x22),
            7,
            9,
        ));
        assert!(wrong_chain.contains("no matching Metadosis mutation lease"));

        StorageHandle::enter(&mut direct, |storage| {
            let binding = metadosis_process_ready_binding(
                storage.chain_id()?,
                storage.block_number()?,
                storage.now_secs()?,
            );
            storage.with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                storage.sstore(ACCOUNT, SLOT, U256::from(4))?;
                let nested = storage
                    .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || Ok(()));
                assert!(denial(nested).contains("no matching Metadosis mutation lease"));
                assert_eq!(storage.sload(ACCOUNT, SLOT)?, U256::from(4));
                let mismatch = storage.require_metadosis_init_genesis();
                assert!(denial(mismatch).contains("lease identity mismatch"));
                Ok(())
            })?;
            assert_eq!(storage.sload(ACCOUNT, SLOT)?, U256::from(4));
            let consumed =
                storage.with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || Ok(()));
            assert!(denial(consumed).contains("no matching Metadosis mutation lease"));
            Ok::<_, PrecompileError>(())
        })
        .unwrap();

        direct.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 1);
        StorageHandle::enter(&mut direct, |storage| {
            let binding = metadosis_process_ready_binding(
                storage.chain_id()?,
                storage.block_number()?,
                storage.now_secs()?,
            );
            let failed =
                storage.with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                    storage.sstore(ACCOUNT, SLOT, U256::from(9))?;
                    Err::<(), PrecompileError>(PrecompileError::Revert("stop".into()))
                });
            assert!(failed.is_err());
            assert_eq!(storage.sload(ACCOUNT, SLOT)?, U256::from(4));
            let closed =
                storage.with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || Ok(()));
            assert!(denial(closed).contains("no matching Metadosis mutation lease"));
            Ok::<_, PrecompileError>(())
        })
        .unwrap();
    }

    #[test]
    fn armed_constructor_budget_is_one_shot() {
        arm_next_direct_metadosis_frame_budget(MetadosisMutationPurposeTag::CycleLifecycle, 1);
        let mut first_db = CacheDB::new(EmptyDB::default());
        let mut first = provider(&mut first_db);
        first
            .begin_metadosis_mutation_frame(
                MetadosisMutationPurposeTag::CycleLifecycle,
                B256::repeat_byte(0x31),
                42,
                9,
            )
            .unwrap();
        let mut second_db = CacheDB::new(EmptyDB::default());
        let mut second = provider(&mut second_db);
        assert!(denial(second.begin_metadosis_mutation_frame(
            MetadosisMutationPurposeTag::CycleLifecycle,
            B256::repeat_byte(0x31),
            42,
            9,
        ))
        .contains("Metadosis mutation frame is not available"));
    }
}
