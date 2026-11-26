//! EVM-level integration test for `IPayNote.withdraw`.
//!
//! The paynote crate's own tests stub the token, so they pin the sub-call a
//! withdrawal makes but not the value it moves. This test runs deposit and
//! withdrawal through the real EVM with real proofs. The stateful
//! `CredisIssuance` fixture is the ERC20 counterparty.
//!
//! What this pins that unit tests cannot:
//!   * an ERC20 withdrawal moves exactly the spent amount from the pool to the
//!     recipient the proof binds, and the change stays in the pool.
//!   * a native COEN note (`asset == 0`) proves under the frozen circuit and
//!     pays the recipient in native balance.
//!   * a failed payout leaves the note spendable.
//!   * native notes merge, and the merged note pays out more than either input.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{sol, SolCall};
use evm2::evm::{AccountInfo, CacheDB, EmptyDB};
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_paynote::api::withdraw_context;
use outbe_paynote::precompile::IPayNote;
use outbe_paynote::test_support::{change_note, merge_proof, note, spend_proof, Note};
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
        function configure(uint256 value) external;
    }
}

const ALICE: Address = Address::new([0x11; 20]);
const BOB: Address = Address::new([0x22; 20]);
const RELAYER: Address = Address::new([0x77; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const TIMESTAMP: u64 = 1_700_000_000;
const NOTE_AMOUNT: u64 = 1_000;
const SPEND_AMOUNT: u64 = 400;
const ALICE_COEN: u64 = 1_000_000;

/// `ALICE` holds the ERC20, approves the pool, and holds native COEN. `ASSET`
/// has a vault, as a deposit requires.
fn world() -> EvmWorld {
    let mut db = CacheDB::new(EmptyDB::default());
    for account in [ALICE, RELAYER] {
        db.insert_account_info(
            &account,
            AccountInfo {
                balance: U256::from(ALICE_COEN),
                ..Default::default()
            },
        );
    }
    let code = runtime_code(include_str!(
        "../../../testing/fixtures/evm/CredisIssuance.hex"
    ));
    for address in [ASSET, VAULT] {
        install_code(&mut db, address, &code);
    }
    seed_storage(&mut db, TIMESTAMP, ALICE, |storage| {
        register_vault(&storage, ASSET, VAULT, 840).unwrap();
        register_source(&storage, PAYNOTE_ADDRESS, StablecoinSource::PayNoteDeposit).unwrap();
    });
    let mut world =
        EvmWorld::new(db, std::sync::Arc::new(outbe_runtime::OutbeRuntime::new())).at(TIMESTAMP);
    world.ok(
        ALICE,
        ASSET,
        IFixture::mintCall {
            account: ALICE,
            amount: U256::from(NOTE_AMOUNT),
        },
    );
    world.ok(
        ALICE,
        ASSET,
        IFixture::approveCall {
            spender: PAYNOTE_ADDRESS,
            amount: U256::MAX,
        },
    );
    world
}

fn call(world: &mut EvmWorld, caller: Address, value: U256, calldata: Vec<u8>) -> SubCallOutput {
    let input = SubCallInput {
        target: PAYNOTE_ADDRESS,
        value,
        calldata: calldata.into(),
        gas_limit: 10_000_000,
        is_static: false,
    };
    world
        .run(caller, input)
        .expect("call must not fail fatally")
}

fn succeeded(out: &SubCallOutput) -> bool {
    matches!(out.status, SubCallStatus::Success)
}

/// `ALICE` deposits a note of `NOTE_AMOUNT` in `asset` under spend key `key`.
fn deposit(world: &mut EvmWorld, key: u64, asset: Address) -> Note {
    let amount = U256::from(NOTE_AMOUNT);
    let paid = note(CHAIN_ID, key, asset, amount);
    let deposit = IPayNote::depositCall {
        asset,
        amount,
        noteSn: field_to_b256(&paid.serial).unwrap(),
    };
    let value = if asset.is_zero() { amount } else { U256::ZERO };
    assert!(succeeded(&call(world, ALICE, value, deposit.abi_encode())));
    paid
}

/// `ALICE` deposits a note in `asset`. Returns the note and a proof that pays
/// `SPEND_AMOUNT` of it to `recipient`.
fn deposit_and_prove(world: &mut EvmWorld, asset: Address, recipient: Address) -> (Note, Vec<u8>) {
    let paid = deposit(world, 23, asset);

    let mut tree = outbe_paynote::client::new_tree(CHAIN_ID).unwrap();
    tree.append(paid.commitment).unwrap();
    let proof = spend_proof(
        CHAIN_ID,
        &tree,
        0,
        &paid,
        withdraw_context(recipient).unwrap(),
        U256::from(SPEND_AMOUNT),
    );
    (paid, proof)
}

fn withdraw(world: &mut EvmWorld, proof: &[u8], recipient: Address) -> SubCallOutput {
    let data = IPayNote::withdrawCall {
        proof: proof.to_vec().into(),
        recipient,
    };
    call(world, RELAYER, U256::ZERO, data.abi_encode())
}

fn is_spent(world: &mut EvmWorld, paid: &Note) -> bool {
    let nullifier = field_to_b256(&paid.nullifier).unwrap();
    world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::isSpentCall { nullifier })
}

fn native_balance(world: &mut EvmWorld, account: Address) -> U256 {
    world
        .evm
        .state_mut()
        .account(&account)
        .map_or(U256::ZERO, |account| account.balance())
}

/// `[pool, bob, relayer, vault]` balances of `ASSET`.
fn balances(world: &mut EvmWorld) -> [U256; 4] {
    [PAYNOTE_ADDRESS, BOB, RELAYER, VAULT]
        .map(|account| world.view(ALICE, ASSET, IFixture::balanceOfCall { account }))
}

#[test]
fn an_erc20_withdrawal_pays_the_bound_recipient_once_and_keeps_the_change() {
    let mut world = world();
    let (paid, proof) = deposit_and_prove(&mut world, ASSET, BOB);
    let (note_amount, spend) = (U256::from(NOTE_AMOUNT), U256::from(SPEND_AMOUNT));
    let funded = [note_amount, U256::ZERO, U256::ZERO, U256::ZERO];
    assert_eq!(balances(&mut world), funded);

    // The relayer cannot redirect the funds to itself.
    assert!(!succeeded(&withdraw(&mut world, &proof, RELAYER)));
    // Fixture mode 2: the token reverts the payout.
    world.ok(
        ALICE,
        ASSET,
        IFixture::configureCall {
            value: U256::from(2),
        },
    );
    assert!(!succeeded(&withdraw(&mut world, &proof, BOB)));
    world.ok(ALICE, ASSET, IFixture::configureCall { value: U256::ZERO });
    assert!(!is_spent(&mut world, &paid));
    assert_eq!(balances(&mut world), funded);
    assert_eq!(
        world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::leafCountCall {}),
        1
    );

    let out = withdraw(&mut world, &proof, BOB);
    assert!(
        succeeded(&out),
        "withdraw must succeed, got {:?} returndata 0x{}",
        out.status,
        alloy_primitives::hex::encode(&out.returndata),
    );
    assert_eq!(
        balances(&mut world),
        [note_amount - spend, spend, U256::ZERO, U256::ZERO]
    );
    assert!(is_spent(&mut world, &paid));
    let change = change_note(CHAIN_ID, &paid, spend).unwrap();
    assert!(world.view(
        ALICE,
        PAYNOTE_ADDRESS,
        IPayNote::hasCommitmentCall {
            commitment: field_to_b256(&change.commitment).unwrap(),
        },
    ));

    assert!(!succeeded(&withdraw(&mut world, &proof, BOB)));
    assert_eq!(
        balances(&mut world),
        [note_amount - spend, spend, U256::ZERO, U256::ZERO]
    );
}

#[test]
fn a_native_withdrawal_pays_the_recipient_in_coen() {
    let mut world = world();
    let (paid, proof) = deposit_and_prove(&mut world, Address::ZERO, BOB);
    let (note_amount, spend) = (U256::from(NOTE_AMOUNT), U256::from(SPEND_AMOUNT));
    assert_eq!(native_balance(&mut world, PAYNOTE_ADDRESS), note_amount);

    let out = withdraw(&mut world, &proof, BOB);
    assert!(
        succeeded(&out),
        "native withdraw must succeed, got {:?} returndata 0x{}",
        out.status,
        alloy_primitives::hex::encode(&out.returndata),
    );
    assert_eq!(native_balance(&mut world, BOB), spend);
    assert_eq!(
        native_balance(&mut world, PAYNOTE_ADDRESS),
        note_amount - spend
    );
    assert!(is_spent(&mut world, &paid));

    assert!(!succeeded(&withdraw(&mut world, &proof, BOB)));
    assert_eq!(native_balance(&mut world, BOB), spend);
}

#[test]
fn merged_native_notes_withdraw_as_one_note() {
    let mut world = world();
    let amount = U256::from(NOTE_AMOUNT);
    let inputs = [23, 24].map(|key| deposit(&mut world, key, Address::ZERO));
    let mut tree = outbe_paynote::client::new_tree(CHAIN_ID).unwrap();
    for input in &inputs {
        tree.append(input.commitment).unwrap();
    }

    let merged = note(CHAIN_ID, 25, Address::ZERO, amount + amount);
    let merge = IPayNote::mergePayNotesCall {
        proof: merge_proof(CHAIN_ID, &tree, &inputs.each_ref(), &merged).into(),
    };
    let out = call(&mut world, RELAYER, U256::ZERO, merge.abi_encode());
    assert!(
        succeeded(&out),
        "native merge must succeed, got {:?} returndata 0x{}",
        out.status,
        alloy_primitives::hex::encode(&out.returndata),
    );
    tree.append(merged.commitment).unwrap();
    for input in &inputs {
        assert!(is_spent(&mut world, input));
    }
    // A merge moves no funds.
    assert_eq!(native_balance(&mut world, PAYNOTE_ADDRESS), amount + amount);

    // More than either input held.
    let spend = amount + U256::ONE;
    let proof = spend_proof(
        CHAIN_ID,
        &tree,
        2,
        &merged,
        withdraw_context(BOB).unwrap(),
        spend,
    );
    assert!(succeeded(&withdraw(&mut world, &proof, BOB)));
    assert_eq!(native_balance(&mut world, BOB), spend);
    assert_eq!(
        native_balance(&mut world, PAYNOTE_ADDRESS),
        amount + amount - spend
    );
}
