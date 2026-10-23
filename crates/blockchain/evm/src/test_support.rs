//! In-memory EVM that drives calls through [`sub_call::run`], for integration tests.

use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{Address, Bytes, Log, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallError, SubCallInput, SubCallOutput,
        SubCallStatus,
    },
};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

use crate::sub_call::{self, SubCallEnvironment};

pub type EvmCtx = EthEvmContext<CacheDB<EmptyDB>>;

const CALL_GAS_LIMIT: u64 = 5_000_000;

/// Isolated child-call environment without runtime body readers.
pub fn fresh_environment(self_address: Address, spec: SpecId) -> SubCallEnvironment {
    SubCallEnvironment {
        self_address,
        outer_is_static: false,
        spec,
    }
}

/// Runtime bytecode from a hex fixture.
pub fn runtime_code(hex: &str) -> Bytecode {
    Bytecode::new_raw(Bytes::from(
        alloy_primitives::hex::decode(hex.trim()).expect("hex bytecode fixture"),
    ))
}

pub fn install_call_scan_markers<DB: revm::Database>(db: &mut CacheDB<DB>, code: &Bytecode) {
    for address in [
        outbe_primitives::addresses::NOD_ADDRESS,
        outbe_primitives::addresses::GEM_ADDRESS,
        outbe_primitives::addresses::GEM_FACTORY_ADDRESS,
        outbe_primitives::addresses::INTEX_ADDRESS,
        outbe_primitives::addresses::CREDIS_ADDRESS,
    ] {
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code.clone()),
                ..Default::default()
            },
        );
    }
}

pub fn install_code(db: &mut CacheDB<EmptyDB>, address: Address, code: &Bytecode) {
    db.insert_account_info(
        address,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code.clone()),
            ..Default::default()
        },
    );
}

/// Runs `seed` against `db` as block 1 at `timestamp` and flushes its writes.
pub fn seed_storage<R>(
    db: &mut CacheDB<EmptyDB>,
    timestamp: u64,
    beneficiary: Address,
    seed: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    let block = BlockContext::new(1, timestamp, CHAIN_ID, beneficiary, vec![beneficiary]);
    let mut provider = DirectStorageProvider::new(db, block);
    let seeded = StorageHandle::enter(&mut provider, seed);
    provider.flush().expect("flush seeded storage");
    seeded
}

pub struct EvmWorld {
    pub ctx: EvmCtx,
}

impl EvmWorld {
    pub fn new(db: CacheDB<EmptyDB>) -> Self {
        Self {
            ctx: Context::mainnet()
                .with_db(db)
                .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID),
        }
    }

    pub fn at(mut self, timestamp: u64) -> Self {
        self.ctx.block.timestamp = U256::from(timestamp);
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
        sub_call::run(&mut self.ctx, environment, input)
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
        self.ctx.journaled_state.logs()
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
