//! End-to-end flow: pledge → request → pay → reclaim → unpledge.
//!
//! Vault state is not asserted in these tests: `HashMapStorageProvider` does
//! not run a real EVM, so the runtime's Rust → Solidity sub-calls into
//! `IVaultProvider` / `IERC20` are stubbed via
//! `HashMapStorageProvider::enable_sub_call_stub` (returns
//! `SubCallOutput::default_success()`).
//!
//! Proof verification is similarly stubbed: real Barretenberg proofs take
//! seconds to generate, so each test wraps the spend-side calls in
//! `outbe_gratispool::verifier::with_verifier_outcome(true, ...)` to force
//! the verifier to accept the dummy proof bytes. The runtime gates that
//! sit in front of the verifier (root window, nullifier set, receiver
//! binding) still execute against real on-chain state.

use alloy_primitives::{keccak256, Address, U256};

use outbe_credis::{CredisContract, NUMBER_OF_ANADOSIS, SECONDS_PER_MONTH};
use outbe_gratis::Gratis;
use outbe_gratisfactory::runtime as gf;
use outbe_gratispool::constants::{
    DenomAmount, ACTION_REQUEST_CREDIS, ACTION_UNPLEDGE, DENOMINATION_COUNT,
};
use outbe_gratispool::schema::GratisPoolContract;
use outbe_gratispool::state::{commitment_hash, nullifier_hash, receiver_binding};
use outbe_gratispool::verifier::with_verifier_outcome;
use outbe_gratispool::SpendArgs;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::runtime::RequestArgs;
use crate::tests::common::*;

fn seed_oracle(storage: StorageHandle<'_>, rate_1e18: U256) {
    let mut oracle = OracleContract::new(storage);
    oracle.register_pair("COEN", "0xUSD").unwrap();
    oracle
        .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate_1e18, 0, 0)
        .unwrap();
}

/// Gives `account` a positive RCFI by recording a gratis cohort acquired one
/// year before the test's block time. The fidelity gate now lives in
/// `gratisfactory::pledge_gratis` (it requires `get_rcfi(caller) > 0`), so the
/// e2e flow must seed this before its pledge leg or `pledge_gratis` rejects.
/// The zero-RCFI rejection itself is asserted in the gratisfactory crate
/// (`pledge_rejects_zero_rcfi`).
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * 86_400;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

fn build_spend_args(
    storage: StorageHandle<'_>,
    nullifier_secret: U256,
    denom_id: u8,
    action_tag: u64,
    target: Address,
    nonce: U256,
) -> SpendArgs {
    let pool_state = GratisPoolContract::new(storage);
    SpendArgs {
        merkle_root: pool_state.current_root(denom_id).unwrap(),
        nullifier_hash: nullifier_hash(nullifier_secret).unwrap(),
        denom_id,
        receiver_binding: receiver_binding(action_tag, target, CHAIN_ID, nonce).unwrap(),
        proof: vec![0x00; 32], // dummy — verifier outcome forced via with_verifier_outcome
    }
}

#[test]
fn full_request_pay_reclaim_unpledge_flow() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let pledge_amount = DenomAmount::from_id(denom_id).unwrap().amount();

        // Mint Alice enough Gratis to pledge.
        Gratis::new(storage.clone())
            .mine(alice(), pledge_amount)
            .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());

        // Alice pre-computes two notes: pledge-side (spent at requestCredis)
        // and reclaim-side (spent at unpledgeGratis after payAnadosis).
        let pledge_secret = U256::from(0x1111u64);
        let pledge_null = U256::from(0x2222u64);
        let reclaim_secret = U256::from(0x3333u64);
        let reclaim_null = U256::from(0x4444u64);

        let pledge_commitment = commitment_hash(pledge_secret, pledge_null, denom_id).unwrap();
        let reclaim_commitment = commitment_hash(reclaim_secret, reclaim_null, denom_id).unwrap();

        // 1) Pledge: shielded deposit into the per-denomination pool.
        //    pledge_gratis returns the post-insert Merkle root, which we feed
        //    straight into the spend proof's public input below.
        let (pledge_root, _, _) =
            gf::pledge_gratis(storage.clone(), alice(), denom_id, pledge_commitment).unwrap();

        // 2) Request credis: Alice is also the bundleAccount in this test.
        //    Verifier outcome forced true since the proof bytes are dummy.
        let args = RequestArgs {
            merkle_root: pledge_root,
            nullifier_hash: nullifier_hash(pledge_null).unwrap(),
            denom_id,
            // requestCredis binds `reclaim_commitment` into the nonce slot.
            receiver_binding: receiver_binding(
                ACTION_REQUEST_CREDIS,
                alice(),
                CHAIN_ID,
                reclaim_commitment,
            )
            .unwrap(),
            proof: vec![0x00; 32],
            reclaim_commitment,
        };

        let (position_id, amount_stables) = with_verifier_outcome(true, || {
            runtime::request_credis(
                storage.clone(),
                alice(),
                asset(),
                vault(),
                alice(),
                args,
                CREATED_AT,
                BLOCK_NUMBER,
            )
            .unwrap()
        });

        // amount_stables = pledge_amount * 2e18 / (1e12 * 1e18) for rate 2.0.
        let expected_stables = pledge_amount * U256::from(2u64) * one_e18()
            / (U256::from(1_000_000_000_000u128) * one_e18());
        assert_eq!(amount_stables, expected_stables);

        // Position created with the right backing.
        let credis = CredisContract::new(storage.clone());
        let position = credis.get_position(position_id).unwrap();
        assert_eq!(position.bundle_account, alice());
        assert_eq!(position.total_anadosis_amount, amount_stables);
        assert_eq!(position.total_gratis_amount, pledge_amount);

        // 3) Pay all NUMBER_OF_ANADOSIS installments. Only the final one
        //    triggers the reclaim-commitment insert into the pool.
        let pre_pay_leaf_count = GratisPoolContract::new(storage.clone())
            .leaf_count(denom_id)
            .unwrap();
        for n in 1..=NUMBER_OF_ANADOSIS {
            runtime::pay_anadosis(
                storage.clone(),
                alice(),
                position_id,
                CREATED_AT + (n as u64) * SECONDS_PER_MONTH,
                BLOCK_NUMBER + n as u64,
            )
            .unwrap();
        }
        // Reclaim commitment landed exactly once.
        let post_pay_leaf_count = GratisPoolContract::new(storage.clone())
            .leaf_count(denom_id)
            .unwrap();
        assert_eq!(post_pay_leaf_count, pre_pay_leaf_count + 1);

        // 4) Unpledge using the reclaim secret. Goes through gratisfactory
        //    now (the pool itself no longer moves Gratis balances). The
        //    per-pledger ledger is keyed by depositor, so the destination
        //    must match the pledger in the current PoC; the shielded part
        //    of the design is the on-chain link between commitment and
        //    depositor, not the destination address.
        let unpledge_args = build_spend_args(
            storage.clone(),
            reclaim_null,
            denom_id,
            ACTION_UNPLEDGE,
            alice(),
            U256::ZERO,
        );
        let returned = with_verifier_outcome(true, || {
            outbe_gratisfactory::runtime::unpledge_gratis(storage.clone(), &unpledge_args, alice())
                .unwrap()
        });
        assert_eq!(returned, pledge_amount);

        // Alice received the full denomination back; escrow drained.
        let gratis = Gratis::new(storage.clone());
        assert_eq!(gratis.balance_of(alice()).unwrap(), pledge_amount);
        assert_eq!(gratis.pledged_of(alice()).unwrap(), U256::ZERO);
    });
}

#[test]
fn request_credis_rejects_overdue_anadosis() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let amount = DenomAmount::from_id(denom_id).unwrap().amount();
        Gratis::new(storage.clone())
            .mine(alice(), amount * U256::from(2u64))
            .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());

        let c1 = commitment_hash(U256::from(1u64), U256::from(2u64), denom_id).unwrap();
        let c2 = commitment_hash(U256::from(3u64), U256::from(4u64), denom_id).unwrap();
        let reclaim_c1 = commitment_hash(U256::from(5u64), U256::from(6u64), denom_id).unwrap();
        let reclaim_c2 = commitment_hash(U256::from(7u64), U256::from(8u64), denom_id).unwrap();

        // First pledge + request.
        let (root1, _, _) = gf::pledge_gratis(storage.clone(), alice(), denom_id, c1).unwrap();
        let args1 = RequestArgs {
            merkle_root: root1,
            nullifier_hash: nullifier_hash(U256::from(2u64)).unwrap(),
            denom_id,
            receiver_binding: receiver_binding(
                ACTION_REQUEST_CREDIS,
                alice(),
                CHAIN_ID,
                reclaim_c1,
            )
            .unwrap(),
            proof: vec![0u8; 32],
            reclaim_commitment: reclaim_c1,
        };
        with_verifier_outcome(true, || {
            runtime::request_credis(
                storage.clone(),
                alice(),
                asset(),
                vault(),
                alice(),
                args1,
                CREATED_AT,
                BLOCK_NUMBER,
            )
            .unwrap();
        });

        // Second pledge — then attempt a second request once anadosis-1 is
        // overdue on the first position.
        let (root2, _, _) = gf::pledge_gratis(storage.clone(), alice(), denom_id, c2).unwrap();
        let args2 = RequestArgs {
            merkle_root: root2,
            nullifier_hash: nullifier_hash(U256::from(4u64)).unwrap(),
            denom_id,
            receiver_binding: receiver_binding(
                ACTION_REQUEST_CREDIS,
                alice(),
                CHAIN_ID,
                reclaim_c2,
            )
            .unwrap(),
            proof: vec![0u8; 32],
            reclaim_commitment: reclaim_c2,
        };
        let err = with_verifier_outcome(true, || {
            runtime::request_credis(
                storage.clone(),
                alice(),
                asset(),
                vault(),
                alice(),
                args2,
                CREATED_AT + SECONDS_PER_MONTH + 1,
                BLOCK_NUMBER,
            )
            .unwrap_err()
        });
        assert!(err.to_string().contains("overdue"));
    });
}

// Zero-RCFI rejection is no longer a credisfactory concern: the fidelity gate
// moved to `gratisfactory::pledge_gratis`. The rejection is asserted in the
// gratisfactory crate (`pledge_rejects_zero_rcfi`).

#[test]
fn request_credis_rejects_zero_asset() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let args = RequestArgs {
            merkle_root: U256::ZERO,
            nullifier_hash: U256::ZERO,
            denom_id: 1,
            receiver_binding: U256::ZERO,
            proof: vec![],
            reclaim_commitment: U256::ZERO,
        };
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            Address::ZERO,
            vault(),
            alice(),
            args,
            CREATED_AT,
            BLOCK_NUMBER,
        )
        .unwrap_err();
        assert!(err.to_string().contains("asset"));
    });
}

#[test]
fn request_credis_rejects_zero_vault_provider() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let args = RequestArgs {
            merkle_root: U256::ZERO,
            nullifier_hash: U256::ZERO,
            denom_id: 1,
            receiver_binding: U256::ZERO,
            proof: vec![],
            reclaim_commitment: U256::ZERO,
        };
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            asset(),
            Address::ZERO,
            alice(),
            args,
            CREATED_AT,
            BLOCK_NUMBER,
        )
        .unwrap_err();
        assert!(err.to_string().contains("vault"));
    });
}

#[test]
fn request_credis_rejects_zero_bundle_account() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    StorageHandle::enter(&mut storage, |storage| {
        let args = RequestArgs {
            merkle_root: U256::ZERO,
            nullifier_hash: U256::ZERO,
            denom_id: 1,
            receiver_binding: U256::ZERO,
            proof: vec![],
            reclaim_commitment: U256::ZERO,
        };
        let err = runtime::request_credis(
            storage.clone(),
            alice(),
            asset(),
            vault(),
            Address::ZERO,
            args,
            CREATED_AT,
            BLOCK_NUMBER,
        )
        .unwrap_err();
        assert!(err.to_string().contains("bundle account"));
    });
}

#[test]
fn pay_anadosis_rejects_non_owner_caller() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let amount = DenomAmount::from_id(denom_id).unwrap().amount();
        Gratis::new(storage.clone()).mine(alice(), amount).unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());

        let pledge_c = commitment_hash(U256::from(11u64), U256::from(12u64), denom_id).unwrap();
        let reclaim_c = commitment_hash(U256::from(13u64), U256::from(14u64), denom_id).unwrap();
        let (pledge_root, _, _) =
            gf::pledge_gratis(storage.clone(), alice(), denom_id, pledge_c).unwrap();

        let args = RequestArgs {
            merkle_root: pledge_root,
            nullifier_hash: nullifier_hash(U256::from(12u64)).unwrap(),
            denom_id,
            receiver_binding: receiver_binding(ACTION_REQUEST_CREDIS, alice(), CHAIN_ID, reclaim_c)
                .unwrap(),
            proof: vec![0u8; 32],
            reclaim_commitment: reclaim_c,
        };
        let (position_id, _) = with_verifier_outcome(true, || {
            runtime::request_credis(
                storage.clone(),
                alice(),
                asset(),
                vault(),
                alice(),
                args,
                CREATED_AT,
                BLOCK_NUMBER,
            )
            .unwrap()
        });

        // bob is not the bundleAccount on this position.
        let err = runtime::pay_anadosis(
            storage.clone(),
            bob(),
            position_id,
            CREATED_AT + SECONDS_PER_MONTH,
            BLOCK_NUMBER + 1,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("bundleAccount"),
            "expected bundleAccount-mismatch error, got: {err}",
        );
    });
}

/// Reclaim-swap attack closure.
///
/// Honest user generates a proof bound to `reclaim_user`, submits
/// `RequestArgs` with that commitment. Mempool attacker copies the bytes
/// and substitutes their own `reclaim_attacker` — the runtime must
/// reject with `ReceiverBindingMismatch` because it now hashes the
/// substituted commitment into the binding and the result no longer
/// matches the prover's `receiver_binding`. Without binding
/// `reclaim_commitment` into the nonce slot this attack would land the
/// credis position against `bundleAccount` (as intended) but with the
/// attacker's reclaim leg, letting them later unpledge and drain the
/// gratis collateral.
#[test]
fn request_credis_rejects_swapped_reclaim_commitment() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage.set_block_number(BLOCK_NUMBER);
    storage.enable_sub_call_stub();
    StorageHandle::enter(&mut storage, |storage| {
        let denom_id: u8 = 1;
        let pledge_amount = DenomAmount::from_id(denom_id).unwrap().amount();

        Gratis::new(storage.clone())
            .mine(alice(), pledge_amount)
            .unwrap();
        seed_fidelity(storage.clone(), alice());
        seed_oracle(storage.clone(), U256::from(2u64) * one_e18());

        let pledge_secret = U256::from(0xA1u64);
        let pledge_null = U256::from(0xA2u64);
        let pledge_commitment = commitment_hash(pledge_secret, pledge_null, denom_id).unwrap();
        let (pledge_root, _, _) =
            gf::pledge_gratis(storage.clone(), alice(), denom_id, pledge_commitment).unwrap();

        // The honest user's reclaim commitment — what the proof actually
        // binds into `receiver_binding`.
        let reclaim_user =
            commitment_hash(U256::from(0xB1u64), U256::from(0xB2u64), denom_id).unwrap();
        // The attacker's reclaim commitment — what they substitute in the
        // intercepted `RequestArgs` after copying the proof bytes.
        let reclaim_attacker =
            commitment_hash(U256::from(0xC1u64), U256::from(0xC2u64), denom_id).unwrap();
        assert_ne!(reclaim_user, reclaim_attacker);

        let binding_user =
            receiver_binding(ACTION_REQUEST_CREDIS, alice(), CHAIN_ID, reclaim_user).unwrap();

        // Swapped args: proof's binding still names `reclaim_user`, but
        // `args.reclaim_commitment` is the attacker's. The runtime hashes
        // the *args* value into its recomputed binding, which now diverges.
        let args = RequestArgs {
            merkle_root: pledge_root,
            nullifier_hash: nullifier_hash(pledge_null).unwrap(),
            denom_id,
            receiver_binding: binding_user,
            proof: vec![0u8; 32],
            reclaim_commitment: reclaim_attacker,
        };
        let err = with_verifier_outcome(true, || {
            runtime::request_credis(
                storage.clone(),
                alice(),
                asset(),
                vault(),
                alice(),
                args,
                CREATED_AT,
                BLOCK_NUMBER,
            )
            .unwrap_err()
        });
        assert!(
            err.to_string().contains("receiver binding"),
            "swapped reclaim_commitment must reject with ReceiverBindingMismatch, got: {err}",
        );
    });
}

/// Anchor: the denomination ladder length matches the constant the
/// rewired e2e tests assume.
#[test]
fn denomination_ladder_count() {
    assert_eq!(DENOMINATION_COUNT, 5);
}

/// Sanity check: the commitment-hash helper is deterministic for the same
/// inputs (used as the position-id input in `create_position`, so a
/// regression would break replay).
#[test]
fn commitment_hash_deterministic() {
    let h1 = commitment_hash(U256::from(1u64), U256::from(2u64), 1).unwrap();
    let h2 = commitment_hash(U256::from(1u64), U256::from(2u64), 1).unwrap();
    let h3 = commitment_hash(U256::from(1u64), U256::from(2u64), 2).unwrap();
    assert_eq!(h1, h2);
    assert_ne!(h1, h3);
    // Anchor against an unrelated keccak so a change in the Poseidon
    // instance is caught visually if someone swaps it out.
    let stray = U256::from_be_bytes(keccak256(b"unrelated").0);
    assert_ne!(h1, stray);
}
