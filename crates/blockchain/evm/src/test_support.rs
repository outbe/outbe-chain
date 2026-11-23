//! In-memory EVM that drives calls through [`sub_call::run_with_host`], for integration tests.

use alloy_primitives::{Address, Bytes, Log, U256};
use alloy_sol_types::SolCall;
use evm2::{
    bytecode::Bytecode,
    evm::{AccountInfo, CacheDB, DynDatabase, EmptyDB},
    Evm, SpecId,
};
use outbe_primitives::{
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallError, SubCallInput, SubCallOutput,
        SubCallStatus,
    },
};
use reth_ethereum::evm::EthEvmEnv;

use crate::{
    factory::{OutbeEvmFactory, OutbeEvmTypes},
    sub_call::{self, SubCallEnvironment},
};

pub type EvmCtx = Evm<'static, OutbeEvmTypes>;

const CALL_GAS_LIMIT: u64 = 5_000_000;

/// Isolated child-call environment without runtime body readers.
pub fn fresh_environment(self_address: Address, spec: SpecId) -> SubCallEnvironment {
    SubCallEnvironment {
        self_address,
        outer_is_static: false,
        spec,
    }
}

/// Sets the simulated enclosing call depth on an [`EvmCtx`] for depth-boundary tests.
pub fn set_call_depth(evm: &mut EvmCtx, depth: u16) {
    evm.ext_mut().current_call_depth = depth;
}

/// Returns the gas limit selected for the most recent Outbe system call on `evm`.
#[cfg(any(test, feature = "test-utils"))]
pub fn last_system_call_gas_limit(evm: &EvmCtx) -> Option<u64> {
    evm.ext().last_system_call.as_ref().map(|tx| tx.gas_limit)
}

/// Runtime bytecode from a hex fixture.
pub fn runtime_code(hex: &str) -> Bytecode {
    Bytecode::new_legacy(Bytes::from(
        alloy_primitives::hex::decode(hex.trim()).expect("hex bytecode fixture"),
    ))
}

pub fn install_call_scan_markers<DB: DynDatabase>(
    db: &mut CacheDB<DB>,
    addresses: &[Address],
    code: &Bytecode,
) {
    for &address in addresses {
        db.insert_account_info(&address, AccountInfo::default().with_code(code.clone()));
    }
}

pub fn install_code(db: &mut CacheDB<EmptyDB>, address: Address, code: &Bytecode) {
    db.insert_account_info(&address, AccountInfo::default().with_code(code.clone()));
}

/// Runs `seed` against `db` as block 1 at `timestamp` and flushes its writes.
pub fn seed_storage<R>(
    db: &mut CacheDB<EmptyDB>,
    timestamp: u64,
    beneficiary: Address,
    seed: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    let block = BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 1,
        timestamp,
        chain_id: CHAIN_ID,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: timestamp.max(1),
        proposer: beneficiary,
        validators: vec![beneficiary],
    };
    let mut provider = DirectStorageProvider::new(db, block);
    let seeded = StorageHandle::enter(&mut provider, seed);
    provider.flush().expect("flush seeded storage");
    seeded
}

pub struct EvmWorld {
    pub evm: EvmCtx,
    host: std::sync::Arc<dyn crate::EvmRuntimeHost>,
}

impl EvmWorld {
    pub fn new(db: CacheDB<EmptyDB>, host: std::sync::Arc<dyn crate::EvmRuntimeHost>) -> Self {
        let factory = OutbeEvmFactory::new(std::sync::Arc::clone(&host));
        let env = EthEvmEnv::new(SpecId::PRAGUE, Default::default(), CHAIN_ID);
        Self {
            evm: factory.create_evm(db, env),
            host,
        }
    }

    pub fn at(mut self, timestamp: u64) -> Self {
        let mut block = *self.evm.block();
        block.timestamp = U256::from(timestamp);
        self.evm.set_block(block);
        self
    }

    pub fn run(
        &mut self,
        caller: Address,
        input: SubCallInput,
    ) -> Result<SubCallOutput, SubCallError> {
        let environment = SubCallEnvironment {
            self_address: caller,
            outer_is_static: false,
            spec: SpecId::PRAGUE,
        };
        sub_call::run_with_host(
            &mut self.evm,
            environment,
            input,
            std::sync::Arc::clone(&self.host),
        )
    }

    pub fn call(
        &mut self,
        caller: Address,
        target: Address,
        calldata: Bytes,
        is_static: bool,
    ) -> SubCallOutput {
        self.run(
            caller,
            SubCallInput {
                target,
                value: U256::ZERO,
                calldata,
                gas_limit: CALL_GAS_LIMIT,
                is_static,
            },
        )
        .expect("sub-call must not fail fatally")
    }

    /// A successful call, decoded.
    pub fn ok<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call.abi_encode().into(), false);
        decode_success::<C>(&out)
    }

    /// A successful static call, decoded.
    pub fn view<C: SolCall>(&mut self, caller: Address, target: Address, call: C) -> C::Return {
        let out = self.call(caller, target, call.abi_encode().into(), true);
        decode_success::<C>(&out)
    }

    pub fn logs(&self) -> &[Log] {
        self.evm.logs()
    }
}

fn decode_success<C: SolCall>(out: &SubCallOutput) -> C::Return {
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}",
        out.status
    );
    C::abi_decode_returns(&out.returndata).expect("decode call return")
}

#[cfg(any(test, feature = "test-utils"))]
pub use crate::system_tx::TEST_BLOCK_ORDER;
