//! The in-process enclave stand-in (`enclave_client::test_enclave`) drives these
//! confidential Gratis tests. It runs the real enclave engine against a fixed dev
//! state key. The tests check balances: they decrypt the ciphertext with the
//! account's view key exactly as a client would.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{FidelityCohortOp, FidelityOpSection, GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{decrypt_balance, derive_modify_key, derive_view_key, modify_mac};

use crate::api;
use crate::enclave_client::test_enclave;
use crate::precompile::{dispatch, IGratis};

const CHAIN_ID: u64 = 1;

fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}
fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}

/// Build the modify authorization a client would send for `op`.
fn auth(op: GratisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
    let sk = test_enclave::state_key();
    let mk = derive_modify_key(&sk, account).unwrap();
    ModifyAuth {
        mac: modify_mac(&mk, account, op, amount, nonce, chain_b256()),
        op_nonce: nonce,
    }
}

fn view_balance(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::balance_ct(storage, account).unwrap();
    decrypt_balance(&vk, account, &blob).unwrap()
}

/// Run `f` inside a fresh storage scope with the in-process enclave installed.
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

#[test]
fn mine_credits_encrypted_balance() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);

        // Second mine advances the op nonce and accumulates the (hidden) balance.
        let more = U256::from(500u64);
        api::mint(
            storage.clone(),
            alice(),
            more,
            auth(GratisOp::Mint, alice(), more, 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(1500u64));
    });
}

#[test]
fn one_whole_gratis_round_trips_as_one_million_raw_units() {
    with_env(|storage| {
        let one_gratis = U256::from(1_000_000u64);
        api::mint(
            storage.clone(),
            alice(),
            one_gratis,
            auth(GratisOp::Mint, alice(), one_gratis, 0),
        )
        .unwrap();

        assert_eq!(view_balance(storage.clone(), alice()), one_gratis);
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(GratisOp::Mint, alice(), amount, 0);
        api::mint(storage.clone(), alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail. The nonce advanced to 1.
        assert!(api::mint(storage.clone(), alice(), amount, a).is_err());
    });
}

#[test]
fn mine_rejects_forged_auth() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let mut a = auth(GratisOp::Mint, alice(), amount, 0);
        a.mac[0] ^= 0xff;
        assert!(api::mint(storage.clone(), alice(), amount, a).is_err());
    });
}

#[test]
fn burn_reduces_encrypted_balance() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(1000u64),
            auth(GratisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();
        api::burn(
            storage.clone(),
            alice(),
            U256::from(400u64),
            auth(GratisOp::Burn, alice(), U256::from(400u64), 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(600u64));
    });
}

#[test]
fn burn_insufficient_balance_reverts() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(100u64),
            auth(GratisOp::Mint, alice(), U256::from(100u64), 0),
        )
        .unwrap();
        assert!(api::burn(
            storage.clone(),
            alice(),
            U256::from(200u64),
            auth(GratisOp::Burn, alice(), U256::from(200u64), 1)
        )
        .is_err());
    });
}

// --- Precompile ABI surface (no enclave needed for the non-transferable stubs) ---

fn run_dispatch(call: Bytes, caller: Address) -> outbe_primitives::error::Result<Bytes> {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        dispatch(storage.clone(), &call, caller, U256::ZERO)
    })
}

#[test]
fn metadata_uses_six_decimal_gratis_units() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let gratis = crate::Gratis::new(storage);
        assert_eq!(gratis.name(), "gratis");
        assert_eq!(gratis.symbol(), "GRATIS");
        assert_eq!(gratis.decimals(), 6);
    });
}

#[test]
fn precompile_transfer_reverts() {
    let call = Bytes::from(
        IGratis::IGratisCalls::transfer(IGratis::transferCall {
            to: Address::repeat_byte(0x22),
            amount: U256::from(1u64),
        })
        .abi_encode(),
    );
    let err = run_dispatch(call, alice()).unwrap_err();
    assert!(err.to_string().contains("transfers are not allowed"));
}

#[test]
fn precompile_balance_of_returns_ciphertext() {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| {
        let amount = U256::from(777u64);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let call = Bytes::from(
            IGratis::IGratisCalls::balanceOf(IGratis::balanceOfCall { account: alice() })
                .abi_encode(),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap()
    });
    // The returned bytes are the ciphertext blob. Decrypt them with the view key.
    let blob = IGratis::balanceOfCall::abi_decode_returns(&out).unwrap();
    let vk = derive_view_key(&test_enclave::state_key(), alice()).unwrap();
    assert_eq!(
        decrypt_balance(&vk, alice(), &blob).unwrap(),
        U256::from(777u64)
    );
    test_enclave::uninstall();
}

#[test]
fn folded_fidelity_section_failure_reverts_the_whole_op() {
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        // A folded mint whose fidelity section carries an UNDECRYPTABLE cohort
        // blob. The gratis mint half would succeed, but the enclave rejects the
        // WHOLE op when the section fails. So nothing is committed.
        let bad_section = FidelityOpSection {
            op: FidelityCohortOp::In,
            timestamp: 1_000_000,
            first_qualified_start: 0,
            current_blob: vec![0xAB; 56], // valid length, garbage ciphertext/tag
        };
        let err = api::mint_with_fidelity(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
            bad_section,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("fidelity section failed"),
            "expected a fidelity-section rejection, got: {err}"
        );

        // Atomic revert: no gratis state was written (balance, op_nonce).
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 0);
    });
}

fn probe() -> FidelityOpSection {
    FidelityOpSection {
        op: FidelityCohortOp::Probe,
        timestamp: 1_700_000_000,
        first_qualified_start: 0,
        current_blob: Vec::new(),
    }
}

#[test]
fn real_note_profiles_share_nullifiers_and_preserve_original_owner() {
    use crate::{client, pledge::PledgePool};
    use outbe_protocol::{codec, protocol::zk::ProofGenerator};
    use outbe_zk_backend::barretenberg::{verify_circuit, Barretenberg};
    use outbe_zk_canonical::noir::{pledgenote_issue, pledgenote_unpledge};
    with_env(|storage| {
        let amount = (U256::ONE << 200usize) + U256::from(100);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let key = derive_modify_key(&test_enclave::state_key(), alice()).unwrap();
        let note = client::Note::initial(CHAIN_ID, alice(), &key, amount, 1).unwrap();
        let (commitment, _) = api::pledge_with_fidelity(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Pledge, alice(), amount, 1),
            probe(),
        )
        .unwrap();
        assert_eq!(commitment, note.commitment().unwrap());
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        let mut tree = client::new_tree(CHAIN_ID).unwrap();
        tree.append(codec::field_from_b256(&commitment).unwrap())
            .unwrap();
        let spend = U256::from(40);
        let context = api::unpledge_context(CHAIN_ID, alice(), spend).unwrap();
        let proof = client::prove_unpledge(&note, &tree, spend, context).unwrap();
        assert!(verify_circuit::<pledgenote_unpledge::PledgenoteUnpledge>(&proof).unwrap());
        let issue = client::prove_issue(&note, &tree, spend, B256::from(U256::from(19))).unwrap();
        let second_context =
            client::prove_issue(&note, &tree, spend, B256::from(U256::from(20))).unwrap();
        let first = pledgenote_issue::decode_public_inputs(&issue).unwrap();
        let second = pledgenote_issue::decode_public_inputs(&second_context).unwrap();
        assert_eq!(first.nullifier, second.nullifier);
        assert_eq!(
            first.nullifier,
            pledgenote_unpledge::decode_public_inputs(&proof)
                .unwrap()
                .nullifier
        );
        assert_ne!(first.return_note_serial, second.return_note_serial);
        // A holder may not redirect the withdrawal or the authenticated return serial.
        let (w, mut p) = client::unpledge_inputs(&note, &tree, spend, context).unwrap();
        p.owner = Address::repeat_byte(0x55);
        assert!(
            ProofGenerator::<pledgenote_unpledge::PledgenoteUnpledge>::generate(
                &Barretenberg::default(),
                &w.try_into().unwrap(),
                &p.try_into().unwrap()
            )
            .is_err()
        );
        let (w, mut p) = client::issue_inputs(&note, &tree, spend, context).unwrap();
        p.return_note_serial = B256::from(U256::from(9));
        assert!(
            ProofGenerator::<pledgenote_issue::PledgenoteIssue>::generate(
                &Barretenberg::default(),
                &w.try_into().unwrap(),
                &p.try_into().unwrap()
            )
            .is_err()
        );
        // Valid cryptography with the wrong operation context still cannot withdraw.
        let wrong =
            client::prove_unpledge(&note, &tree, spend, B256::from(U256::from(99))).unwrap();
        let pool = PledgePool::new(storage.clone());
        let root = pool.current_root.read().unwrap();
        assert!(api::unpledge(storage.clone(), &wrong).is_err());
        assert_eq!(pool.current_root.read().unwrap(), root);
        assert!(!pool
            .spent_nullifiers
            .read(&note.nullifier().unwrap())
            .unwrap());
        api::unpledge(storage.clone(), &proof).unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), spend);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            amount - spend
        );
        assert_eq!(pool.leaf_count.read().unwrap(), 2);
        assert!(crate::pledge::consume_issue(&storage, &issue).is_err());
        assert!(api::unpledge(storage.clone(), &proof).is_err());
        let change = note.change(spend).unwrap().unwrap();
        tree.append(codec::field_from_b256(&change.commitment().unwrap()).unwrap())
            .unwrap();
        assert_eq!(
            codec::field_to_b256(&tree.root()).unwrap(),
            pool.current_root.read().unwrap()
        );
        // A partial withdrawal's change remains owned by the original source.
        let context = api::unpledge_context(CHAIN_ID, alice(), change.amount).unwrap();
        let proof = client::prove_unpledge(&change, &tree, change.amount, context).unwrap();
        api::unpledge(storage.clone(), &proof).unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
}
