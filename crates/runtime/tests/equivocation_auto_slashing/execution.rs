use super::*;
use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use evm2::{
    block::BundleSource,
    bytecode::Bytecode,
    env::BlockEnvExt,
    evm::{AccountInfo, CacheDB, Database as _, EmptyDB},
    SpecId,
};
use outbe_evm::config::OutbeEvmConfig;
use outbe_primitives::{
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle},
};
use outbe_runtime::OutbeRuntime;
use outbe_slashindicator::schema::SlashIndicator;
use outbe_system_common::addresses::SLASH_INDICATOR_ADDRESS;
use outbe_validatorset::ValidatorLifecycle;
use reth_ethereum::evm::EthEvmEnv;
use reth_evm::{execute::BlockExecutor as _, BlockExecutorFactory, ConfigureEvm as _};
use reth_primitives_traits::SignedTransaction as _;

pub fn execute(
    seeded: &HashMapStorageProvider,
    transaction: outbe_primitives::OutbeTxEnvelope,
    sender: Address,
) {
    let mut state = execute_envelope(seeded, transaction, sender, AccountInfo::default());
    let account = state.get_account(&sender).unwrap().unwrap();
    assert_eq!(account.balance, U256::ZERO);
    assert_eq!(account.nonce, 1);
    let context = BlockContext {
        block_number: 1,
        timestamp: 1,
        chain_id: 1,
        proposer: OWNER,
        block_gas_limit: 30_000_000,
        user_gas_used: 0,
        genesis_hash: B256::ZERO,
        genesis_timestamp: 0,
        validators: vec![],
    };
    let mut direct = DirectStorageProvider::new(&mut state, context);
    StorageHandle::enter(&mut direct, |storage| {
        assert_eq!(
            SlashIndicator::new(storage.clone())
                .get_felony_count(ACCUSED)
                .unwrap(),
            1
        );
        assert!(matches!(
            ValidatorSet::new(storage.clone())
                .validator_lifecycle(ACCUSED)
                .unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert_eq!(
            Staking::new(storage).stake_amount.read(&ACCUSED).unwrap(),
            U256::from(STAKE * 92 / 100)
        );
    });
}

pub type TestState = CacheDB<EmptyDB>;

pub fn execute_envelope(
    seeded: &HashMapStorageProvider,
    transaction: outbe_primitives::OutbeTxEnvelope,
    sender: Address,
    account: AccountInfo,
) -> TestState {
    let mut db = CacheDB::new(EmptyDB::default());
    // Seeded precompiles must survive EIP-161 touched-account cleanup.
    let marker = Bytecode::new_legacy([0xef].into());
    let system_account = AccountInfo {
        code_hash: marker.hash_slow(),
        code: Some(marker),
        ..Default::default()
    };
    for address in seeded.storage.keys().map(|(address, _)| *address) {
        db.insert_account_info(&address, system_account.clone());
    }
    for (&(address, slot), &value) in &seeded.storage {
        db.insert_account_storage(&address, &slot, &value);
    }
    db.insert_account_info(
        &STAKING_ADDRESS,
        AccountInfo {
            balance: U256::from(2 * STAKE),
            ..system_account.clone()
        },
    );
    db.insert_account_info(&SLASH_INDICATOR_ADDRESS, system_account);
    db.insert_account_info(&sender, account);
    let chain = Arc::new(
        reth_chainspec::ChainSpecBuilder::mainnet()
            .build()
            .map_header(outbe_primitives::OutbeHeader::new),
    );
    let config = OutbeEvmConfig::new(chain, Arc::new(OutbeRuntime::new()));
    let env = EthEvmEnv::new(
        SpecId::SHANGHAI,
        BlockEnvExt {
            number: U256::from(1),
            timestamp: U256::from(1),
            gas_limit: U256::from(30_000_000_u64),
            basefee: U256::from(MIN_PROTOCOL_BASE_FEE),
            beneficiary: OWNER,
            ..Default::default()
        },
        1,
    );
    let block =
        reth_primitives_traits::SealedBlock::seal_slow(outbe_primitives::OutbeBlock::default());
    let mut ctx = config.context_for_block(&block).unwrap();
    // The immutable cluster suite covers block hooks; this component executes the user envelope.
    ctx.execute_outbe_block_hooks = false;
    ctx.system_layout_error = None;
    ctx.block_state_root = None;
    let output = {
        let evm = BlockExecutorFactory::evm_with_env(&config, &mut db, env);
        let mut executor = config.create_executor(evm, ctx);
        executor
            .execute_transaction(transaction.try_into_recovered().unwrap())
            .unwrap();
        assert!(
            executor.receipts()[0].success,
            "{:?}",
            executor.receipts()[0]
        );
        executor.into_state()
    };
    db.commit_source(&BundleSource(&output));
    db
}
