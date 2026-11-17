//! SELFDESTRUCT sends the COEN that would strand on a system address, or burn in a contract of
//! the same transaction, to address 0 (OIP-00374).
use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use outbe_evm::OutbeEvmFactory;
use outbe_system_common::addresses::{STAKING_ADDRESS, VOTE_ADDRESS, ZKPROOF_VERIFY_ADDRESS};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{result::ExecutionResult, BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    DatabaseCommit,
};

const CALLER: Address = Address::repeat_byte(0xc1);
const OLD: Address = Address::repeat_byte(0xd1);
const WRAPPER: Address = Address::repeat_byte(0xd2);
const FIVE_COEN: u128 = 5_000_000_000_000_000_000;
const MARKER: [u8; 1] = [0xef];

fn put(db: &mut CacheDB<EmptyDB>, address: Address, balance: U256, code: &[u8]) {
    let code = Bytecode::new_legacy(Bytes::copy_from_slice(code));
    db.insert_account_info(
        address,
        AccountInfo {
            balance,
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
}

fn world() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(FIVE_COEN),
            ..Default::default()
        },
    );
    for system in [STAKING_ADDRESS, VOTE_ADDRESS, ZKPROOF_VERIFY_ADDRESS] {
        put(&mut db, system, U256::ZERO, &MARKER);
    }
    db
}

/// `PUSH20 target SELFDESTRUCT`
fn selfdestruct_to(target: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(target.as_slice());
    code.push(0xff);
    code
}

/// `ADDRESS SELFDESTRUCT`
const SELFDESTRUCT_TO_SELF: [u8; 2] = [0x30, 0xff];

fn transact(
    db: &mut CacheDB<EmptyDB>,
    kind: TxKind,
    value: U256,
    data: Vec<u8>,
) -> ExecutionResult {
    let nonce = db.cache.accounts[&CALLER].info.nonce;
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            gas_limit: 30_000_000,
            ..Default::default()
        },
    };
    let outcome = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(&mut *db, env)
        .transact_raw(
            TxEnv::builder()
                .caller(CALLER)
                .nonce(nonce)
                .kind(kind)
                .value(value)
                .data(Bytes::from(data))
                .gas_limit(3_000_000)
                .build()
                .unwrap(),
        )
        .unwrap();
    db.commit(outcome.state);
    outcome.result
}

/// The same transaction on the upstream revm EVM, which has no Outbe instructions.
fn upstream_gas(db: &CacheDB<EmptyDB>, to: Address) -> u64 {
    use revm::{context::ContextSetters, ExecuteEvm, MainBuilder, MainContext};
    let mut evm = revm::Context::mainnet()
        .with_db(db.clone())
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = 1;
            cfg.set_spec_and_mainnet_gas_params(SpecId::PRAGUE);
        })
        .modify_block_chained(|block| block.gas_limit = 30_000_000)
        .build_mainnet();
    let nonce = db.cache.accounts[&CALLER].info.nonce;
    evm.set_tx(
        TxEnv::builder()
            .caller(CALLER)
            .nonce(nonce)
            .kind(TxKind::Call(to))
            .gas_limit(3_000_000)
            .chain_id(Some(1))
            .build()
            .unwrap(),
    );
    evm.replay().unwrap().result.tx_gas_used()
}

fn balance(db: &CacheDB<EmptyDB>, address: Address) -> U256 {
    db.cache
        .accounts
        .get(&address)
        .map_or(U256::ZERO, |account| account.info.balance)
}

#[test]
fn selfdestruct_to_a_system_address_sends_the_balance_to_address_zero() {
    for system in [
        Address::ZERO,
        STAKING_ADDRESS,
        VOTE_ADDRESS,
        ZKPROOF_VERIFY_ADDRESS,
    ] {
        let mut db = world();
        put(
            &mut db,
            OLD,
            U256::from(FIVE_COEN),
            &selfdestruct_to(system),
        );
        let result = transact(&mut db, TxKind::Call(OLD), U256::ZERO, vec![]);
        assert!(result.is_success(), "{system}: {result:?}");
        assert_eq!(
            balance(&db, Address::ZERO),
            U256::from(FIVE_COEN),
            "{system}"
        );
        assert_eq!(balance(&db, OLD), U256::ZERO);
        if system != Address::ZERO {
            assert_eq!(balance(&db, system), U256::ZERO, "{system}");
        }
    }
}

#[test]
fn selfdestruct_to_a_normal_or_stablecoin_address_moves_the_balance_as_in_ethereum() {
    let normal = Address::repeat_byte(0x42);
    let mut stablecoin = [0x11u8; 20];
    stablecoin[..2].copy_from_slice(&outbe_system_common::addresses::STABLECOIN_ADDRESS_PREFIX);
    let stablecoin = Address::from(stablecoin);
    for target in [normal, stablecoin] {
        let mut db = world();
        put(&mut db, stablecoin, U256::ZERO, &MARKER);
        put(
            &mut db,
            OLD,
            U256::from(FIVE_COEN),
            &selfdestruct_to(target),
        );
        let result = transact(&mut db, TxKind::Call(OLD), U256::ZERO, vec![]);
        assert!(result.is_success(), "{target}: {result:?}");
        assert_eq!(balance(&db, target), U256::from(FIVE_COEN), "{target}");
        assert_eq!(balance(&db, Address::ZERO), U256::ZERO, "{target}");
    }
}

#[test]
fn an_older_contract_that_names_itself_keeps_its_balance() {
    let mut db = world();
    put(&mut db, OLD, U256::from(FIVE_COEN), &SELFDESTRUCT_TO_SELF);
    let result = transact(&mut db, TxKind::Call(OLD), U256::ZERO, vec![]);
    assert!(result.is_success(), "{result:?}");
    assert_eq!(balance(&db, OLD), U256::from(FIVE_COEN));
    assert_eq!(balance(&db, Address::ZERO), U256::ZERO);
}

#[test]
fn a_contract_of_the_same_transaction_that_names_itself_sends_its_balance_to_address_zero() {
    let mut db = world();
    let result = transact(
        &mut db,
        TxKind::Create,
        U256::from(FIVE_COEN),
        SELFDESTRUCT_TO_SELF.to_vec(),
    );
    assert!(result.is_success(), "{result:?}");
    assert_eq!(balance(&db, Address::ZERO), U256::from(FIVE_COEN));
}

#[test]
fn a_reverted_selfdestruct_moves_nothing() {
    let mut db = world();
    put(
        &mut db,
        OLD,
        U256::from(FIVE_COEN),
        &selfdestruct_to(STAKING_ADDRESS),
    );
    // CALL(gas, OLD, 0, 0, 0, 0, 0), then REVERT(0, 0).
    let mut wrapper = vec![0x5f, 0x5f, 0x5f, 0x5f, 0x5f, 0x73];
    wrapper.extend_from_slice(OLD.as_slice());
    wrapper.extend_from_slice(&[0x5a, 0xf1, 0x5f, 0x5f, 0xfd]);
    put(&mut db, WRAPPER, U256::ZERO, &wrapper);
    let result = transact(&mut db, TxKind::Call(WRAPPER), U256::ZERO, vec![]);
    assert!(
        matches!(result, ExecutionResult::Revert { .. }),
        "{result:?}"
    );
    assert_eq!(balance(&db, OLD), U256::from(FIVE_COEN));
    assert_eq!(balance(&db, Address::ZERO), U256::ZERO);
    assert_eq!(balance(&db, STAKING_ADDRESS), U256::ZERO);
}

#[test]
fn a_selfdestruct_that_is_not_redirected_costs_the_upstream_gas() {
    let existing = Address::repeat_byte(0x43);
    for code in [
        selfdestruct_to(Address::repeat_byte(0x42)),
        selfdestruct_to(existing),
        SELFDESTRUCT_TO_SELF.to_vec(),
    ] {
        let mut db = world();
        put(&mut db, existing, U256::ONE, &[]);
        put(&mut db, OLD, U256::from(FIVE_COEN), &code);
        let upstream = upstream_gas(&db, OLD);
        let result = transact(&mut db, TxKind::Call(OLD), U256::ZERO, vec![]);
        assert!(result.is_success(), "{result:?}");
        assert_eq!(result.tx_gas_used(), upstream, "{code:?}");
    }
}
