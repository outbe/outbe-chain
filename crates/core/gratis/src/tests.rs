//! Confidential Gratis tests driven by the in-process enclave stand-in
//! (`enclave_client::test_enclave`), which runs the real enclave engine against a
//! fixed dev state key. Balances are asserted by decrypting the ciphertext with
//! the account's view key exactly as a client would.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{FidelityCohortOp, FidelityOpSection, GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
    pledge_secret, spend_auth_mac,
};

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
fn smart_account() -> Address {
    address!("0x2222222222222222222222222222222222222222")
}
fn asset() -> Address {
    address!("0x3333333333333333333333333333333333333333")
}

/// The oracle-derived loan terms the gratisfactory seals into a pledge ticket.
/// Stables and gratis are deliberately different numbers so a unit mix-up shows up.
fn terms(stables: U256, gratis: U256) -> api::PledgeTerms {
    api::PledgeTerms {
        stables_amount: stables,
        gratis_amount: gratis,
        asset: asset(),
        entry_price: stables * U256::from(1_000_000u64) / gratis,
        issuance_currency: 840,
        asset_decimals: 6,
        valuation_price: stables * outbe_primitives::units::SCALE_1E18 / gratis,
    }
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

fn view_pledged(storage: StorageHandle<'_>, account: Address) -> U256 {
    let sk = test_enclave::state_key();
    let vk = derive_view_key(&sk, account).unwrap();
    let blob = api::pledged_ct(storage, account).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, account, &blob).unwrap()
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
        assert_eq!(api::total_supply(storage.clone()).unwrap(), amount);
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
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(1500u64)
        );
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
        assert_eq!(api::total_supply(storage).unwrap(), one_gratis);
    });
}

#[test]
fn mine_rejects_replayed_op_nonce() {
    with_env(|storage| {
        let amount = U256::from(100u64);
        let a = auth(GratisOp::Mint, alice(), amount, 0);
        api::mint(storage.clone(), alice(), amount, a.clone()).unwrap();
        // Replaying the same (amount, nonce=0, mac) must fail - nonce advanced to 1.
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
fn burn_reduces_balance_and_supply() {
    with_env(|storage| {
        api::mint(
            storage.clone(),
            alice(),
            U256::from(1000u64),
            auth(GratisOp::Mint, alice(), U256::from(1000u64), 0),
        )
        .unwrap();
        let remaining = api::burn(
            storage.clone(),
            alice(),
            U256::from(400u64),
            auth(GratisOp::Burn, alice(), U256::from(400u64), 1),
        )
        .unwrap();
        assert_eq!(remaining, U256::from(600u64));
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(600u64));
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(600u64)
        );
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

#[test]
fn pledge_consume_and_settle_flow() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let stables = U256::from(500u64);
        let sk = test_enclave::state_key();
        // Mine + pledge: the pledger asks for `stables` of credit, the gratis it costs
        // is drained from the balance and parked in the ticket (pledged_ct still 0),
        // and pledged_total - a GRATIS aggregate - counts the gratis, not the stables.
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let handle = pledge(
            storage.clone(),
            alice(),
            stables,
            terms(stables, amount),
            auth(GratisOp::Pledge, alice(), stables, 1),
        )
        .unwrap();
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), amount);

        // requestCredis from a distinct smart account: alice derives the pledge
        // secret from her modify key + the public handle and binds it to `smart_account`.
        // The collateral is credited into alice's OWN pledged ledger (no escrow) and
        // the ticket is deleted; pledged_total is unchanged.
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), smart_account());
        let (consumed_terms, collateral_id) =
            consume(storage.clone(), handle, smart_account(), spend).unwrap();
        assert_eq!(
            consumed_terms,
            terms(stables, amount),
            "credis reads the pledge-time quote back out of the ticket"
        );
        assert_eq!(view_pledged(storage.clone(), alice()), amount);
        assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), amount);
        // Re-consuming the now-deleted ticket is rejected.
        assert!(consume(storage.clone(), handle, smart_account(), spend).is_err());

        // Ten settlements: each releases 1/10 from alice's pledged ledger back to
        // her balance.
        let per = amount / U256::from(10u64);
        for _ in 0..10 {
            collateral(
                storage.clone(),
                handle,
                collateral_id,
                per,
                view_pledged(storage.clone(), alice()),
                api::CollateralAction::Return,
            )
            .unwrap();
        }
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
        // A further release rejected - pledged ledger is empty.
        assert!(collateral(
            storage.clone(),
            handle,
            collateral_id,
            per,
            view_pledged(storage.clone(), alice()),
            api::CollateralAction::Return
        )
        .is_err());
    });
}

#[test]
fn burn_pledged_reduces_supply_and_pledged() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let sk = test_enclave::state_key();
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let stables = U256::from(500u64);
        let handle = pledge(
            storage.clone(),
            alice(),
            stables,
            terms(stables, amount),
            auth(GratisOp::Pledge, alice(), stables, 1),
        )
        .unwrap();
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), smart_account());
        let (_, collateral_id) = consume(storage.clone(), handle, smart_account(), spend).unwrap();

        // Release across 3 settlements (300), leaving 700 outstanding, then burn it.
        let per = amount / U256::from(10u64);
        for _ in 0..3 {
            collateral(
                storage.clone(),
                handle,
                collateral_id,
                per,
                view_pledged(storage.clone(), alice()),
                api::CollateralAction::Return,
            )
            .unwrap();
        }
        let outstanding = U256::from(700u64);
        let burned = collateral(
            storage.clone(),
            handle,
            collateral_id,
            outstanding,
            outstanding,
            api::CollateralAction::Burn,
        )
        .unwrap();
        assert_eq!(burned, outstanding);
        assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
        // total_supply drops by the burned collateral; the 300 released stays liquid.
        assert_eq!(
            api::total_supply(storage.clone()).unwrap(),
            U256::from(300u64)
        );
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn direct_unpledge_returns_collateral_and_blocks_credis() {
    with_env(|storage| {
        let amount = U256::from(1000u64);
        let sk = test_enclave::state_key();
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let stables = U256::from(500u64);
        let handle = pledge(
            storage.clone(),
            alice(),
            stables,
            terms(stables, amount),
            auth(GratisOp::Pledge, alice(), stables, 1),
        )
        .unwrap();

        // Credis rejected -> direct unpledge is quoted in the same unit as the pledge
        // (stables in) and returns the whole gratis collateral.
        let returned = api::unpledge(
            storage.clone(),
            alice(),
            stables,
            handle,
            auth(GratisOp::Unpledge, alice(), stables, 2),
        )
        .unwrap();
        assert_eq!(returned, amount);
        assert_eq!(view_balance(storage.clone(), alice()), amount);
        assert_eq!(
            api::pledged_total_supply(storage.clone()).unwrap(),
            U256::ZERO
        );

        // The deleted ticket can no longer be consumed for credis.
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), smart_account());
        assert!(consume(storage.clone(), handle, smart_account(), spend).is_err());
    });
}

#[test]
fn pledge_validity_preserves_expired_cancellation_and_prevents_replay() {
    // Creation just before an eight-hour boundary must not shorten the 900-second lifetime.
    const CREATED: u64 = 28_799;
    for (now, rejected) in [
        (U256::from(CREATED - 1), Some("pledge note not yet valid")),
        (U256::from(CREATED + 899), None),
        (U256::from(CREATED + 900), None),
        (U256::from(CREATED + 901), Some("pledge note expired")),
        (
            U256::from(u64::MAX) + U256::ONE,
            Some("pledge timestamp exceeds u64"),
        ),
    ] {
        with_env(|storage| {
            storage.set_block_timestamp(U256::from(CREATED)).unwrap();
            let amount = U256::from(1000);
            let stables = U256::from(500);
            api::mint(
                storage.clone(),
                alice(),
                amount,
                auth(GratisOp::Mint, alice(), amount, 0),
            )
            .unwrap();
            let accepted_terms = terms(stables, amount);
            let note = pledge(
                storage.clone(),
                alice(),
                stables,
                accepted_terms,
                auth(GratisOp::Pledge, alice(), stables, 1),
            )
            .unwrap();
            let gratis = crate::Gratis::new(storage.clone());
            let ticket = gratis.journal_head.read().unwrap();
            let balance = view_balance(storage.clone(), alice());
            let mk = derive_modify_key(&test_enclave::state_key(), alice()).unwrap();
            let spend = spend_auth_mac(&pledge_secret(&mk, note), smart_account());
            storage.set_block_timestamp(now).unwrap();
            let result = consume(storage.clone(), note, smart_account(), spend);
            assert_eq!(api::total_supply(storage.clone()).unwrap(), amount);
            assert_eq!(api::pledged_total_supply(storage.clone()).unwrap(), amount);
            assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 2);
            assert_eq!(view_balance(storage.clone(), alice()), balance);

            if let Some(reason) = rejected {
                assert!(result.unwrap_err().to_string().contains(reason));
                assert_eq!(gratis.journal_head.read().unwrap(), ticket);
                assert_eq!(view_pledged(storage.clone(), alice()), U256::ZERO);
                // Cancellation remains possible even when the consume clock is invalid/expired.
                assert_eq!(
                    api::unpledge(
                        storage.clone(),
                        alice(),
                        stables,
                        note,
                        auth(GratisOp::Unpledge, alice(), stables, 2)
                    )
                    .unwrap(),
                    amount
                );
                assert_eq!(view_balance(storage.clone(), alice()), amount);
                assert_eq!(
                    api::pledged_total_supply(storage.clone()).unwrap(),
                    U256::ZERO
                );
                // A fresh nonce cannot replay cancellation of a deleted ticket.
                assert!(api::unpledge(
                    storage.clone(),
                    alice(),
                    stables,
                    note,
                    auth(GratisOp::Unpledge, alice(), stables, 3)
                )
                .is_err());
            } else {
                assert_eq!(result.unwrap().0, accepted_terms);
                assert_eq!(view_pledged(storage.clone(), alice()), amount);
                assert!(api::unpledge(
                    storage.clone(),
                    alice(),
                    stables,
                    note,
                    auth(GratisOp::Unpledge, alice(), stables, 2)
                )
                .is_err());
            }
            assert!(consume(storage.clone(), note, smart_account(), spend).is_err());
        });
    }
}

#[test]
fn pledge_rejects_unrepresentable_timestamps_without_locking_value() {
    with_env(|storage| {
        let amount = U256::from(1000);
        let stables = U256::from(500);
        api::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();
        let before = api::balance_ct(storage.clone(), alice()).unwrap();
        for now in [U256::from(u64::MAX), U256::from(u64::MAX) + U256::ONE] {
            storage.set_block_timestamp(now).unwrap();
            let error = pledge(
                storage.clone(),
                alice(),
                stables,
                terms(stables, amount),
                auth(GratisOp::Pledge, alice(), stables, 1),
            )
            .unwrap_err();
            assert!(error.to_string().contains("pledge timestamp"));
            assert_eq!(api::balance_ct(storage.clone(), alice()).unwrap(), before);
            assert_eq!(api::total_supply(storage.clone()).unwrap(), amount);
            assert_eq!(
                api::pledged_total_supply(storage.clone()).unwrap(),
                U256::ZERO
            );
            assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 1);
        }
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
            to: smart_account(),
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
    // The returned bytes are the ciphertext blob; decrypt with the view key.
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
        // WHOLE op when the section fails - so nothing is committed.
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
            err.to_string()
                .contains("fidelity section must use journal state"),
            "expected a fidelity-section rejection, got: {err}"
        );

        // Atomic revert: no gratis state was written (balance, supply, op_nonce).
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        assert_eq!(api::total_supply(storage.clone()).unwrap(), U256::ZERO);
        assert_eq!(api::op_nonce(storage.clone(), alice()).unwrap(), 0);
    });
}

fn pledge(
    storage: StorageHandle<'_>,
    owner: Address,
    amount: U256,
    terms: api::PledgeTerms,
    auth: ModifyAuth,
) -> outbe_primitives::error::Result<B256> {
    let reply = api::pledge(storage, owner, amount, terms, auth)?;
    let view = derive_view_key(&test_enclave::state_key(), owner).unwrap();
    Ok(outbe_tee::confidential::decrypt_pledge_reply(&view, &reply).unwrap())
}
fn consume(
    storage: StorageHandle<'_>,
    note: B256,
    account: Address,
    spend: [u8; 32],
) -> outbe_primitives::error::Result<(api::PledgeTerms, B256)> {
    let credential = outbe_tee::confidential::encrypt_pledge_credential(
        &outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::CREDENTIAL_SECRET),
        chain_b256(),
        note,
        account,
        spend,
    )
    .unwrap();
    api::consume_pledge(storage, U256::from_be_bytes(note.0), credential, account)
}
fn collateral(
    storage: StorageHandle<'_>,
    note: B256,
    id: B256,
    amount: U256,
    expected_remaining: U256,
    action: api::CollateralAction,
) -> outbe_primitives::error::Result<U256> {
    api::apply_collateral(
        storage,
        api::CollateralAuthorization {
            credis_id: U256::from_be_bytes(note.0),
            collateral_id: id,
            amount,
            expected_remaining,
            action,
        },
        0,
    )
}

#[test]
fn allocations_bind_credis_cap_returns_and_reject_replay_after_restart() {
    with_env(|storage| {
        let total = U256::from(1000);
        api::mint(
            storage.clone(),
            alice(),
            total,
            auth(GratisOp::Mint, alice(), total, 0),
        )
        .unwrap();
        let allocate = |amount: u64, nonce| {
            let amount = U256::from(amount);
            let note = pledge(
                storage.clone(),
                alice(),
                amount,
                terms(amount, amount),
                auth(GratisOp::Pledge, alice(), amount, nonce),
            )
            .unwrap();
            let key = derive_modify_key(&test_enclave::state_key(), alice()).unwrap();
            let (_, id) = consume(
                storage.clone(),
                note,
                smart_account(),
                spend_auth_mac(&pledge_secret(&key, note), smart_account()),
            )
            .unwrap();
            (note, id)
        };
        let (a, aid) = allocate(100, 1);
        let (b, bid) = allocate(900, 2);
        assert_eq!(view_pledged(storage.clone(), alice()), total);
        let before = crate::Gratis::new(storage.clone())
            .journal_head
            .read()
            .unwrap();
        assert!(collateral(
            storage.clone(),
            a,
            bid,
            U256::from(20),
            U256::from(900),
            api::CollateralAction::Return
        )
        .unwrap_err()
        .to_string()
        .contains("binding"));
        assert!(collateral(
            storage.clone(),
            a,
            aid,
            U256::from(150),
            U256::from(100),
            api::CollateralAction::Return
        )
        .unwrap_err()
        .to_string()
        .contains("allocation exceeded"));
        assert_eq!(
            crate::Gratis::new(storage.clone())
                .journal_head
                .read()
                .unwrap(),
            before
        );
        let alice_before = api::balance_ct(storage.clone(), alice()).unwrap();
        let other_before = api::balance_ct(storage.clone(), smart_account()).unwrap();
        collateral(
            storage.clone(),
            a,
            aid,
            U256::from(20),
            U256::from(100),
            api::CollateralAction::Return,
        )
        .unwrap();
        assert!(collateral(
            storage.clone(),
            a,
            aid,
            U256::from(20),
            U256::from(100),
            api::CollateralAction::Return
        )
        .unwrap_err()
        .to_string()
        .contains("stale"));
        // Every account's view changes with the global root, including accounts
        // with no state. Ciphertext comparison cannot identify the updated source.
        assert_ne!(
            api::balance_ct(storage.clone(), alice()).unwrap(),
            alice_before
        );
        assert_ne!(
            api::balance_ct(storage.clone(), smart_account()).unwrap(),
            other_before
        );
        assert_eq!(other_before.len(), alice_before.len());
        collateral(
            storage.clone(),
            a,
            aid,
            U256::from(80),
            U256::from(80),
            api::CollateralAction::Return,
        )
        .unwrap();
        outbe_tee_enclave::confidential_ledger::clear_cache();
        assert!(collateral(
            storage.clone(),
            a,
            aid,
            U256::ONE,
            U256::ZERO,
            api::CollateralAction::Return
        )
        .is_err());
        assert_eq!(view_balance(storage.clone(), alice()), U256::from(100));
        assert_eq!(view_pledged(storage.clone(), alice()), U256::from(900));
        collateral(
            storage.clone(),
            b,
            bid,
            U256::from(900),
            U256::from(900),
            api::CollateralAction::Burn,
        )
        .unwrap();
        assert_eq!(api::total_supply(storage.clone()).unwrap(), U256::from(100));
        assert_eq!(view_pledged(storage, alice()), U256::ZERO);
    });
}

#[test]
fn reverted_candidate_and_stale_response_cannot_change_committed_state() {
    with_env(|storage| {
        use outbe_tee::confidential::{head, persist, Call, Domain};
        let amount = U256::from(100);
        let request = || {
            outbe_tee_enclave::confidential_ledger::empty_request(
                GratisOp::Mint,
                chain_b256(),
                alice(),
                amount,
            )
        };
        let mut input = request();
        input.modify_auth = auth(GratisOp::Mint, alice(), amount, 0);
        let prepared =
            crate::enclave_client::execute(&storage, Call::Gratis(Box::new(input))).unwrap();
        let original = head(&storage, Domain::Gratis).unwrap();
        let result: outbe_primitives::error::Result<()> = storage.with_checkpoint(|| {
            api::mint(
                storage.clone(),
                alice(),
                amount,
                auth(GratisOp::Mint, alice(), amount, 0),
            )?;
            assert_eq!(view_balance(storage.clone(), alice()), amount);
            Err(outbe_primitives::error::PrecompileError::Revert(
                "later token call failed".into(),
            ))
        });
        assert!(result.is_err());
        assert_eq!(head(&storage, Domain::Gratis).unwrap(), original);
        assert_eq!(view_balance(storage.clone(), alice()), U256::ZERO);
        // A different transaction from the same prior head must have different
        // ciphertext/AEAD key material, even though its journal index is reused.
        api::mint(
            storage.clone(),
            alice(),
            U256::from(200),
            auth(GratisOp::Mint, alice(), U256::from(200), 0),
        )
        .unwrap();
        assert!(persist(&storage, &prepared.updates[0]).is_err());
        let committed = crate::Gratis::new(storage.clone())
            .journal_records
            .get_bytes(&0)
            .read()
            .unwrap();
        assert_ne!(committed, prepared.updates[0].record);
        assert_eq!(view_balance(storage, alice()), U256::from(200));
    });
}

#[test]
fn collateral_storage_access_trace_is_independent_of_source() {
    use outbe_tee::confidential::{CollateralAction, CollateralAuthorization};
    let run = |owner: Address, action| {
        test_enclave::install();
        outbe_tee_enclave::confidential_ledger::clear_cache();
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let id = StorageHandle::enter(&mut provider, |storage| {
            let amount = U256::from(100);
            api::mint(
                storage.clone(),
                owner,
                amount,
                auth(GratisOp::Mint, owner, amount, 0),
            )
            .unwrap();
            let note = pledge(
                storage.clone(),
                owner,
                amount,
                terms(amount, amount),
                auth(GratisOp::Pledge, owner, amount, 1),
            )
            .unwrap();
            let key = derive_modify_key(&test_enclave::state_key(), owner).unwrap();
            let spend = spend_auth_mac(&pledge_secret(&key, note), smart_account());
            let credential = outbe_tee::confidential::encrypt_pledge_credential(
                &outbe_tee_enclave::crypto::x25519_public(
                    &outbe_tee_enclave::dev::CREDENTIAL_SECRET,
                ),
                chain_b256(),
                note,
                smart_account(),
                spend,
            )
            .unwrap();
            api::consume_pledge(storage, U256::ONE, credential, smart_account())
                .unwrap()
                .1
        });
        provider.enable_storage_trace();
        StorageHandle::enter(&mut provider, |storage| {
            api::apply_collateral(
                storage,
                CollateralAuthorization {
                    credis_id: U256::ONE,
                    collateral_id: id,
                    action,
                    amount: U256::from(20),
                    expected_remaining: U256::from(100),
                },
                0,
            )
            .unwrap();
        });
        provider.storage_trace().to_vec()
    };
    for action in [CollateralAction::Return, CollateralAction::Burn] {
        assert_eq!(
            run(alice(), action),
            run(Address::repeat_byte(0x77), action)
        );
    }
}
