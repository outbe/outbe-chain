//! In-memory storage for deterministic tests.
//!
//! With `enable_production_storage_gas_metering`, persistent reads cost 100 gas and writes cost 5000 gas.
//! These flat costs match the EVM context provider. Metering is disabled by default.
//! Slot warmth and value changes do not affect these costs.
//! Transient operations and `enclave_upgrade_id` do not charge the storage meter.

use alloy_primitives::{Address, Bytes, Log, LogData, B256, U256};
use revm::context::journaled_state::JournalCheckpoint;
use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};
use revm::state::{AccountInfo, Bytecode};
use std::collections::{BTreeMap, HashMap, VecDeque};

#[cfg(feature = "bench-utils")]
use serde::{Deserialize, Serialize};

use crate::error::{PrecompileError, PrimitivesStorageError, Result};
use crate::storage::{
    MetadosisMutationEntitlements, MetadosisMutationPurposeTag, PrecompileStorageProvider,
    StorageHandle, SubCallError, SubCallInput, SubCallOutput, SubCallStatus,
};

#[cfg(feature = "bench-utils")]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageTraceKind {
    Read,
    Write,
}

#[cfg(feature = "bench-utils")]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageTraceOperation {
    pub address: Address,
    pub slot: U256,
    pub kind: StorageTraceKind,
}

/// In-memory storage provider for unit testing.
///
/// Gas is unlimited by default; tests may install a limit and inspect exact
/// deductions to exercise deterministic out-of-gas behavior.
#[cfg_attr(feature = "bench-utils", derive(Clone))]
pub struct HashMapStorageProvider {
    pub storage: HashMap<(Address, U256), U256>,
    transient: HashMap<(Address, U256), U256>,
    accounts: HashMap<Address, AccountInfo>,
    pub events: HashMap<Address, Vec<LogData>>,
    ordered_events: Vec<Log>,
    /// canonical-history fixture used by `canonical_block_hash`.
    /// Tests seed this directly via `set_canonical_block_hash`. An unset
    /// entry yields `Ok(None)` (block outside retention / unknown).
    canonical_block_hashes: BTreeMap<u64, B256>,
    chain_id: u64,
    genesis_hash: B256,
    genesis_timestamp: u64,
    timestamp: U256,
    beneficiary: Address,
    block_number: u64,
    is_static: bool,
    gas_limit: Option<u64>,
    gas_used: u64,
    meter_storage_gas: bool,
    metered_storage_reads: u64,
    metered_storage_writes: u64,
    mutation_failure_at: Option<usize>,
    mutation_failure_address: Option<Address>,
    mutation_failure_after: bool,
    mutation_operations: usize,
    snapshots: Vec<Snapshot>,
    /// When true, `sub_call` returns `SubCallOutput::default_success()`
    /// instead of the trait default `Err(SubCallError::NotAvailable)`. Some
    /// tests exercise runtime paths that issue Rust -> Solidity sub-calls but
    /// do not assert child-frame state. These tests enable it via
    /// [`Self::enable_sub_call_stub`].
    sub_call_stub: bool,
    /// Per-address return data stubs. Entries registered via
    /// [`Self::stub_sub_call_at`] take priority over `sub_call_stub`.
    sub_call_stubs: HashMap<Address, Bytes>,
    /// Per-address-and-selector return data stubs. These take priority over
    /// address-wide stubs so tests can model contracts with multiple views.
    sub_call_selector_stubs: HashMap<(Address, [u8; 4]), Bytes>,
    /// Per-address-and-selector return data served in order, one per call,
    /// ahead of the fixed stubs. Models a view whose answer changes between
    /// calls, such as a balance around a transfer.
    sub_call_selector_sequences: HashMap<(Address, [u8; 4]), VecDeque<Bytes>>,
    /// Every sub-call input in dispatch order, so tests can decode what was sent.
    sub_call_inputs: Vec<SubCallInput>,
    metadosis_mutation_entitlements: MetadosisMutationEntitlements,
    metadosis_mutation_active: Option<(MetadosisMutationPurposeTag, B256, u64, u64)>,
    #[cfg(feature = "bench-utils")]
    storage_trace_enabled: bool,
    #[cfg(feature = "bench-utils")]
    storage_trace: Vec<StorageTraceOperation>,
}

#[cfg_attr(feature = "bench-utils", derive(Clone))]
struct Snapshot {
    storage: HashMap<(Address, U256), U256>,
    accounts: HashMap<Address, AccountInfo>,
    events: HashMap<Address, Vec<LogData>>,
    ordered_events: Vec<Log>,
    #[cfg(feature = "bench-utils")]
    storage_trace_len: usize,
}

impl HashMapStorageProvider {
    /// Creates a new test storage provider with the given chain ID.
    pub fn new(chain_id: u64) -> Self {
        Self::new_with_chain_identity(chain_id, B256::ZERO)
    }

    /// Creates a test provider with an explicit immutable chain identity.
    pub fn new_with_chain_identity(chain_id: u64, genesis_hash: B256) -> Self {
        Self {
            storage: HashMap::new(),
            transient: HashMap::new(),
            accounts: HashMap::new(),
            events: HashMap::new(),
            ordered_events: Vec::new(),
            canonical_block_hashes: BTreeMap::new(),
            chain_id,
            genesis_hash,
            genesis_timestamp: 1,
            timestamp: U256::ZERO,
            beneficiary: Address::ZERO,
            block_number: 0,
            is_static: false,
            gas_limit: None,
            gas_used: 0,
            meter_storage_gas: false,
            metered_storage_reads: 0,
            metered_storage_writes: 0,
            mutation_failure_at: None,
            mutation_failure_address: None,
            mutation_failure_after: false,
            mutation_operations: 0,
            snapshots: Vec::new(),
            sub_call_stub: false,
            sub_call_stubs: HashMap::new(),
            sub_call_selector_stubs: HashMap::new(),
            sub_call_selector_sequences: HashMap::new(),
            sub_call_inputs: Vec::new(),
            metadosis_mutation_entitlements: MetadosisMutationEntitlements::NONE,
            metadosis_mutation_active: None,
            #[cfg(feature = "bench-utils")]
            storage_trace_enabled: false,
            #[cfg(feature = "bench-utils")]
            storage_trace: Vec::new(),
        }
    }

    /// Starts a benchmark-only persistent storage trace and discards any prior
    /// trace. Production providers and default builds do not contain this path.
    #[cfg(feature = "bench-utils")]
    pub fn enable_storage_trace(&mut self) {
        self.storage_trace.clear();
        self.storage_trace_enabled = true;
    }

    /// Returns successful persistent storage operations since tracing began.
    #[cfg(feature = "bench-utils")]
    pub fn storage_trace(&self) -> &[StorageTraceOperation] {
        &self.storage_trace
    }

    #[cfg(feature = "bench-utils")]
    fn trace_storage(&mut self, address: Address, slot: U256, kind: StorageTraceKind) {
        if self.storage_trace_enabled {
            self.storage_trace.push(StorageTraceOperation {
                address,
                slot,
                kind,
            });
        }
    }

    /// Test-only provider entitlement for production-shaped Metadosis command
    /// entrypoints. It grants one purpose and still rejects nested or mismatched
    /// leases before the command callback runs.
    #[doc(hidden)]
    pub fn enable_metadosis_mutation_frame(&mut self, purpose: MetadosisMutationPurposeTag) {
        self.enable_metadosis_mutation_frames(purpose, 1);
    }

    /// Test-only fixed route budget for commands which execute a statically
    /// bounded sequence inside one simulated EVM dispatch.
    #[doc(hidden)]
    pub fn enable_metadosis_mutation_frames(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        count: u8,
    ) {
        assert!(
            self.metadosis_mutation_active.is_none(),
            "cannot replace an active Metadosis mutation lease"
        );
        self.metadosis_mutation_entitlements =
            MetadosisMutationEntitlements::repeated(purpose, count);
    }

    /// Opts the provider into stubbing `sub_call`: every dispatched sub-call
    /// returns [`SubCallOutput::default_success`] (success with empty
    /// returndata) instead of [`SubCallError::NotAvailable`].
    ///
    /// Use only in tests whose runtime now issues Rust -> Solidity sub-calls
    /// but do not assert vault/EVM state on the child frame.
    pub fn enable_sub_call_stub(&mut self) {
        self.sub_call_stub = true;
    }

    /// Register a fixed returndata stub for a specific contract address.
    ///
    /// Every call or staticcall to `address` will succeed and return the given
    /// `returndata`. Useful for tests that need to decode the return value of a
    /// sub-call (e.g. a quote function returning a fee struct) without running
    /// a real EVM sub-frame.
    pub fn stub_sub_call_at(&mut self, address: Address, returndata: Bytes) {
        self.sub_call_stubs.insert(address, returndata);
    }

    /// Register fixed returndata for one function selector at `address`.
    pub fn stub_sub_call_at_selector(
        &mut self,
        address: Address,
        selector: [u8; 4],
        returndata: Bytes,
    ) {
        self.sub_call_selector_stubs
            .insert((address, selector), returndata);
    }

    /// Register returndata for one function selector at `address`, served in
    /// order: the first call gets the first entry. Once the sequence is used
    /// up, the fixed stubs answer again.
    pub fn stub_sub_call_sequence_at_selector(
        &mut self,
        address: Address,
        selector: [u8; 4],
        returndata: impl IntoIterator<Item = Bytes>,
    ) {
        self.sub_call_selector_sequences
            .insert((address, selector), returndata.into_iter().collect());
    }

    /// Returns every sub-call input in dispatch order, including calls that failed.
    pub fn sub_call_inputs(&self) -> &[SubCallInput] {
        &self.sub_call_inputs
    }

    // Test helper methods

    pub fn get_account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.accounts.get(&address)
    }

    pub fn get_events(&self, address: Address) -> &Vec<LogData> {
        static EMPTY: Vec<LogData> = Vec::new();
        self.events.get(&address).unwrap_or(&EMPTY)
    }

    /// Returns emitted logs in their canonical cross-address order.
    pub fn get_ordered_events(&self) -> &[Log] {
        &self.ordered_events
    }

    pub fn set_nonce(&mut self, address: Address, nonce: u64) {
        self.accounts.entry(address).or_default().nonce = nonce;
    }

    pub fn set_balance(&mut self, address: Address, balance: U256) {
        self.accounts.entry(address).or_default().balance = balance;
    }

    pub fn get_balance(&self, address: Address) -> U256 {
        self.accounts
            .get(&address)
            .map(|a| a.balance)
            .unwrap_or(U256::ZERO)
    }

    pub fn set_genesis_timestamp(&mut self, genesis_timestamp: u64) {
        self.genesis_timestamp = genesis_timestamp;
    }

    pub const fn with_genesis_timestamp(mut self, genesis_timestamp: u64) -> Self {
        self.genesis_timestamp = genesis_timestamp;
        self
    }

    pub fn set_timestamp(&mut self, timestamp: U256) {
        self.timestamp = timestamp;
    }

    pub fn set_beneficiary(&mut self, beneficiary: Address) {
        self.beneficiary = beneficiary;
    }

    pub fn set_block_number(&mut self, block_number: u64) {
        self.block_number = block_number;
    }

    /// Seeds the canonical-history fixture used by
    /// [`PrecompileStorageProvider::canonical_block_hash`].
    pub fn set_canonical_block_hash(&mut self, number: u64, hash: B256) {
        self.canonical_block_hashes.insert(number, hash);
    }

    pub fn clear_transient(&mut self) {
        self.transient.clear();
    }

    pub fn clear_events(&mut self, address: Address) {
        self.events.remove(&address);
        self.ordered_events.retain(|event| event.address != address);
    }

    pub fn set_static(&mut self, is_static: bool) {
        self.is_static = is_static;
    }

    /// Enables deterministic explicit-gas testing. Calls to `deduct_gas`
    /// consume this budget exactly like the production gas tracker.
    pub fn set_gas_limit(&mut self, gas_limit: u64) {
        self.gas_limit = Some(gas_limit);
        self.gas_used = 0;
        self.metered_storage_reads = 0;
        self.metered_storage_writes = 0;
    }

    /// Opts this test provider into the production warm-SLOAD/SSTORE-reset
    /// charges. Default tests remain lightweight and unmetered.
    pub fn enable_production_storage_gas_metering(&mut self) {
        self.meter_storage_gas = true;
        self.metered_storage_reads = 0;
        self.metered_storage_writes = 0;
    }

    /// Returns the production-shaped persistent storage operations observed
    /// since the last gas-limit reset.
    pub fn metered_storage_operations(&self) -> (u64, u64) {
        (self.metered_storage_reads, self.metered_storage_writes)
    }

    /// Injects a deterministic failure immediately before the zero-based
    /// persistent-write/event operation selected by `operation`.
    pub fn fail_mutation_at(&mut self, operation: usize) {
        self.mutation_failure_at = Some(operation);
        self.mutation_failure_address = None;
        self.mutation_failure_after = false;
        self.mutation_operations = 0;
    }

    /// Injects a deterministic failure before the first persistent mutation
    /// owned by `address`.
    ///
    /// This keeps activation rollback tests coupled to owner boundaries rather
    /// than to incidental storage-operation ordinals.
    pub fn fail_mutation_at_address(&mut self, address: Address) {
        self.mutation_failure_at = None;
        self.mutation_failure_address = Some(address);
        self.mutation_failure_after = false;
        self.mutation_operations = 0;
    }

    /// Injects a deterministic failure immediately after the provider applies
    /// the zero-based persistent-write/event operation selected by `operation`.
    /// The caller's journal checkpoint must restore that write.
    pub fn fail_after_mutation_at(&mut self, operation: usize) {
        self.mutation_failure_at = Some(operation);
        self.mutation_failure_address = None;
        self.mutation_failure_after = true;
        self.mutation_operations = 0;
    }

    /// Disables mutation failure injection and returns the number of
    /// persistent-write/event operations observed since the last reset.
    pub fn clear_mutation_failure(&mut self) -> usize {
        self.mutation_failure_at = None;
        self.mutation_failure_address = None;
        self.mutation_failure_after = false;
        std::mem::take(&mut self.mutation_operations)
    }

    /// Characterizes the mutation order of `call`.
    ///
    /// It runs `call` once on storage from `seed` and counts its
    /// persistent-write/event operations. Then, for each operation `n`, it runs
    /// `call` on new storage from `seed` with a failure injected before
    /// operation `n`. `view` reads the storage after each run. Each failing run
    /// must stop with exactly the injected error, else the result is an error.
    pub fn mutation_prefix_views<T, V>(
        seed: impl Fn() -> Self,
        call: impl Fn(StorageHandle) -> Result<T>,
        view: impl Fn(&mut Self) -> V,
    ) -> Result<MutationPrefixViews<V>> {
        let mut provider = seed();
        provider.clear_mutation_failure();
        provider.enter(&call)?;
        let mutations = provider.clear_mutation_failure();
        let complete = view(&mut provider);

        let mut before_mutation = Vec::with_capacity(mutations);
        for operation in 0..mutations {
            let mut provider = seed();
            provider.fail_mutation_at(operation);
            let outcome = provider.enter(&call);
            provider.clear_mutation_failure();
            let expected = PrecompileError::from(
                PrimitivesStorageError::InjectedMutationFailureBeforeOperation { operation },
            );
            if !matches!(&outcome, Err(error) if error.is_node_local() && error.to_string() == expected.to_string())
            {
                return Err(
                    PrimitivesStorageError::InjectedMutationDidNotStop { operation }.into(),
                );
            }
            before_mutation.push(view(&mut provider));
        }
        Ok(MutationPrefixViews {
            before_mutation,
            complete,
            mutations,
        })
    }

    fn before_mutation(&mut self, address: Address) -> Result<()> {
        if self.mutation_failure_address == Some(address) {
            return Err(PrimitivesStorageError::InjectedMutationFailureForOwner { address }.into());
        }
        if !self.mutation_failure_after
            && self.mutation_failure_at == Some(self.mutation_operations)
        {
            return Err(
                PrimitivesStorageError::InjectedMutationFailureBeforeOperation {
                    operation: self.mutation_operations,
                }
                .into(),
            );
        }
        Ok(())
    }

    fn after_mutation(&mut self) -> Result<()> {
        let operation = self.mutation_operations;
        self.mutation_operations = self.mutation_operations.saturating_add(1);
        if self.mutation_failure_after && self.mutation_failure_at == Some(operation) {
            Err(PrimitivesStorageError::InjectedMutationFailureAfterOperation { operation }.into())
        } else {
            Ok(())
        }
    }

    pub fn enter<R>(&mut self, f: impl FnOnce(StorageHandle) -> R) -> R {
        StorageHandle::enter(self, f)
    }
}

/// The storage views of one mutation-order characterization. See
/// [`HashMapStorageProvider::mutation_prefix_views`].
#[derive(Debug, PartialEq, Eq)]
pub struct MutationPrefixViews<V> {
    /// Entry `n` is the view after a failure before mutation `n`.
    pub before_mutation: Vec<V>,
    /// The view after the complete call.
    pub complete: V,
    /// The persistent-write/event operations of the complete call.
    pub mutations: usize,
}

impl PrecompileStorageProvider for HashMapStorageProvider {
    fn begin_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> Result<()> {
        if self.metadosis_mutation_active.is_some()
            || self.chain_id != chain_id
            || self.block_number != block_number
            || !self
                .metadosis_mutation_entitlements
                .consume(purpose, binding)
        {
            return Err(PrimitivesStorageError::TestProviderNoMatchingMetadosisLease.into());
        }
        self.metadosis_mutation_active = Some((purpose, binding, chain_id, block_number));
        Ok(())
    }

    fn require_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        chain_id: u64,
        block_number: u64,
    ) -> Result<()> {
        if self.metadosis_mutation_active != Some((purpose, binding, chain_id, block_number)) {
            return Err(PrimitivesStorageError::TestProviderMetadosisLeaseIdentityMismatch.into());
        }
        Ok(())
    }

    fn finish_metadosis_mutation_frame(
        &mut self,
        purpose: MetadosisMutationPurposeTag,
        binding: B256,
        _completed: bool,
    ) -> Result<()> {
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
        self.chain_id
    }

    fn genesis_hash(&self) -> B256 {
        self.genesis_hash
    }

    fn genesis_timestamp(&self) -> u64 {
        self.genesis_timestamp
    }

    fn timestamp(&self) -> U256 {
        self.timestamp
    }

    fn set_block_timestamp(&mut self, timestamp: U256) {
        self.timestamp = timestamp;
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }

    fn block_number(&self) -> u64 {
        self.block_number
    }

    fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
        Ok(self.canonical_block_hashes.get(&number).copied())
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        let account = self.accounts.entry(address).or_default();
        account.code_hash = code.hash_slow();
        account.code = Some(code);
        Ok(())
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        Ok(self.accounts.entry(address).or_default().clone())
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        if self.meter_storage_gas {
            self.deduct_gas(WARM_STORAGE_READ_COST)?;
            self.metered_storage_reads = self.metered_storage_reads.saturating_add(1);
        }
        let value = self
            .storage
            .get(&(address, key))
            .copied()
            .unwrap_or(U256::ZERO);
        #[cfg(feature = "bench-utils")]
        self.trace_storage(address, key, StorageTraceKind::Read);
        Ok(value)
    }
    fn enclave_upgrade_id(&mut self) -> Result<U256> {
        Ok(self
            .storage
            .get(&(crate::addresses::TEE_REGISTRY_ADDRESS, U256::from(44)))
            .copied()
            .unwrap_or(U256::ZERO))
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        Ok(self
            .transient
            .get(&(address, key))
            .copied()
            .unwrap_or(U256::ZERO))
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        if self.meter_storage_gas {
            self.deduct_gas(SSTORE_RESET)?;
            self.metered_storage_writes = self.metered_storage_writes.saturating_add(1);
        }
        self.before_mutation(address)?;
        self.storage.insert((address, key), value);
        self.after_mutation()?;
        #[cfg(feature = "bench-utils")]
        self.trace_storage(address, key, StorageTraceKind::Write);
        Ok(())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.transient.insert((address, key), value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.before_mutation(address)?;
        self.ordered_events.push(Log {
            address,
            data: event.clone(),
        });
        self.events.entry(address).or_default().push(event);
        self.after_mutation()
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<()> {
        let Some(limit) = self.gas_limit else {
            return Ok(());
        };
        let next = self
            .gas_used
            .checked_add(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        if next > limit {
            return Err(PrecompileError::OutOfGas);
        }
        self.gas_used = next;
        Ok(())
    }

    fn refund_gas(&mut self, _gas: i64) {}

    fn gas_used(&self) -> u64 {
        self.gas_used
    }

    fn gas_refunded(&self) -> i64 {
        0
    }

    fn is_static(&self) -> bool {
        self.is_static
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        let idx = self.snapshots.len();
        self.snapshots.push(Snapshot {
            storage: self.storage.clone(),
            accounts: self.accounts.clone(),
            events: self.events.clone(),
            ordered_events: self.ordered_events.clone(),
            #[cfg(feature = "bench-utils")]
            storage_trace_len: self.storage_trace.len(),
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
            self.storage = snapshot.storage;
            self.accounts = snapshot.accounts;
            self.events = snapshot.events;
            self.ordered_events = snapshot.ordered_events;
            #[cfg(feature = "bench-utils")]
            self.storage_trace.truncate(snapshot.storage_trace_len);
        }
        Ok(())
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let from_balance = self.accounts.entry(from).or_default().balance;
        if from_balance < amount {
            return Err(PrimitivesStorageError::InsufficientBalance {
                from,
                balance: from_balance,
                needed: amount,
            }
            .into());
        }

        // Self-transfer is a no-op, as in `DirectStorageProvider`.
        if from == to {
            return Ok(());
        }

        let to_balance = self
            .accounts
            .get(&to)
            .map_or(U256::ZERO, |info| info.balance);
        let credited = to_balance.checked_add(amount).ok_or(
            PrimitivesStorageError::CreditBalanceOverflow {
                amount,
                to,
                balance: to_balance,
            },
        )?;
        self.accounts.entry(from).or_default().balance -= amount;
        self.accounts.entry(to).or_default().balance = credited;
        Ok(())
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let info = self.accounts.entry(address).or_default();
        info.balance = info.balance.checked_add(amount).ok_or(
            PrimitivesStorageError::IncreaseBalanceOverflow {
                address,
                amount,
                balance: info.balance,
            },
        )?;
        Ok(())
    }

    fn sub_call(
        &mut self,
        input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.sub_call_inputs.push(input.clone());
        if let Some(selector) = input
            .calldata
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
        {
            let next = self
                .sub_call_selector_sequences
                .get_mut(&(input.target, selector))
                .and_then(VecDeque::pop_front);
            if let Some(returndata) = next.or_else(|| {
                self.sub_call_selector_stubs
                    .get(&(input.target, selector))
                    .cloned()
            }) {
                return Ok(SubCallOutput {
                    status: SubCallStatus::Success,
                    returndata,
                    gas_used: 0,
                    gas_refunded: 0,
                });
            }
        }
        if let Some(returndata) = self.sub_call_stubs.get(&input.target).cloned() {
            return Ok(SubCallOutput {
                status: SubCallStatus::Success,
                returndata,
                gas_used: 0,
                gas_refunded: 0,
            });
        }
        if self.sub_call_stub {
            Ok(SubCallOutput::default_success())
        } else {
            Err(SubCallError::NotAvailable)
        }
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        let entry = self.accounts.entry(address).or_default();
        if entry.balance < amount {
            return Err(PrimitivesStorageError::InsufficientBalanceForBurn {
                address,
                balance: entry.balance,
                needed: amount,
            }
            .into());
        }
        entry.balance -= amount;
        Ok(())
    }
}

#[cfg(test)]
mod metadosis_mutation_frame_tests {
    use super::HashMapStorageProvider;
    use crate::storage::{MetadosisMutationPurposeTag as Purpose, PrecompileStorageProvider};
    use alloy_primitives::B256;

    #[test]
    fn wrong_finish_does_not_destroy_the_test_provider_frame() {
        let purpose = Purpose::CycleLifecycle;
        let binding = B256::repeat_byte(1);
        let mut provider = HashMapStorageProvider::new(7);
        provider.set_block_number(11);
        provider.enable_metadosis_mutation_frame(purpose);
        provider
            .begin_metadosis_mutation_frame(purpose, binding, 7, 11)
            .unwrap();

        assert!(provider
            .finish_metadosis_mutation_frame(purpose, B256::repeat_byte(2), false)
            .is_err());
        provider
            .finish_metadosis_mutation_frame(purpose, binding, false)
            .unwrap();
        assert!(provider
            .begin_metadosis_mutation_frame(purpose, binding, 7, 11)
            .is_err());
    }
}
