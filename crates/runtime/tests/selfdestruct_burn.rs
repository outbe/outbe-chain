//! SELFDESTRUCT sends the COEN that would strand on a system address, or burn in a contract of
//! the same transaction, to address 0 (OIP-00374).
use alloy_consensus::TxLegacy;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    BaseEvmTypes, Evm, SpecId, TxResult,
};
use outbe_evm::{factory::OutbeEvmTypes, OutbeEvmFactory};
use outbe_system_common::addresses::{STAKING_ADDRESS, VOTE_ADDRESS, ZKPROOF_VERIFY_ADDRESS};
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;

const CALLER: Address = Address::repeat_byte(0xc1);
const OLD: Address = Address::repeat_byte(0xd1);
const WRAPPER: Address = Address::repeat_byte(0xd2);
const FIVE_COEN: u128 = 5_000_000_000_000_000_000;
const MARKER: [u8; 1] = [0xef];

fn put(db: &mut CacheDB<EmptyDB>, address: Address, balance: U256, code: &[u8]) {
    let code = Bytecode::new_legacy(Bytes::copy_from_slice(code));
    db.insert_account_info(
        &address,
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
        &CALLER,
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
) -> TxResult<OutbeEvmTypes> {
    let nonce = db.account_info(&CALLER).map_or(0, |info| info.nonce);
    let env = EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        1,
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce,
            gas_price: 0,
            gas_limit: 3_000_000,
            to: kind,
            value,
            input: Bytes::from(data),
        }),
        CALLER,
    );
    let detached = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(&mut *db, env)
        .transact(&tx)
        .unwrap()
        .detach();
    db.commit_pending(&detached.pending_state);
    detached.result
}

/// The same transaction on the upstream evm2 EVM, which has no Outbe instructions.
fn upstream_gas(db: &CacheDB<EmptyDB>, to: Address) -> u64 {
    let nonce = db.account_info(&CALLER).map_or(0, |info| info.nonce);
    let mut evm = Evm::<BaseEvmTypes>::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        evm2::ethereum::ethereum_tx_registry(SpecId::PRAGUE),
        db.clone(),
        evm2::Precompiles::base(SpecId::PRAGUE),
    );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce,
            gas_price: 0,
            gas_limit: 3_000_000,
            to: TxKind::Call(to),
            value: U256::ZERO,
            input: Bytes::new(),
        }),
        CALLER,
    );
    let gas = evm.transact(&tx).unwrap().result().tx_gas_used();
    gas
}

fn balance(db: &CacheDB<EmptyDB>, address: Address) -> U256 {
    db.account_info(&address)
        .map_or(U256::ZERO, |info| info.balance)
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
    assert!(result.is_revert(), "{result:?}");
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
