//! Real precompile frames with stateful custody/vault counterparties.
//! Solidity tests exercise token transfers and custody signatures; these tests pin
//! lifecycle routing and rollback across the Rust/EVM boundary.
use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_compressed_entities::ExecutionScope;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_evm::sub_call;
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    block::BlockContext,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallOutput, SubCallStatus,
    },
};
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};
use std::sync::Arc;

sol! {
    interface Fixture {
        function topUpFor(address account, address token, uint256 amount) external;
        function withdraw(uint256 amount, address receiver, address owner) external returns (uint256);
        function counter() external view returns (uint256);
    }
}
const CALLER: Address = Address::new([0x11; 20]);
const ACCOUNT: Address = Address::new([0x22; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x44; 20]);
const CUSTODY: Address = Address::new([0x55; 20]);
type Ctx = revm::Context<
    revm::context::BlockEnv,
    revm::context::TxEnv,
    revm::context::CfgEnv,
    CacheDB<EmptyDB>,
>;

fn code_account(code: Vec<u8>) -> AccountInfo {
    let code = Bytecode::new_raw(code.into());
    AccountInfo {
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    }
}

/// All views return `word`. The mutation selector writes slot zero before
/// returning/reverting; counter() exposes that slot for rollback assertions.
fn fixture(mutation: [u8; 4], word: u8, revert: bool) -> Vec<u8> {
    let mut code = vec![0x5f, 0x35, 0x60, 0xe0, 0x1c, 0x63];
    code.extend(mutation);
    code.extend([0x14, 0x60, 0, 0x57]);
    let mutation_jump = 12;
    code.extend([0x5f, 0x35, 0x60, 0xe0, 0x1c, 0x63]);
    code.extend(Fixture::counterCall::SELECTOR);
    code.extend([0x14, 0x60, 0, 0x57]);
    let counter_jump = 26;
    code.extend([0x60, word, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]);
    code[mutation_jump] = u8::try_from(code.len()).unwrap();
    code.extend([0x5b, 0x60, 0x01, 0x5f, 0x55]);
    if revert {
        code.extend([0x5f, 0x5f, 0xfd]);
    } else {
        code.extend([0x60, word, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]);
    }
    code[counter_jump] = u8::try_from(code.len()).unwrap();
    code.extend([0x5b, 0x5f, 0x54, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]);
    code
}
fn context(status: u8, fail_topup: bool) -> Ctx {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(ACCOUNT, code_account(vec![0x00])); // Permissive receiver cannot spoof custody.
    db.insert_account_info(
        CUSTODY,
        code_account(fixture(Fixture::topUpForCall::SELECTOR, status, fail_topup)),
    );
    db.insert_account_info(
        VAULT,
        code_account(fixture(Fixture::withdrawCall::SELECTOR, 1, false)),
    );
    db.insert_account_info(
        ASSET,
        code_account(vec![0x60, 1, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3]),
    );
    let block = BlockContext::new(
        1,
        1,
        outbe_primitives::chain::CHAIN_ID,
        CALLER,
        vec![CALLER],
    );
    let mut provider = DirectStorageProvider::new(&mut db, block);
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage.clone());
        router.bundle_custody.write(CUSTODY).unwrap();
        router.asset_vault_set(ASSET).insert(VAULT).unwrap();
        router.liquidity_targets.insert(CALLER).unwrap();
        router.liquidity_target_types.write(&CALLER, 1).unwrap();
    });
    provider.flush().unwrap();
    drop(provider);
    Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.chain_id = outbe_primitives::chain::CHAIN_ID)
}
fn run(ctx: &mut Ctx, target: Address, data: Vec<u8>) -> SubCallOutput {
    sub_call::run(
        ctx,
        CALLER,
        false,
        SpecId::PRAGUE,
        None,
        Arc::new(ExecutionScope::new()),
        SubCallInput {
            target,
            value: U256::ZERO,
            calldata: data.into(),
            gas_limit: 5_000_000,
            is_static: false,
        },
    )
    .unwrap()
}
fn counter(ctx: &mut Ctx, target: Address) -> U256 {
    let out = run(ctx, target, Fixture::counterCall {}.abi_encode());
    assert!(matches!(out.status, SubCallStatus::Success));
    Fixture::counterCall::abi_decode_returns(&out.returndata).unwrap()
}
#[test]
fn custody_topup_failure_rolls_back_vault_withdrawal() {
    for fail in [false, true] {
        let mut ctx = context(1, fail);
        let out = run(
            &mut ctx,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::withdrawCall {
                asset: ASSET,
                amount: U256::from(1),
                receiver: ACCOUNT,
            }
            .abi_encode(),
        );
        assert_eq!(
            matches!(out.status, SubCallStatus::Success),
            !fail,
            "{:?}",
            out.status
        );
        assert_eq!(counter(&mut ctx, VAULT), U256::from(u8::from(!fail)));
        assert_eq!(counter(&mut ctx, CUSTODY), U256::from(u8::from(!fail)));
    }
}
#[test]
fn unopened_and_closed_bundles_reject_real_request_credis_before_enclave_access() {
    for status in [0, 2] {
        let mut ctx = context(status, false);
        let out = run(
            &mut ctx,
            CREDIS_FACTORY_ADDRESS,
            ICredisFactory::requestCredisCall {
                smartAccount: ACCOUNT,
                pledgeHandle: B256::repeat_byte(1),
                spendAuth: B256::repeat_byte(2),
                referenceCurrency: 840,
            }
            .abi_encode(),
        );
        assert!(!matches!(out.status, SubCallStatus::Success));
        assert!(
            out.returndata
                .windows(b"bundle is not open".len())
                .any(|w| w == b"bundle is not open"),
            "{:?}",
            out
        );
        assert_eq!(counter(&mut ctx, VAULT), U256::ZERO);
        assert_eq!(counter(&mut ctx, CUSTODY), U256::ZERO);
        let withdrawal = run(
            &mut ctx,
            VAULT_ROUTER_ADDRESS,
            IVaultRouter::withdrawCall {
                asset: ASSET,
                amount: U256::from(1),
                receiver: ACCOUNT,
            }
            .abi_encode(),
        );
        assert!(!matches!(withdrawal.status, SubCallStatus::Success));
    }
}
