//! EVM-level integration test for `IPayNote.deposit`.
//!
//! The paynote crate's own tests cover only `deposit`'s pre-mutation guards,
//! because its body moves real ERC20 balances that an in-memory storage
//! provider cannot model. This test drives the precompile through the real EVM.
//! The ERC20 counterparty is the stateful `FactorySettlement` fixture.
//!
//! What this pins that unit tests cannot:
//!   * the deposited tokens land in the pool at `PAYNOTE_ADDRESS`, not in the
//!     vault. A spend routes them into the vault later.
//!   * the asset must have a registered reserve vault.
//!   * the pool must receive exactly the deposited amount.
//!   * a revert anywhere in that chain rolls the tree back atomically.
//!   * the appended leaf is the runtime-derived commitment, readable through
//!     the public view ABI.

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall};
use evm2::evm::{CacheDB, EmptyDB};
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_paynote::hash::{note_commitment, note_sn};
use outbe_paynote::precompile::IPayNote;
use outbe_paynote::Field;
use outbe_primitives::chain::CHAIN_ID;
use outbe_primitives::storage::{SubCallOutput, SubCallStatus};
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
const SPEND_KEY: u64 = 17;

fn serial() -> Field {
    note_sn(Field::from(SPEND_KEY)).unwrap()
}

/// The commitment the runtime must derive, computed independently here.
fn expected_commitment(asset: Address, amount: U256) -> B256 {
    field_to_b256(&note_commitment(CHAIN_ID, serial(), asset, amount).unwrap()).unwrap()
}

/// `ALICE` holds and has approved both assets to the pool. `ASSET` has a vault
/// and PayNote is a `PayNoteDeposit` source, as a live deployment would have.
fn world() -> EvmWorld {
    let mut db = CacheDB::new(EmptyDB::default());
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
    let call = IPayNote::depositCall {
        asset,
        amount,
        noteSn: field_to_b256(&serial()).unwrap(),
    };
    world.call(ALICE, PAYNOTE_ADDRESS, call.abi_encode().into(), false)
}

fn leaf_count(world: &mut EvmWorld) -> u64 {
    world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::leafCountCall {})
}

/// `[alice, pool, vault]` balances of `asset`.
fn balances(world: &mut EvmWorld, asset: Address) -> [U256; 3] {
    [ALICE, PAYNOTE_ADDRESS, VAULT]
        .map(|account| world.view(ALICE, asset, IFixture::balanceOfCall { account }))
}

/// A revert anywhere in the deposit must roll it back. It must not leave a
/// commitment behind for value the pool never received.
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

    // The pool keeps custody. Only a spend routes value into the vault.
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
    // A proof built against the post-deposit root right now would be spendable.
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
fn a_duplicate_leaf_reverts_but_a_differing_amount_under_the_same_serial_appends() {
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
