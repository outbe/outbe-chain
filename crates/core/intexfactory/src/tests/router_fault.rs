use alloy_primitives::{Address, LogData, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{
    AccountInfo, Bytecode, PrecompileStorageProvider, SubCallError, SubCallInput, SubCallOutput,
};
use revm::context::journaled_state::JournalCheckpoint;

use crate::constants::ORIGIN_ROUTER_ADDRESS;

/// The inner provider, except that while armed one OriginRouter call fails on this node's
/// own database, which no injected storage write can reach.
pub(super) struct RouterFaultProvider {
    inner: HashMapStorageProvider,
    failing: Option<[u8; 4]>,
}

impl RouterFaultProvider {
    pub(super) fn new(inner: HashMapStorageProvider, selector: [u8; 4]) -> Self {
        Self {
            inner,
            failing: Some(selector),
        }
    }

    pub(super) fn heal(&mut self) {
        self.failing = None;
    }
}

impl PrecompileStorageProvider for RouterFaultProvider {
    fn chain_id(&self) -> u64 {
        self.inner.chain_id()
    }

    fn genesis_hash(&self) -> B256 {
        self.inner.genesis_hash()
    }

    fn timestamp(&self) -> U256 {
        self.inner.timestamp()
    }

    fn set_block_timestamp(&mut self, timestamp: U256) {
        self.inner.set_block_timestamp(timestamp);
    }

    fn beneficiary(&self) -> Address {
        self.inner.beneficiary()
    }

    fn block_number(&self) -> u64 {
        self.inner.block_number()
    }

    fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
        self.inner.canonical_block_hash(number)
    }

    fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
        self.inner.set_code(address, code)
    }

    fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
        self.inner.account_info(address)
    }

    fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.inner.sload(address, key)
    }

    fn enclave_upgrade_id(&mut self) -> Result<U256> {
        self.inner.enclave_upgrade_id()
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
        self.inner.tload(address, key)
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.inner.sstore(address, key, value)
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
        self.inner.tstore(address, key, value)
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
        self.inner.emit_event(address, event)
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<()> {
        self.inner.deduct_gas(gas)
    }

    fn refund_gas(&mut self, gas: i64) {
        self.inner.refund_gas(gas);
    }

    fn gas_used(&self) -> u64 {
        self.inner.gas_used()
    }

    fn gas_refunded(&self) -> i64 {
        self.inner.gas_refunded()
    }

    fn is_static(&self) -> bool {
        self.inner.is_static()
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.inner.checkpoint()
    }

    fn checkpoint_commit(&mut self) {
        self.inner.checkpoint_commit();
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        self.inner.checkpoint_revert(checkpoint);
    }

    fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
        self.inner.transfer_balance(from, to, amount)
    }

    fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        self.inner.increase_balance(address, amount)
    }

    fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
        self.inner.decrease_balance(address, amount)
    }

    fn sub_call(
        &mut self,
        input: SubCallInput,
    ) -> std::result::Result<SubCallOutput, SubCallError> {
        if input.target == ORIGIN_ROUTER_ADDRESS
            && self
                .failing
                .is_some_and(|selector| input.calldata.starts_with(&selector))
        {
            return Err(SubCallError::DatabaseError(
                "injected router read failure".into(),
            ));
        }
        self.inner.sub_call(input)
    }
}
