use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_evm::OutbeEvmFactory;
use outbe_primitives::{
    addresses::{STABLECOIN_POLICY_REGISTRY_ADDRESS, UPDATE_ADDRESS},
    block::BlockContext,
    stablecoin_fork::STABLECOIN_V1_PROTOCOL_VERSION_RAW,
    storage::{direct::DirectStorageProvider, StorageHandle},
};
use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
use outbe_update::{ProtocolVersion, Update};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{
        result::{ExecutionResult, Output},
        BlockEnv, CfgEnv, TxEnv,
    },
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::AccountInfo,
};

const CHAIN_ID: u64 = 1;
const CALLER: Address = Address::new([0xc5; 20]);
const ACTIVATION_HEIGHT: u64 = 30_100;
const GAS_LIMIT: u64 = 100_000;

fn state_with_version(version: ProtocolVersion) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(10u64).pow(U256::from(20u64)),
            ..Default::default()
        },
    );
    if !version.is_zero() {
        db.insert_account_storage(UPDATE_ADDRESS, U256::ZERO, U256::from(version.raw()))
            .unwrap();
    }
    db
}

fn env(block_number: u64, spec: SpecId) -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(spec),
        block_env: BlockEnv {
            number: U256::from(block_number),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    }
}

fn policy_exists(db: CacheDB<EmptyDB>, block_number: u64, spec: SpecId) -> ExecutionResult {
    let data = IStablecoinPolicyRegistry::policyExistsCall {
        policyId: U256::from(1u64),
    }
    .abi_encode();
    let tx = TxEnv::builder()
        .caller(CALLER)
        .nonce(0)
        .kind(TxKind::Call(STABLECOIN_POLICY_REGISTRY_ADDRESS))
        .data(data.into())
        .gas_limit(GAS_LIMIT)
        .build()
        .unwrap();

    OutbeEvmFactory::new()
        .create_evm(db, env(block_number, spec))
        .transact_raw(tx)
        .unwrap()
        .result
}

fn assert_policy_active(result: ExecutionResult) {
    let ExecutionResult::Success {
        output: Output::Call(output),
        ..
    } = result
    else {
        panic!("policy route should be active");
    };
    assert!(IStablecoinPolicyRegistry::policyExistsCall::abi_decode_returns(&output).unwrap());
}

#[test]
fn h_minus_one_h_and_h_plus_one_use_the_selected_state_snapshot() {
    let active = ProtocolVersion::from_raw(STABLECOIN_V1_PROTOCOL_VERSION_RAW);

    let before = policy_exists(
        state_with_version(ProtocolVersion::ZERO),
        ACTIVATION_HEIGHT - 1,
        SpecId::PRAGUE,
    );
    let at = policy_exists(
        state_with_version(active),
        ACTIVATION_HEIGHT,
        SpecId::PRAGUE,
    );
    let after = policy_exists(
        state_with_version(active),
        ACTIVATION_HEIGHT + 1,
        SpecId::PRAGUE,
    );

    assert!(matches!(before, ExecutionResult::Revert { .. }));
    assert_policy_active(at);
    assert_policy_active(after);
}

#[test]
fn ethereum_spec_id_is_not_the_stablecoin_activation_authority() {
    let active = ProtocolVersion::from_raw(STABLECOIN_V1_PROTOCOL_VERSION_RAW);

    assert_policy_active(policy_exists(
        state_with_version(active),
        ACTIVATION_HEIGHT,
        SpecId::LONDON,
    ));
    assert_policy_active(policy_exists(
        state_with_version(active),
        ACTIVATION_HEIGHT,
        SpecId::PRAGUE,
    ));

    assert!(matches!(
        policy_exists(
            state_with_version(ProtocolVersion::ZERO),
            ACTIVATION_HEIGHT,
            SpecId::PRAGUE,
        ),
        ExecutionResult::Revert { .. }
    ));
}

#[test]
fn same_block_update_write_is_visible_to_the_following_user_call() {
    let active = ProtocolVersion::from_raw(STABLECOIN_V1_PROTOCOL_VERSION_RAW);
    let mut db = state_with_version(ProtocolVersion::ZERO);

    {
        let mut provider = DirectStorageProvider::new(
            &mut db,
            BlockContext::empty_for_tests(ACTIVATION_HEIGHT, 0, CHAIN_ID),
        );
        StorageHandle::enter(&mut provider, |storage| {
            Update::new(storage)
                .set_active_version(active, ACTIVATION_HEIGHT)
                .unwrap();
        });
        provider.flush().unwrap();
    }

    assert_policy_active(policy_exists(db, ACTIVATION_HEIGHT, SpecId::PRAGUE));
}
