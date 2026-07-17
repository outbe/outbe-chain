use alloy_primitives::{Address, Bytes, LogData, B256, U256};
use revm::context::journaled_state::JournalCheckpoint;
use revm::state::{AccountInfo, Bytecode};
use std::collections::{BTreeMap, HashMap};

use crate::error::PrecompileError;

use crate::error::Result;
use crate::storage::{
    PrecompileStorageProvider, StorageHandle, SubCallError, SubCallInput, SubCallOutput,
    SubCallStatus,
};

/// In-memory storage provider for unit testing.
///
/// No gas tracking — all gas operations are no-ops.
pub struct HashMapStorageProvider {
    pub storage: HashMap<(Address, U256), U256>,
    transient: HashMap<(Address, U256), U256>,
    accounts: HashMap<Address, AccountInfo>,
    pub events: HashMap<Address, Vec<LogData>>,
    /// canonical-history fixture used by `canonical_block_hash`.
    /// Tests seed this directly via `set_canonical_block_hash`; an unset
    /// entry yields `Ok(None)` (block outside retention / unknown).
    canonical_block_hashes: BTreeMap<u64, B256>,
    chain_id: u64,
    timestamp: U256,
    beneficiary: Address,
    block_number: u64,
    is_static: bool,
    snapshots: Vec<Snapshot>,
    /// When true, `sub_call` returns `SubCallOutput::default_success()`
    /// instead of the trait default `Err(SubCallError::NotAvailable)`. Tests
    /// that exercise runtime paths which issue Rust → Solidity sub-calls but
    /// don't assert child-frame state opt in via [`Self::enable_sub_call_stub`].
    sub_call_stub: bool,
    /// Per-address return data stubs. Entries registered via
    /// [`Self::stub_sub_call_at`] take priority over `sub_call_stub`.
    sub_call_stubs: HashMap<Address, Bytes>,
    /// Calldata of every dispatched sub-call, in order, for tests that assert the
    /// outbound calls a runtime path makes (e.g. a per-chain message fan-out).
    sub_call_log: Vec<(Address, Bytes)>,
}

struct Snapshot {
    storage: HashMap<(Address, U256), U256>,
    accounts: HashMap<Address, AccountInfo>,
    events: HashMap<Address, Vec<LogData>>,
}

impl HashMapStorageProvider {
    /// Creates a new test storage provider with the given chain ID.
    pub fn new(chain_id: u64) -> Self {
        Self {
            storage: HashMap::new(),
            transient: HashMap::new(),
            accounts: HashMap::new(),
            events: HashMap::new(),
            canonical_block_hashes: BTreeMap::new(),
            chain_id,
            timestamp: U256::ZERO,
            beneficiary: Address::ZERO,
            block_number: 0,
            is_static: false,
            snapshots: Vec::new(),
            sub_call_stub: false,
            sub_call_stubs: HashMap::new(),
            sub_call_log: Vec::new(),
        }
    }

    /// Opts the provider into stubbing `sub_call`: every dispatched sub-call
    /// returns [`SubCallOutput::default_success`] (success with empty
    /// returndata) instead of [`SubCallError::NotAvailable`].
    ///
    /// Use only in tests whose runtime now issues Rust → Solidity sub-calls
    /// (e.g. credisfactory `request_credis` / `pay_anadosis` calling
    /// `IVaultProvider` and `IERC20`) but do not assert vault/EVM state on the
    /// child frame.
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

    // Test helper methods

    pub fn get_account_info(&self, address: Address) -> Option<&AccountInfo> {
        self.accounts.get(&address)
    }

    pub fn get_events(&self, address: Address) -> &Vec<LogData> {
        static EMPTY: Vec<LogData> = Vec::new();
        self.events.get(&address).unwrap_or(&EMPTY)
    }

    /// Calldata of every sub-call dispatched to `address`, in dispatch order.
    pub fn recorded_sub_calls(&self, address: Address) -> Vec<Bytes> {
        self.sub_call_log
            .iter()
            .filter(|(target, _)| *target == address)
            .map(|(_, data)| data.clone())
            .collect()
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
    }

    pub fn set_static(&mut self, is_static: bool) {
        self.is_static = is_static;
    }

    pub fn enter<R>(&mut self, f: impl FnOnce(StorageHandle) -> R) -> R {
        StorageHandle::enter(self, f)
    }
}

impl PrecompileStorageProvider for HashMapStorageProvider {
    fn chain_id(&self) -> u64 {
        self.chain_id
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
        Ok(self
            .storage
            .get(&(address, key))
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
        self.storage.insert((address, key), value);
        Ok(())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.transient.insert((address, key), value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.events.entry(address).or_default().push(event);
        Ok(())
    }

    fn deduct_gas(&mut self, _gas: u64) -> Result<()> {
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
        self.is_static
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        let idx = self.snapshots.len();
        self.snapshots.push(Snapshot {
            storage: self.storage.clone(),
            accounts: self.accounts.clone(),
            events: self.events.clone(),
        });
        JournalCheckpoint {
            log_i: 0,
            journal_i: idx,
            selfdestructed_i: 0,
        }
    }

    fn checkpoint_commit(&mut self) {
        self.snapshots.pop();
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        if let Some(snapshot) = self.snapshots.drain(checkpoint.journal_i..).next() {
            self.storage = snapshot.storage;
            self.accounts = snapshot.accounts;
            self.events = snapshot.events;
        }
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }

        let from_balance = self.accounts.entry(from).or_default().balance;
        if from_balance < amount {
            return Err(crate::error::PrecompileError::Fatal(format!(
                "insufficient balance: {from} has {from_balance} but needs {amount}"
            )));
        }

        self.accounts.entry(from).or_default().balance -= amount;
        self.accounts.entry(to).or_default().balance += amount;
        Ok(())
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        self.accounts.entry(address).or_default().balance += amount;
        Ok(())
    }

    fn sub_call(
        &mut self,
        input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        self.sub_call_log
            .push((input.target, input.calldata.clone()));
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
            return Err(PrecompileError::Fatal(format!(
                "insufficient balance for burn: {address} has {} but needs {amount}",
                entry.balance
            )));
        }
        entry.balance -= amount;
        Ok(())
    }
}
