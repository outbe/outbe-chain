//! EVM-level integration test for `IPayNote.transfer` and
//! `IPayNote.resolveTransfer`.
//!
//! Runs the whole transfer flow through the real EVM with real proofs, on
//! native COEN notes. Alice deposits for herself, locks part of the note to
//! Bob's public key, and the locked note is then claimed, declined or
//! reclaimed. Alice also deposits straight to Bob, with no transfer proof.
//!
//! What this pins that unit tests cannot:
//!   * the note a claim, decline or reclaim creates is an ordinary note: its
//!     owner withdraws real value with the frozen spend circuit.
//!   * the pool clock, not the prover, opens the sender's reclaim.
//!   * a transfer and its resolution move no value out of the pool.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use evm2::evm::{AccountInfo, CacheDB, EmptyDB};
use outbe_evm::test_support::EvmWorld;
use outbe_paynote::api::withdraw_context;
use outbe_paynote::hash::note_sn;
use outbe_paynote::precompile::IPayNote;
use outbe_paynote::runtime::PAYNOTE_TRANSFER_TIMELOCK_SECS;
use outbe_paynote::test_support::{
    change_note, claim_proof, locked_note, note, self_claim, self_deposit, spend_proof,
    transfer_proof, LockedNote, Note,
};
use outbe_paynote::{Field, PayNoteTree};
use outbe_primitives::chain::CHAIN_ID;
use outbe_primitives::storage::{SubCallInput, SubCallOutput, SubCallStatus};
use outbe_system_common::addresses::PAYNOTE_ADDRESS;
use outbe_zk::codec::field_to_b256;

const ALICE: Address = Address::new([0x11; 20]);
const BOB: Address = Address::new([0x22; 20]);
const RELAYER: Address = Address::new([0x77; 20]);
const TIMESTAMP: u64 = 1_700_000_000;
const DEADLINE: u64 = TIMESTAMP + PAYNOTE_TRANSFER_TIMELOCK_SECS;
const NOTE_AMOUNT: u64 = 1_000;
const SENT: u64 = 400;
const ALICE_COEN: u64 = 1_000_000;
/// Key of Alice's deposited note.
const ALICE_KEY: u64 = 17;
/// Bob's secret. Alice knows only its `note_sn`.
const BOB_KEY: u64 = 21;
const ALICE_REFUND_KEY: u64 = 22;
/// Key of the note Bob claims into.
const BOB_NOTE_KEY: u64 = 23;
/// Leaves after the transfer: Alice's deposit, her note, the locked note, her
/// change.
const LOCKED_LEAF: u32 = 2;
const RESOLVED_LEAF: u32 = 4;

fn call(world: &mut EvmWorld, caller: Address, value: U256, calldata: Vec<u8>) -> bool {
    let input = SubCallInput {
        target: PAYNOTE_ADDRESS,
        value,
        calldata: calldata.into(),
        gas_limit: 10_000_000,
        is_static: false,
    };
    let out: SubCallOutput = world
        .run(caller, input)
        .expect("call must not fail fatally");
    matches!(out.status, SubCallStatus::Success)
}

fn native_balance(world: &mut EvmWorld, account: Address) -> U256 {
    world
        .evm
        .state_mut()
        .account(&account)
        .map_or(U256::ZERO, |account| account.balance())
}

fn is_spent(world: &mut EvmWorld, nullifier: Field) -> bool {
    let nullifier = field_to_b256(&nullifier).unwrap();
    world.view(ALICE, PAYNOTE_ADDRESS, IPayNote::isSpentCall { nullifier })
}

/// A world where Alice holds native COEN.
fn world() -> EvmWorld {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &ALICE,
        AccountInfo {
            balance: U256::from(ALICE_COEN),
            ..Default::default()
        },
    );
    EvmWorld::new(db, std::sync::Arc::new(outbe_runtime::OutbeRuntime::new())).at(TIMESTAMP)
}

/// The note of `SENT` native COEN that Alice locks to Bob, knowing only his
/// public key.
fn locked_to_bob() -> LockedNote {
    let bob_pk = note_sn(Field::from(BOB_KEY)).unwrap();
    locked_note(
        CHAIN_ID,
        (bob_pk, ALICE_REFUND_KEY, 777),
        DEADLINE,
        Address::ZERO,
        U256::from(SENT),
    )
}

/// Alice deposits `NOTE_AMOUNT` of native COEN for herself, claims it, and
/// locks `SENT` of it to Bob. Returns the world, the pool tree and the locked
/// note.
fn transferred() -> (EvmWorld, PayNoteTree, LockedNote) {
    let mut world = world();
    let amount = U256::from(NOTE_AMOUNT);
    let (deposit, deposited) = self_deposit(CHAIN_ID, ALICE_KEY, TIMESTAMP, Address::ZERO, amount);
    assert!(call(&mut world, ALICE, amount, deposit.abi_encode()));
    let mut tree = outbe_paynote::client::new_tree(CHAIN_ID).unwrap();
    let (claim, source) = self_claim(CHAIN_ID, &mut tree, ALICE_KEY, &deposited);
    assert!(call(&mut world, RELAYER, U256::ZERO, claim.abi_encode()));

    let locked = locked_to_bob();
    let proof = transfer_proof(CHAIN_ID, &tree, 1, &source, &locked);
    let transfer = IPayNote::transferCall {
        proof: proof.into(),
    };
    assert!(call(&mut world, RELAYER, U256::ZERO, transfer.abi_encode()));
    assert!(is_spent(&mut world, source.nullifier));
    tree.append(locked.commitment).unwrap();
    let change = change_note(CHAIN_ID, &source, locked.amount).unwrap();
    tree.append(change.commitment).unwrap();
    assert_eq!(native_balance(&mut world, PAYNOTE_ADDRESS), amount);
    (world, tree, locked)
}

/// The holder of `auth_key` resolves the locked note into a note under
/// `output_key`. Returns whether the pool accepted, and the output note.
fn resolve(
    world: &mut EvmWorld,
    tree: &PayNoteTree,
    locked: &LockedNote,
    (auth_key, not_before): (u64, u64),
    output_key: u64,
) -> (bool, Note) {
    let output = note(CHAIN_ID, output_key, Address::ZERO, locked.amount);
    let proof = claim_proof(
        CHAIN_ID,
        tree,
        LOCKED_LEAF,
        locked,
        (auth_key, not_before),
        &output,
    );
    let data = IPayNote::resolveTransferCall {
        proof: proof.into(),
    };
    (call(world, RELAYER, U256::ZERO, data.abi_encode()), output)
}

/// The owner of the resolved note withdraws all of it to `recipient`.
fn withdraw_all(world: &mut EvmWorld, mut tree: PayNoteTree, owned: &Note, recipient: Address) {
    tree.append(owned.commitment).unwrap();
    let before = native_balance(world, recipient);
    let proof = spend_proof(
        CHAIN_ID,
        &tree,
        RESOLVED_LEAF,
        owned,
        withdraw_context(recipient).unwrap(),
        owned.amount,
    );
    let data = IPayNote::withdrawCall {
        proof: proof.into(),
        recipient,
    };
    assert!(call(world, RELAYER, U256::ZERO, data.abi_encode()));
    assert_eq!(native_balance(world, recipient), before + owned.amount);
    assert_eq!(
        native_balance(world, PAYNOTE_ADDRESS),
        U256::from(NOTE_AMOUNT - SENT)
    );
}

#[test]
fn the_recipient_claims_a_transfer_and_withdraws_it() {
    let (mut world, tree, locked) = transferred();
    let (accepted, claimed) = resolve(&mut world, &tree, &locked, (BOB_KEY, 0), BOB_NOTE_KEY);
    assert!(accepted);
    assert!(is_spent(&mut world, locked.nullifier));
    assert_eq!(
        native_balance(&mut world, PAYNOTE_ADDRESS),
        U256::from(NOTE_AMOUNT)
    );
    withdraw_all(&mut world, tree, &claimed, BOB);
}

#[test]
fn a_declined_transfer_is_the_senders_again_at_once() {
    let (mut world, tree, locked) = transferred();
    // Bob sends the value to the refund serial Alice put in the lock.
    let (accepted, refund) = resolve(&mut world, &tree, &locked, (BOB_KEY, 0), ALICE_REFUND_KEY);
    assert!(accepted);
    assert_eq!(refund.serial, locked.sender_serial);
    withdraw_all(&mut world, tree, &refund, ALICE);
}

#[test]
fn the_sender_reclaims_only_after_the_timelock() {
    let (mut world, tree, locked) = transferred();
    // The circuit accepts a sender only with `not_before >= deadline`, and the
    // pool rejects a `not_before` ahead of its clock.
    let reclaim = (ALICE_REFUND_KEY, DEADLINE);
    let (accepted, _) = resolve(&mut world, &tree, &locked, reclaim, ALICE_REFUND_KEY);
    assert!(!accepted);
    assert!(!is_spent(&mut world, locked.nullifier));

    let mut world = world.at(DEADLINE);
    let (accepted, refund) = resolve(&mut world, &tree, &locked, reclaim, ALICE_REFUND_KEY);
    assert!(accepted);
    // The note has one nullifier, so the recipient is now too late.
    let (accepted, _) = resolve(&mut world, &tree, &locked, (BOB_KEY, 0), BOB_NOTE_KEY);
    assert!(!accepted);
    withdraw_all(&mut world, tree, &refund, ALICE);
}

/// A deposit names its recipient. Thus Alice pays Bob in one call and with no
/// proof, and Bob claims the note as he claims a transfer.
#[test]
fn a_deposit_to_the_recipient_is_claimed_and_withdrawn() {
    let mut world = world();
    let locked = locked_to_bob();
    let deposit = IPayNote::depositCall {
        asset: Address::ZERO,
        amount: locked.amount,
        lock: field_to_b256(&locked.lock).unwrap(),
        deadline: DEADLINE,
    };
    assert!(call(&mut world, ALICE, locked.amount, deposit.abi_encode()));
    let mut tree = outbe_paynote::client::new_tree(CHAIN_ID).unwrap();
    tree.append(locked.commitment).unwrap();

    // Alice is the sender, so she cannot take the deposit back before the
    // deadline.
    let reclaimed = note(CHAIN_ID, ALICE_REFUND_KEY, Address::ZERO, locked.amount);
    let reclaim = (ALICE_REFUND_KEY, DEADLINE);
    let proof = claim_proof(CHAIN_ID, &tree, 0, &locked, reclaim, &reclaimed);
    let data = IPayNote::resolveTransferCall {
        proof: proof.into(),
    };
    assert!(!call(&mut world, RELAYER, U256::ZERO, data.abi_encode()));

    let claimed = note(CHAIN_ID, BOB_NOTE_KEY, Address::ZERO, locked.amount);
    let proof = claim_proof(CHAIN_ID, &tree, 0, &locked, (BOB_KEY, 0), &claimed);
    let data = IPayNote::resolveTransferCall {
        proof: proof.into(),
    };
    assert!(call(&mut world, RELAYER, U256::ZERO, data.abi_encode()));
    tree.append(claimed.commitment).unwrap();

    let context = withdraw_context(BOB).unwrap();
    let proof = spend_proof(CHAIN_ID, &tree, 1, &claimed, context, claimed.amount);
    let data = IPayNote::withdrawCall {
        proof: proof.into(),
        recipient: BOB,
    };
    assert!(call(&mut world, RELAYER, U256::ZERO, data.abi_encode()));
    assert_eq!(native_balance(&mut world, BOB), locked.amount);
    assert_eq!(native_balance(&mut world, PAYNOTE_ADDRESS), U256::ZERO);
}
