//! Unit and dispatch tests for the PayNote precompile.
//!
//! The round-trip tests are the load-bearing ones: they prove a real
//! `outbe.paynote@1.3.0` statement from **Rust-computed** public inputs and
//! verify it through the production decoder. If `hash.rs` drifted from the
//! frozen circuit's `paynote.nr`, the Rust root/nullifier would disagree with
//! the in-circuit ones and proving would fail. That failure is what pins the
//! mirror.
//!
//! `deposit` makes VaultRouter and ERC20 sub-calls. The in-memory storage
//! provider cannot model the token state of these calls. Thus, these tests
//! cover only the pre-mutation guards of `deposit`. The EVM-level test
//! `crates/runtime/tests/paynote_deposit.rs` covers the full custody path.
//! Spends use stubbed sub-calls (see [`seed_pool`]). Thus, these tests pin the
//! calls that a spend makes.

mod merge;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use ark_ff::Zero;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_zk::noir::paynote::{Paynote, PublicInputs};
use outbe_zk::paynote::{COMBINED_LEN as PAYNOTE_COMBINED_LEN, PROOF_WORDS as PAYNOTE_PROOF_WORDS};
use outbe_zk::CircuitId as _;

use crate::hash::empty_subtrees;
use crate::precompile::{base_gas, dispatch, IPayNote, PAYABLE_SELECTORS};
use crate::runtime;
use crate::schema::{
    PayNoteContract, PAYNOTE_ROOT_WINDOW, PAYNOTE_TREE_CAPACITY, PAYNOTE_TREE_DEPTH,
};
use crate::Field;

use outbe_zk::codec::field_to_b256;

use crate::PayNoteTree;

const CHAIN_ID: u64 = 31_337;
const OTHER_CHAIN_ID: u64 = 19_280_501;

const ALICE: Address = Address::new([0x11; 20]);
const USDC: Address = Address::new([0x33; 20]);

/// Fixed non-zero statement for pool tests that never settle a right.
fn statement() -> B256 {
    B256::from(U256::from(1u64))
}
const WBTC: Address = Address::new([0x44; 20]);

fn b256(field: Field) -> B256 {
    field_to_b256(&field).unwrap()
}

fn assert_revert<T: std::fmt::Debug>(result: Result<T, PrecompileError>, expected: &str) {
    match result {
        Err(PrecompileError::Revert(message)) => assert_eq!(message, expected),
        other => panic!("expected revert `{expected}`, got {other:?}"),
    }
}
// ---- proving fixtures -----------------------------------------------------
//
// Note derivation, proving, and pool seeding live in
// `test_support` so downstream consumers of notes can reuse them.

use crate::client::{new_tree, witness};
use crate::test_support::{note, note_and_spend_proof, seed_pool};

/// Prove a spend of `spend_amount` out of a single-leaf tree holding `amount`
/// of `asset`, returning the combined proof and the statement it carries.
fn prove_spend(
    chain_id: u64,
    asset: Address,
    amount: u128,
    spend_amount: u128,
) -> (Vec<u8>, PublicInputs, PayNoteTree) {
    let fixture = note_and_spend_proof(
        chain_id,
        asset,
        statement(),
        U256::from(amount),
        U256::from(spend_amount),
    );
    (fixture.proof, fixture.public, fixture.tree)
}

// ---- selectors, value policy, gas -----------------------------------------

#[test]
fn paynote_takes_no_native_value() {
    assert_eq!(PAYABLE_SELECTORS, &[] as &[[u8; 4]]);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enter(|storage| {
        let data = IPayNote::currentRootCall {}.abi_encode();
        let result = dispatch(storage, &data, ALICE, U256::from(1u64));
        assert!(
            result.is_err(),
            "a non-payable precompile must reject credited value"
        );
    });
}

#[test]
fn unknown_selector_is_priced_out_of_gas() {
    assert_eq!(base_gas(&[0xde, 0xad, 0xbe, 0xef]), u64::MAX);
    assert_eq!(
        base_gas(&IPayNote::currentRootCall {}.abi_encode()),
        crate::precompile::PAYNOTE_VIEW_BASE_GAS
    );
}

// ---- formula + tree parity ------------------------------------------------

#[test]
fn asset_separates_otherwise_identical_notes() {
    // The whole reason the commitment binds the asset: a note funded in one
    // token must not be spendable as another.
    let usdc = note(CHAIN_ID, 17, USDC, U256::from(100));
    let wbtc = note(CHAIN_ID, 17, WBTC, U256::from(100));
    assert_eq!(usdc.serial, wbtc.serial, "serial is asset-independent");
    assert_ne!(usdc.commitment, wbtc.commitment);
    assert_ne!(usdc.nullifier, wbtc.nullifier);
}

#[test]
fn one_serial_two_amounts_stay_independently_spendable() {
    // The nullifier binds the commitment, not the serial, so two notes sharing
    // a serial do not alias onto one nullifier and lock each other out.
    let a = note(CHAIN_ID, 17, USDC, U256::from(40));
    let b = note(CHAIN_ID, 17, USDC, U256::from(60));
    assert_eq!(a.serial, b.serial);
    assert_ne!(a.commitment, b.commitment);
    assert_ne!(a.nullifier, b.nullifier);
}

#[test]
fn empty_leaf_is_chain_specific_and_nonzero() {
    let here = empty_subtrees(CHAIN_ID, PAYNOTE_TREE_DEPTH).unwrap();
    let there = empty_subtrees(OTHER_CHAIN_ID, PAYNOTE_TREE_DEPTH).unwrap();
    assert!(!here[0].is_zero());
    assert_ne!(here[0], there[0], "empty leaf must bind the chain");
    assert_ne!(
        here[PAYNOTE_TREE_DEPTH], there[PAYNOTE_TREE_DEPTH],
        "so must the empty root"
    );
}

#[test]
fn incremental_append_matches_naive_recompute() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let mut reference = new_tree(CHAIN_ID).unwrap();
    let leaves: Vec<Field> = (0..5)
        .map(|i| note(CHAIN_ID, 100 + i, USDC, U256::from(10 + u128::from(i))).commitment)
        .collect();

    seed_pool(&mut provider, CHAIN_ID, &leaves);
    for leaf in &leaves {
        reference.append(*leaf).unwrap();
    }

    provider.enter(|storage| {
        let paynote: PayNoteContract<'_> = storage.contract();
        assert_eq!(paynote.leaf_count.read().unwrap(), leaves.len() as u64);
        assert_eq!(
            paynote.current_root.read().unwrap(),
            b256(reference.root()),
            "O(depth) frontier append must agree with recompute-from-leaves"
        );
    });
}

#[test]
fn root_window_retains_only_the_last_entries() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    // One seeded empty root plus enough appends to overflow the window.
    let leaves: Vec<Field> = (0..PAYNOTE_ROOT_WINDOW + 4)
        .map(|i| note(CHAIN_ID, 1_000 + u64::from(i), USDC, U256::from(7)).commitment)
        .collect();
    seed_pool(&mut provider, CHAIN_ID, &leaves);

    provider.enter(|storage| {
        let paynote: PayNoteContract<'_> = storage.contract();
        let roots = paynote.recent_roots.read_all().unwrap();
        assert_eq!(roots.len(), PAYNOTE_ROOT_WINDOW as usize);
        assert!(
            roots.contains(&paynote.current_root.read().unwrap()),
            "the live root must always be acceptable"
        );
    });
}

#[test]
fn tree_capacity_bound_exceeds_u32() {
    // The depth-32 capacity does not fit u32. A u32 leaf counter would wrap on
    // the final append instead of reporting a full tree.
    assert_eq!(PAYNOTE_TREE_CAPACITY, 1u64 << 32);
    assert!(PAYNOTE_TREE_CAPACITY > u64::from(u32::MAX));
}

// ---- deposit guards -------------------------------------------------------

#[test]
fn deposit_guards_fire_before_any_sub_call() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let serial = note(CHAIN_ID, 17, USDC, U256::from(100)).serial;

    provider.enter(|storage| {
        let result = runtime::deposit(storage, ALICE, USDC, U256::from(0), b256(serial));
        assert_revert(result, "PayNote deposit amount must be non-zero");
    });
    provider.enter(|storage| {
        let result = runtime::deposit(storage, ALICE, Address::ZERO, U256::from(100), b256(serial));
        assert_revert(result, "PayNote asset must be non-zero");
    });
    provider.enter(|storage| {
        let result = runtime::deposit(storage, ALICE, USDC, U256::from(100), B256::ZERO);
        assert_revert(result, "PayNote noteSn must be non-zero");
    });
    provider.enter(|storage| {
        // All-ones is above the BN254 modulus, so it is not a canonical word.
        let result = runtime::deposit(
            storage,
            ALICE,
            USDC,
            U256::from(100),
            B256::repeat_byte(0xff),
        );
        assert_revert(result, "PayNote noteSn is not a canonical BN254 field");
    });
}

// ---- consume guards -------------------------------------------------------

#[test]
fn consume_on_a_pristine_pool_reverts() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, _) = prove_spend(CHAIN_ID, USDC, 100, 100);
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote is not initialized",
        );
    });
}

#[test]
fn consume_rejects_a_root_outside_the_window() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, _) = prove_spend(CHAIN_ID, USDC, 100, 100);
    // A pool holding a different leaf never produced the proof's root.
    seed_pool(
        &mut provider,
        CHAIN_ID,
        &[note(CHAIN_ID, 999, USDC, U256::from(5)).commitment],
    );
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote root is not recent",
        );
    });
}

#[test]
fn consume_rejects_a_foreign_chain_statement() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, tree) = prove_spend(OTHER_CHAIN_ID, USDC, 100, 100);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote chain ID does not match runtime",
        );
    });
}

#[test]
fn consume_rejects_a_malformed_proof() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(
        &mut provider,
        CHAIN_ID,
        &[note(CHAIN_ID, 999, USDC, U256::from(5)).commitment],
    );
    provider.enter(|storage| {
        let result = runtime::consume(&storage, &[0u8; 8]);
        assert!(
            matches!(result, Err(PrecompileError::Revert(ref m)) if m.starts_with("PayNote proof is malformed")),
            "got {result:?}"
        );
    });
}

// ---- real-proof round trips ----------------------------------------------

#[test]
fn full_spend_round_trip_books_the_nullifier_and_no_change() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 100);

    // The frozen circuit's transcript is fixed-length. This is what pins
    // `PAYNOTE_PROOF_WORDS`.
    assert_eq!(
        proof.len(),
        PAYNOTE_COMBINED_LEN,
        "combined proof length must match the pinned frozen wire"
    );
    assert_eq!(proof.len(), 4 + (9 + PAYNOTE_PROOF_WORDS) * 32);
    assert!(
        public.change_commitment.is_zero(),
        "full spend has no change"
    );

    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        let claim = runtime::consume(&storage, &proof).expect("valid full spend");
        assert_eq!(claim.asset, USDC);
        assert_eq!(claim.context, statement());
        assert_eq!(claim.spend_amount, 100);

        let paynote: PayNoteContract<'_> = storage.contract();
        assert!(paynote
            .spent_nullifiers
            .read(&b256(public.nullifier))
            .unwrap());
        assert_eq!(
            paynote.leaf_count.read().unwrap(),
            1,
            "a full spend appends nothing"
        );
    });
}

#[test]
fn replaying_a_spent_proof_reverts() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, tree) = prove_spend(CHAIN_ID, USDC, 100, 100);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());

    provider.enter(|storage| {
        runtime::consume(&storage, &proof).expect("first spend");
    });
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote nullifier has already been spent",
        );
    });
}

#[test]
fn partial_spend_appends_exactly_the_circuit_derived_change() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    assert!(
        !public.change_commitment.is_zero(),
        "partial spend must publish change"
    );

    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        let claim = runtime::consume(&storage, &proof).expect("valid partial spend");
        assert_eq!(claim.spend_amount, 40);

        let paynote: PayNoteContract<'_> = storage.contract();
        assert_eq!(
            paynote.leaf_count.read().unwrap(),
            2,
            "the change note is appended"
        );
        assert!(
            paynote
                .commitments
                .read(&b256(public.change_commitment))
                .unwrap(),
            "the appended leaf is the circuit-derived change commitment"
        );

        // And the resulting root must match a recompute over both leaves.
        let mut reference = new_tree(CHAIN_ID).unwrap();
        reference.append(tree.leaves()[0]).unwrap();
        reference.append(public.change_commitment).unwrap();
        assert_eq!(paynote.current_root.read().unwrap(), b256(reference.root()));
    });
}

#[test]
fn a_spend_routes_exactly_the_spent_amount_into_the_vault() {
    use crate::sol_ext::IERC20;
    use outbe_system_common::addresses::{PAYNOTE_ADDRESS, VAULT_ROUTER_ADDRESS};
    use outbe_vaultrouter::precompile::IVaultRouter;

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| runtime::consume(&storage, &proof).unwrap());

    // The pool approves the router for only the spent amount. The router pulls
    // that amount from the pool. The change of 60 stays in the pool.
    let calls: Vec<_> = provider
        .sub_call_inputs()
        .iter()
        .map(|input| (input.target, input.calldata.clone()))
        .collect();
    let approve = IERC20::approveCall {
        spender: VAULT_ROUTER_ADDRESS,
        amount: U256::from(40),
    };
    let route = IVaultRouter::depositFromCall {
        payer: PAYNOTE_ADDRESS,
        asset: USDC,
        assetsMinor: U256::from(40),
    };
    assert_eq!(
        calls,
        [
            (USDC, approve.abi_encode().into()),
            (VAULT_ROUTER_ADDRESS, route.abi_encode().into()),
        ]
    );
}

#[test]
fn a_failed_vault_routing_leaves_the_note_unspent() {
    use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;

    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.stub_sub_call_revert_at(VAULT_ROUTER_ADDRESS, Default::default());
    let before = provider.storage.clone();

    let result = provider.enter(|storage| runtime::consume(&storage, &proof));
    assert!(result.is_err(), "{result:?}");
    assert_eq!(provider.storage, before);
    assert!(provider.get_ordered_events().is_empty());
    assert_unspent(&mut provider, &public);
}

fn assert_unspent(provider: &mut HashMapStorageProvider, public: &PublicInputs) {
    provider.enter(|storage| {
        let paynote: PayNoteContract<'_> = storage.contract();
        assert!(!paynote
            .spent_nullifiers
            .read(&b256(public.nullifier))
            .unwrap());
    });
}

#[test]
fn a_change_the_pool_cannot_append_leaves_the_note_unspent() {
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    let full_spend = prove_spend(CHAIN_ID, USDC, 100, 100).0;
    for (tree_full, expected) in [
        (true, "PayNote commitment tree is full"),
        (false, "PayNote commitment already exists"),
    ] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        provider.enter(|storage| {
            let paynote: PayNoteContract<'_> = storage.contract();
            if tree_full {
                paynote.leaf_count.write(PAYNOTE_TREE_CAPACITY).unwrap();
            } else {
                paynote
                    .commitments
                    .write(&b256(public.change_commitment), true)
                    .unwrap();
            }
        });
        let before = provider.storage.clone();
        provider.enter(|storage| assert_revert(runtime::consume(&storage, &proof), expected));
        assert_eq!(provider.storage, before, "{expected}");
        assert!(provider.get_ordered_events().is_empty());
        assert_unspent(&mut provider, &public);

        // A full spend appends nothing, so neither condition blocks it.
        provider.enter(|storage| runtime::consume(&storage, &full_spend).unwrap());
    }
}

#[test]
fn a_failure_after_the_nullifier_is_booked_rolls_the_whole_spend_back() {
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    let before = provider.storage.clone();

    let mut probe = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut probe, CHAIN_ID, tree.leaves());
    probe.fail_after_mutation_at(usize::MAX);
    probe.enter(|storage| runtime::consume(&storage, &proof).unwrap());
    let mutations = probe.clear_mutation_failure();
    assert!(
        mutations >= 4,
        "nullifier, change leaf, its commitment and events"
    );

    for operation in 0..mutations {
        provider.fail_after_mutation_at(operation);
        let error = provider
            .enter(|storage| runtime::consume(&storage, &proof))
            .unwrap_err();
        assert!(matches!(error, PrecompileError::Storage(_)), "{error:?}");
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, before, "storage at {operation}");
        assert!(provider.get_ordered_events().is_empty());
        assert_unspent(&mut provider, &public);
    }

    provider.enter(|storage| runtime::consume(&storage, &proof).unwrap());
    assert_eq!(provider.storage, probe.storage);
}

#[test]
fn full_width_u256_spend_round_trip() {
    assert_eq!(Paynote::VERSION, "1.4.0");
    assert_eq!(
        Paynote::CIRCUIT_HASH,
        alloy_primitives::hex!("e7216d1f70288b290aaa27e894100c47d48d8562a1d77a720061898c6bdf25a3")
    );
    assert_eq!(
        Paynote::VK_HASH,
        alloy_primitives::hex!("53c1f38158245aceab7897f5c4aabb4e41e8199a0569f52a9b9d43f7d2a3d6fd")
    );

    let note_amount = (U256::from(1) << 200) + U256::from(100);
    let spend_amount = (U256::from(1) << 199) + U256::from(40);
    let fixture = note_and_spend_proof(CHAIN_ID, USDC, statement(), note_amount, spend_amount);
    assert_eq!(fixture.proof.len(), PAYNOTE_COMBINED_LEN);

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, &[fixture.commitment]);
    provider.enter(|storage| {
        let claim = runtime::consume(&storage, &fixture.proof).expect("valid U256 spend");
        assert_eq!(claim.spend_amount, spend_amount);
        let paynote: PayNoteContract<'_> = storage.contract();
        assert!(paynote
            .commitments
            .read(&b256(fixture.public.change_commitment))
            .unwrap());
    });
}

#[test]
fn the_merge_circuit_is_pinned() {
    use outbe_zk::paynote_merge::PaynoteMerge;
    assert_eq!(PaynoteMerge::LABEL, "outbe.paynote.merge");
    assert_eq!(PaynoteMerge::VERSION, "1.1.0");
    assert_eq!(
        PaynoteMerge::CIRCUIT_HASH,
        alloy_primitives::hex!("ca97d3c1caffb3377304787e26791bea5ec5f46e1595d897caeb66d59a14b502")
    );
    assert_eq!(
        PaynoteMerge::VK_HASH,
        alloy_primitives::hex!("1d4914bd70dcb46009d6cc2ac64271f80f01a8e0711ce71c2f62766fdccaa24c")
    );
}

#[test]
fn note_hashes_match_known_answers() {
    use crate::hash::{note_commitment, note_nullifier, note_sn};
    let spend_key = Field::from(17u64);
    let serial = note_sn(spend_key).unwrap();
    let amount = (U256::from(1) << 200) + U256::from(100);
    let commitment = note_commitment(CHAIN_ID, serial, USDC, amount).unwrap();
    let nullifier = note_nullifier(commitment, spend_key).unwrap();
    assert_eq!(
        b256(serial),
        alloy_primitives::b256!("0f35b19d42814cf1a8537456508387afd5aa95683c85d7391a701038a6ec71b1")
    );
    assert_eq!(
        b256(commitment),
        alloy_primitives::b256!("028fbf70b0d0d472b78ddb7fe58bc963966b7bf2e45c012feceac9c6ecb5d9a9")
    );
    assert_eq!(
        b256(nullifier),
        alloy_primitives::b256!("2128c37317f20aa992287b2055d86628181dc8db1d7dab90568103e8c38c06ec")
    );
}

#[test]
fn zero_context_is_rejected_before_verification() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (mut proof, _, tree) = prove_spend(CHAIN_ID, USDC, 100, 100);
    proof[132..164].fill(0);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote context must be non-zero",
        );
    });
}

#[test]
fn two_contexts_of_one_note_share_one_nullifier() {
    let first = note_and_spend_proof(
        CHAIN_ID,
        USDC,
        statement(),
        U256::from(100u64),
        U256::from(100u64),
    );
    let second = note_and_spend_proof(
        CHAIN_ID,
        USDC,
        B256::from(U256::from(2u64)),
        U256::from(100u64),
        U256::from(100u64),
    );
    assert_eq!(first.public.nullifier, second.public.nullifier);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, &[first.commitment]);
    provider.enter(|storage| {
        runtime::consume(&storage, &first.proof).unwrap();
        assert_revert(
            runtime::consume(&storage, &second.proof),
            "PayNote nullifier has already been spent",
        );
    });
}

#[test]
fn a_substituted_context_fails_verification() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (mut proof, _, tree) = prove_spend(CHAIN_ID, USDC, 100, 100);
    assert_eq!(proof[163], 1);
    proof[163] = 2;
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote proof is invalid",
        );
    });
}

#[test]
fn a_substituted_change_commitment_fails_verification() {
    const CHANGE: std::ops::Range<usize> = 4 + 8 * 32..4 + 9 * 32;
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 40);
    assert_eq!(&proof[CHANGE], b256(public.change_commitment).as_slice());
    let foreign = note(CHAIN_ID, 23, USDC, U256::from(60)).commitment;
    for substitute in [foreign, Field::zero()] {
        let mut forged = proof.clone();
        forged[CHANGE].copy_from_slice(b256(substitute).as_slice());
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        let before = provider.storage.clone();
        provider.enter(|storage| {
            assert_revert(
                runtime::consume(&storage, &forged),
                "PayNote proof is invalid",
            );
        });
        assert_eq!(provider.storage, before);
        assert_unspent(&mut provider, &public);
    }
}

#[test]
fn a_note_cannot_be_spent_as_a_different_asset() {
    // Bind the proof to USDC, then seed a pool whose leaf was built for WBTC:
    // membership fails, so the spend is rejected.
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, _, _) = prove_spend(CHAIN_ID, USDC, 100, 100);
    seed_pool(
        &mut provider,
        CHAIN_ID,
        &[note(CHAIN_ID, 17, WBTC, U256::from(100)).commitment],
    );
    provider.enter(|storage| {
        assert_revert(
            runtime::consume(&storage, &proof),
            "PayNote root is not recent",
        );
    });
}

#[test]
fn client_witnesses_match_runtime_roots() {
    let mut tree = new_tree(CHAIN_ID).unwrap();
    assert!(tree.leaves().is_empty());
    assert!(witness(&tree, Field::from(1)).is_err());
    for i in 1..=9 {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        tree.append(Field::from(i)).unwrap();
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        provider.enter(|storage| {
            let paynote: PayNoteContract<'_> = storage.contract();
            assert_eq!(paynote.current_root.read().unwrap(), b256(tree.root()));
        });
        for (index, leaf) in tree.leaves().iter().enumerate() {
            let (actual_index, siblings) = witness(&tree, *leaf).unwrap();
            assert_eq!(actual_index, u32::try_from(index).unwrap());
            let root = PayNoteTree::root_from_inclusion_path(
                crate::hash::paynote_domain(),
                *leaf,
                u64::from(actual_index),
                &siblings,
            )
            .unwrap();
            assert_eq!(root, tree.root());
        }
    }
    assert!(witness(&tree, Field::from(10)).is_err());
    for (domain, depth) in [
        (crate::hash::paynote_domain(), PAYNOTE_TREE_DEPTH - 1),
        (Field::from(42), PAYNOTE_TREE_DEPTH),
    ] {
        let mut wrong_tree = PayNoteTree::new(domain, Field::from(0), depth).unwrap();
        wrong_tree.append(Field::from(1)).unwrap();
        assert!(witness(&wrong_tree, Field::from(1)).is_err());
    }
}

#[test]
fn only_a_settlement_factory_may_consume_through_the_abi() {
    use outbe_system_common::addresses::NOD_FACTORY_ADDRESS;

    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let (proof, public, tree) = prove_spend(CHAIN_ID, USDC, 100, 100);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    let data = IPayNote::consumeCall {
        proof: proof.into(),
    }
    .abi_encode();
    assert_eq!(base_gas(&data), crate::precompile::PAYNOTE_MERGE_BASE_GAS);

    let before = provider.storage.clone();
    provider.enter(|storage| {
        assert_revert(
            dispatch(storage, &data, ALICE, U256::ZERO),
            "PayNote caller may not spend notes",
        )
    });
    assert_eq!(provider.storage, before);

    provider.enter(|storage| {
        let ret = dispatch(storage, &data, NOD_FACTORY_ADDRESS, U256::ZERO).unwrap();
        let ret = IPayNote::consumeCall::abi_decode_returns(&ret).unwrap();
        assert_eq!(ret.asset, USDC);
        assert_eq!(ret.spendAmount, U256::from(100));
        assert_eq!(ret.context, statement());
        assert_eq!(ret.nullifier, b256(public.nullifier));
    });
}
