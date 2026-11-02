//! `upgradeContract` on Outbe in a real EVM.
//!
//! An approved `upgradeContract` runs through `applyApproved` on the
//! CrosschainGov precompile. The precompile calls
//! `proxy.upgradeToAndCall(implementation, callData)` from its own frame, so
//! the `onlyOwner` check of the proxy passes. The ERC-1967 implementation
//! slot of the proxy changes and the proxy state stays.
//!
//! The bytecode is `testing/fixtures/evm/UupsUpgrade.sol`.

use alloy_evm::eth::EthEvmContext;
use alloy_primitives::{keccak256, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_crosschaingov::precompile::ICrosschainGov;
use outbe_crosschaingov::CrosschainGov;
use outbe_evm::sub_call;
use outbe_evm::test_support::{fresh_environment, runtime_code};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::block::BlockContext;
use outbe_primitives::storage::{
    direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallOutput, SubCallStatus,
};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

sol! {
    interface IUupsCounter {
        function owner() external view returns (address);
        function count() external view returns (uint256);
        function implementation() external view returns (address);
        function upgradeToAndCall(address newImplementation, bytes calldata data) external payable;
    }
}

/// `Context::mainnet()` runs on chain id 1, so Outbe is chain id 1 here.
const OUTBE_CHAIN_ID: u64 = 1;
const PROXY: Address = Address::new([0x70; 20]);
const IMPLEMENTATION_V1: Address = Address::new([0x71; 20]);
const IMPLEMENTATION_V2: Address = Address::new([0x72; 20]);
const STRANGER: Address = Address::new([0xC0; 20]);
const PROPOSAL_ID: U256 = U256::from_limbs([7, 0, 0, 0]);
const IMPLEMENTATION_SLOT: U256 = U256::from_be_bytes(alloy_primitives::hex!(
    "360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc"
));
const COUNT: u64 = 41;

fn install(db: &mut CacheDB<EmptyDB>, address: Address, code: &Bytecode) {
    db.insert_account_info(
        address,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code.clone()),
            ..Default::default()
        },
    );
}

fn upgrade_payload() -> String {
    format!(
        r#"{{"operation":"upgradeContract","chainId":{OUTBE_CHAIN_ID},"proxy":"{PROXY:#x}","implementation":"{IMPLEMENTATION_V2:#x}","callData":"0x"}}"#
    )
}

/// A proxy on `IMPLEMENTATION_V1`, owned by the precompile, with a count,
/// and an approved `upgradeContract` to `IMPLEMENTATION_V2`.
fn database() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    install(
        &mut db,
        PROXY,
        &runtime_code(include_str!(
            "../../../../testing/fixtures/evm/UupsProxy.hex"
        )),
    );
    let counter = runtime_code(include_str!(
        "../../../../testing/fixtures/evm/UupsCounter.hex"
    ));
    install(&mut db, IMPLEMENTATION_V1, &counter);
    install(&mut db, IMPLEMENTATION_V2, &counter);
    let slots = [
        (IMPLEMENTATION_SLOT, IMPLEMENTATION_V1.into_word().into()),
        (U256::ZERO, CROSSCHAIN_GOV_ADDRESS.into_word().into()),
        (U256::ONE, U256::from(COUNT)),
    ];
    for (slot, value) in slots {
        db.insert_account_storage(PROXY, slot, value).unwrap();
    }

    let block = BlockContext::new(0, 1, OUTBE_CHAIN_ID, Address::ZERO, vec![]);
    let mut provider = DirectStorageProvider::new(&mut db, block);
    StorageHandle::enter(&mut provider, |storage| {
        CrosschainGov::new(storage)
            .approved_payload
            .write(&PROPOSAL_ID, keccak256(upgrade_payload()))
            .unwrap();
    });
    provider.flush().unwrap();
    db
}

fn call<C>(
    ctx: &mut EthEvmContext<CacheDB<EmptyDB>>,
    (caller, target): (Address, Address),
    call: C,
) -> SubCallOutput
where
    C: SolCall,
{
    sub_call::run(
        ctx,
        fresh_environment(caller, SpecId::PRAGUE),
        SubCallInput {
            target,
            value: U256::ZERO,
            calldata: Bytes::from(call.abi_encode()),
            gas_limit: 5_000_000,
            is_static: false,
        },
    )
    .expect("sub-call executes")
}

fn read<C: SolCall>(ctx: &mut EthEvmContext<CacheDB<EmptyDB>>, view: C) -> C::Return {
    let result = call(ctx, (STRANGER, PROXY), view);
    assert!(matches!(result.status, SubCallStatus::Success));
    C::abi_decode_returns(&result.returndata).unwrap()
}

#[test]
fn approved_upgrade_changes_the_implementation_slot_and_keeps_the_state() {
    let mut ctx = Context::mainnet().with_db(database());
    assert_eq!(
        read(&mut ctx, IUupsCounter::implementationCall {}),
        IMPLEMENTATION_V1
    );

    let apply = ICrosschainGov::applyApprovedCall {
        proposalId: PROPOSAL_ID,
        payload: upgrade_payload().into_bytes().into(),
    };
    let result = call(&mut ctx, (STRANGER, CROSSCHAIN_GOV_ADDRESS), apply);
    assert!(
        matches!(result.status, SubCallStatus::Success),
        "applyApproved: {:?} 0x{}",
        result.status,
        alloy_primitives::hex::encode(&result.returndata)
    );

    assert_eq!(
        read(&mut ctx, IUupsCounter::implementationCall {}),
        IMPLEMENTATION_V2
    );
    assert_eq!(
        read(&mut ctx, IUupsCounter::countCall {}),
        U256::from(COUNT)
    );
    assert_eq!(
        read(&mut ctx, IUupsCounter::ownerCall {}),
        CROSSCHAIN_GOV_ADDRESS
    );
}

#[test]
fn an_upgrade_from_another_sender_fails_the_owner_check() {
    let mut ctx = Context::mainnet().with_db(database());
    let upgrade = IUupsCounter::upgradeToAndCallCall {
        newImplementation: IMPLEMENTATION_V2,
        data: Bytes::new(),
    };
    let result = call(&mut ctx, (STRANGER, PROXY), upgrade);
    assert!(!matches!(result.status, SubCallStatus::Success));
    assert_eq!(
        read(&mut ctx, IUupsCounter::implementationCall {}),
        IMPLEMENTATION_V1
    );
}
