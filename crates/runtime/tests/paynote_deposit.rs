//! EVM-level integration test for `IPayNote.deposit`.
//!
//! The paynote crate's own tests cover only `deposit`'s pre-mutation guards,
//! because its body moves real ERC20 balances. An in-memory storage provider
//! cannot model these balances. This test runs the precompile through the real
//! EVM. The stateful `FactorySettlement` fixture is the ERC20 counterparty.
//!
//! What this pins that unit tests cannot:
//!   * the deposited tokens go into the pool at `PAYNOTE_ADDRESS`, not into
//!     the vault. A spend routes them into the vault later.
//!   * the asset must have a registered reserve vault.
//!   * the pool must receive exactly the deposited amount.
//!   * a revert anywhere in that chain rolls the tree back atomically.
//!   * the appended leaf is the runtime-derived commitment, readable through
//!     the public view ABI.
//!   * a native COEN deposit (`asset == 0`) moves `msg.value` into the pool.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};
use evm2::evm::{AccountInfo, CacheDB, EmptyDB};
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_paynote::hash::{note_sn, transfer_commitment, transfer_lock};
use outbe_paynote::precompile::IPayNote;
use outbe_paynote::runtime::PAYNOTE_TRANSFER_TIMELOCK_SECS;
use outbe_paynote::Field;
use outbe_primitives::chain::CHAIN_ID;
use outbe_primitives::storage::{SubCallInput, SubCallOutput, SubCallStatus};
use outbe_system_common::addresses::PAYNOTE_ADDRESS;
use outbe_vaultrouter::precompile::IVaultRouter::StablecoinSource;
use outbe_vaultrouter::test_support::{register_source, register_vault};
use outbe_zk::codec::field_to_b256;

sol! {
    interface IFixture {
        function mint(address account, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function configure(uint256 mode, address factory, bytes reentry) external;
    }
}

const ALICE: Address = Address::new([0x11; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const UNREGISTERED_ASSET: Address = Address::new([0x66; 20]);
const TIMESTAMP: u64 = 1_700_000_000;
const DEPOSIT_AMOUNT: u64 = 1_000;
const DEADLINE: u64 = TIMESTAMP + PAYNOTE_TRANSFER_TIMELOCK_SECS;
const SPEND_KEY: u64 = 17;
const RHO: u64 = 777;
const ALICE_COEN: u64 = 1_000_000;

/// Alice deposits for herself: her serial is both the recipient and the
/// sender of the lock.
fn lock() -> Field {
    let serial = note_sn(Field::from(SPEND_KEY)).unwrap();
    transfer_lock(serial, serial, Field::from(RHO)).unwrap()
}

/// The commitment the runtime must derive, computed independently here.
fn expected_commitment(asset: Address, amount: U256) -> B256 {
    field_to_b256(&transfer_commitment(CHAIN_ID, lock(), DEADLINE, asset, amount).unwrap()).unwrap()
}

/// `ALICE` has both assets and approves the pool for both, and holds native
/// COEN. `ASSET` has a vault. PayNote is a `PayNoteDeposit` source. A live
/// deployment has the same setup.
fn world() -> EvmWorld {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &ALICE,
        AccountInfo {
            balance: U256::from(ALICE_COEN),
            ..Default::default()
        },
    );
    let code = runtime_code(include_str!(
        "../../../testing/fixtures/evm/FactorySettlement.hex"
    ));
    for address in [ASSET, UNREGISTERED_ASSET, VAULT] {
        install_code(&mut db, address, &code);
    }
    seed_storage(&mut db, TIMESTAMP, ALICE, |storage| {
        register_vault(&storage, ASSET, VAULT, 840).unwrap();
        register_source(&storage, PAYNOTE_ADDRESS, StablecoinSource::PayNoteDeposit).unwrap();
    });
    let mut world =
        EvmWorld::new(db, std::sync::Arc::new(outbe_runtime::OutbeRuntime::new())).at(TIMESTAMP);
    for asset in [ASSET, UNREGISTERED_ASSET] {
        world.ok(
            ALICE,
            asset,
            IFixture::mintCall {
                account: ALICE,
                amount: U256::MAX >> 1,
            },
        );
        world.ok(
            ALICE,
            asset,
            IFixture::approveCall {
                spender: PAYNOTE_ADDRESS,
                amount: U256::MAX,
            },
        );
    }
    world
}

fn deposit(world: &mut EvmWorld, asset: Address, amount: U256) -> SubCallOutput {
    deposit_with_value(world, asset, amount, U256::ZERO)
}

fn deposit_with_value(
    world: &mut EvmWorld,
    asset: Address,
    amount: U256,
    value: U256,
) -> SubCallOutput {
    let call = IPayNote::depositCall {
        asset,
        amount,
        lock: field_to_b256(&lock()).unwrap(),
        deadline: DEADLINE,
    };
    let input = SubCallInput {
        target: PAYNOTE_ADDRESS,
        value,
        calldata: call.abi_encode().into(),
        gas_limit: 5_000_000,
        is_static: false,
    };
    world
        .run(ALICE, input)
        .expect("deposit must not fail fatally")
}

fn native_balance(world: &mut EvmWorld, account: Address) -> U256 {
    world
        .evm
        .state_mut()
        .account(&account)
        .map_or(U256::ZERO, |account| account.balance())
}

fn leaf_count(world: &mut EvmWorld) -> u64 {
    world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::leafCountCall {})
}

/// `[alice, pool, vault]` balances of `asset`.
fn balances(world: &mut EvmWorld, asset: Address) -> [U256; 3] {
    [ALICE, PAYNOTE_ADDRESS, VAULT]
        .map(|account| world.view(ALICE, asset, IFixture::balanceOfCall { account }))
}

/// A revert at any step of the deposit must roll back the full deposit. No
/// commitment must stay for value that the pool did not receive.
fn assert_pristine(world: &mut EvmWorld) {
    assert_eq!(leaf_count(world), 0);
    assert_eq!(
        world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::currentRootCall {}),
        B256::ZERO
    );
}

fn assert_reverted(out: &SubCallOutput) {
    assert!(
        !matches!(out.status, SubCallStatus::Success),
        "deposit must revert"
    );
}

#[test]
fn deposit_holds_full_width_amount_in_the_pool_and_appends_the_derived_commitment() {
    let mut world = world();
    let amount = (U256::from(1) << 200) + U256::from(DEPOSIT_AMOUNT);
    let [alice_before, pool_before, vault_before] = balances(&mut world, ASSET);

    let out = deposit(&mut world, ASSET, amount);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "deposit must succeed, got {:?} returndata 0x{}",
        out.status,
        alloy_primitives::hex::encode(&out.returndata),
    );

    // The pool keeps the tokens. Only a spend routes value into the vault.
    assert_eq!(
        balances(&mut world, ASSET),
        [alice_before - amount, pool_before + amount, vault_before]
    );
    assert_eq!(leaf_count(&mut world), 1);
    // The leaf is the commitment the runtime derived from the asset and amount
    // it actually moved, not anything the caller supplied.
    assert!(world.view(
        ALICE,
        PAYNOTE_ADDRESS,
        IPayNote::hasCommitmentCall {
            commitment: expected_commitment(ASSET, amount),
        },
    ));
    // A claim proof built against the post-deposit root right now would resolve.
    let root = world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::currentRootCall {});
    assert!(world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::isKnownRootCall { root }));
}

#[test]
fn deposit_reverts_and_leaves_no_leaf_when_the_asset_has_no_vault() {
    let mut world = world();
    let before = balances(&mut world, UNREGISTERED_ASSET);
    assert_reverted(&deposit(
        &mut world,
        UNREGISTERED_ASSET,
        U256::from(DEPOSIT_AMOUNT),
    ));
    assert_eq!(balances(&mut world, UNREGISTERED_ASSET), before);
    assert_pristine(&mut world);
}

#[test]
fn a_token_that_delivers_less_than_the_amount_deposits_nothing() {
    let mut world = world();
    // Fixture mode 9: fee-on-transfer, one unit short.
    world.ok(
        ALICE,
        ASSET,
        IFixture::configureCall {
            mode: U256::from(9),
            factory: Address::ZERO,
            reentry: Default::default(),
        },
    );
    let before = balances(&mut world, ASSET);
    assert_reverted(&deposit(&mut world, ASSET, U256::from(DEPOSIT_AMOUNT)));
    assert_eq!(balances(&mut world, ASSET), before);
    assert_pristine(&mut world);
}

#[test]
fn a_duplicate_leaf_reverts_but_a_differing_amount_under_the_same_lock_appends() {
    let mut world = world();
    let amount = U256::from(DEPOSIT_AMOUNT);
    assert!(matches!(
        deposit(&mut world, ASSET, amount).status,
        SubCallStatus::Success
    ));
    assert_reverted(&deposit(&mut world, ASSET, amount));
    assert_eq!(leaf_count(&mut world), 1);

    let other = amount + U256::ONE;
    assert!(matches!(
        deposit(&mut world, ASSET, other).status,
        SubCallStatus::Success
    ));
    assert_eq!(leaf_count(&mut world), 2);
    for amount in [amount, other] {
        assert!(world.view(
            ALICE,
            PAYNOTE_ADDRESS,
            IPayNote::hasCommitmentCall {
                commitment: expected_commitment(ASSET, amount),
            },
        ));
    }
}

#[test]
fn native_deposit_moves_value_into_the_pool_and_appends_the_commitment() {
    let mut world = world();
    let amount = U256::from(DEPOSIT_AMOUNT);

    // A mismatched value reverts and the route refunds it.
    assert_reverted(&deposit_with_value(
        &mut world,
        Address::ZERO,
        amount,
        amount - U256::ONE,
    ));
    assert_eq!(native_balance(&mut world, ALICE), U256::from(ALICE_COEN));
    assert_pristine(&mut world);

    let out = deposit_with_value(&mut world, Address::ZERO, amount, amount);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "native deposit must succeed, got {:?} returndata 0x{}",
        out.status,
        alloy_primitives::hex::encode(&out.returndata),
    );
    assert_eq!(
        native_balance(&mut world, ALICE),
        U256::from(ALICE_COEN) - amount
    );
    assert_eq!(native_balance(&mut world, PAYNOTE_ADDRESS), amount);
    assert_eq!(leaf_count(&mut world), 1);
    assert!(world.view(
        ALICE,
        PAYNOTE_ADDRESS,
        IPayNote::hasCommitmentCall {
            commitment: expected_commitment(Address::ZERO, amount),
        },
    ));
}
