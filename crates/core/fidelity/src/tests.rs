//! Fidelity crate tests over the confidential (enclave-backed) path.
//!
//! The enclave engine pins the deep RCFI/cohort math
//! (`outbe-tee-enclave`'s `fidelity` module: LIFO split, golden decay.py
//! replay, blob padding). Here we test the on-chain orchestration:
//! - cohort ops persist encrypted blobs
//! - the global anchor is set once
//! - leagues come back from the snapshot
//! - the signed-auth query path decrypts only for the owner

use alloy_primitives::{address, Address, U256};
use k256::ecdsa::signature::hazmat::PrehashSigner;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::enclave_client::test_enclave;
use crate::schema::FidelityContract;
use crate::{api, MAX_LEAGUE, MIN_LEAGUE};

const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BOB: Address = address!("0x2222222222222222222222222222222222222222");
use outbe_primitives::time::SECONDS_PER_DAY as DAY;
const T0: u64 = 1_000_000;

/// Run `f` in a fresh storage scope on the test-enclave chain, with the
/// in-process fidelity enclave installed.
fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    test_enclave::install();
    let mut storage = HashMapStorageProvider::new(test_enclave::DEV_CHAIN_ID);
    let out = StorageHandle::enter(&mut storage, |storage| f(storage.clone()));
    test_enclave::uninstall();
    out
}

#[test]
fn cohort_in_encrypts_and_sets_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No state yet: empty blob, unset anchor.
        assert!(c.cohorts_ct_of(ALICE).unwrap().is_empty());
        assert_eq!(c.first_qualified_start().unwrap(), 0);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();

        // Blob is now non-empty ciphertext, and the global anchor is set to the
        // first acquisition UTC day.
        let blob = c.cohorts_ct_of(ALICE).unwrap();
        assert!(!blob.is_empty());
        let expected_day = outbe_primitives::time::utc_day_from_timestamp(T0);
        assert_eq!(c.first_qualified_start().unwrap(), expected_day);

        // A later acquisition by a different owner does NOT move the set-once
        // anchor.
        api::cohort_in(storage.clone(), BOB, U256::from(5u64), T0 + 100 * DAY).unwrap();
        assert_eq!(c.first_qualified_start().unwrap(), expected_day);
    });
}

#[test]
fn zero_amount_cohort_op_is_a_noop() {
    with_env(|storage| {
        api::cohort_in(storage.clone(), ALICE, U256::ZERO, T0).unwrap();
        let c = FidelityContract::new(storage.clone());
        assert!(c.cohorts_ct_of(ALICE).unwrap().is_empty());
        assert_eq!(c.first_qualified_start().unwrap(), 0);
    });
}

#[test]
fn league_reflects_holding_and_sale() {
    with_env(|storage| {
        // Sole holder, no sales -> top league at a later time.
        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        let league = api::league_at(storage.clone(), ALICE, T0 + 100 * DAY).unwrap();
        assert_eq!(league, MAX_LEAGUE);

        // Selling most of the position drops efficiency -> league falls.
        api::cohort_out(storage.clone(), ALICE, U256::from(900u64), T0 + 100 * DAY).unwrap();
        let after = api::league_at(storage.clone(), ALICE, T0 + 200 * DAY).unwrap();
        assert!(after < MAX_LEAGUE);

        // An owner with no cohorts is at the floor.
        assert_eq!(
            api::league_at(storage.clone(), BOB, T0 + 200 * DAY).unwrap(),
            MIN_LEAGUE
        );
    });
}

#[test]
fn snapshot_batches_owner_leagues_in_order() {
    with_env(|storage| {
        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        // Bob acquires then sells everything -> low efficiency.
        api::cohort_in(storage.clone(), BOB, U256::from(1_000u64), T0).unwrap();
        api::cohort_out(storage.clone(), BOB, U256::from(1_000u64), T0 + 10 * DAY).unwrap();

        let leagues =
            api::snapshot_leagues(storage.clone(), T0 + 100 * DAY, &[ALICE, BOB]).unwrap();
        assert_eq!(leagues.len(), 2);
        assert_eq!(leagues[0], (ALICE, MAX_LEAGUE));
        assert_eq!(leagues[1].0, BOB);
        assert!(leagues[1].1 <= MAX_LEAGUE);
    });
}

/// secp256k1 signer + its EVM address.
fn evm_signer(seed: u8) -> (k256::ecdsa::SigningKey, Address) {
    let sk = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
    let point = sk.verifying_key().to_encoded_point(false);
    let addr = Address::from_slice(&alloy_primitives::keccak256(&point.as_bytes()[1..])[12..]);
    (sk, addr)
}

/// EIP-191 owner authorization over the query-auth message for the test chain.
fn query_auth(sk: &k256::ecdsa::SigningKey, account: Address, expiry: u64) -> Vec<u8> {
    let msg = outbe_tee::protocol::fidelity_query_auth_message(
        test_enclave::dev_chain(),
        account,
        expiry,
    );
    let prehash = outbe_tee::protocol::eip191_hash(&msg);
    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        sk.sign_prehash(prehash.as_slice()).unwrap();
    let mut sig65 = [0u8; 65];
    sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
    sig65[64] = recid.to_byte();
    sig65.to_vec()
}

#[test]
fn signed_query_returns_index_for_owner_and_rejects_others() {
    with_env(|storage| {
        let (sk, account) = evm_signer(0x33);
        api::cohort_in(storage.clone(), account, U256::from(1_000u64), T0).unwrap();

        let c = FidelityContract::new(storage.clone());
        let expiry = T0 + 400 * DAY;

        // Owner-signed authorization -> the enclave returns a positive RCFI.
        let sig = query_auth(&sk, account, expiry);
        let result = c
            .query_index_at(account, T0 + 100 * DAY, expiry, sig)
            .unwrap();
        assert!(result.rcfi > U256::ZERO);
        assert_eq!(result.league, MAX_LEAGUE);

        // A different key signing for `account` is rejected (wrong signer).
        let (other, _) = evm_signer(0x34);
        let forged = query_auth(&other, account, expiry);
        assert!(c
            .query_index_at(account, T0 + 100 * DAY, expiry, forged)
            .is_err());
    });
}

#[test]
fn max_rcfi_at_uses_plaintext_anchor() {
    with_env(|storage| {
        let c = FidelityContract::new(storage.clone());
        // No anchor yet -> zero ceiling.
        assert_eq!(c.max_rcfi_at(T0).unwrap(), U256::ZERO);

        api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
        // After qualification, the ceiling grows with elapsed time (pure on-chain
        // t_dec of the plaintext anchor - no enclave).
        let early = c.max_rcfi_at(T0 + 10 * DAY).unwrap();
        let later = c.max_rcfi_at(T0 + 100 * DAY).unwrap();
        assert!(later > early);
    });
}

#[test]
fn cohort_ciphertext_is_deterministic_across_executions() {
    // The consensus invariant: two independent executions of the SAME sequence
    // of cohort ops produce BYTE-IDENTICAL ciphertext (and league). Thus every
    // validator converges on identical encrypted state (deterministic nonce, no
    // randomness).
    let run = || {
        with_env(|storage| {
            api::cohort_in(storage.clone(), ALICE, U256::from(1_000u64), T0).unwrap();
            api::cohort_in(storage.clone(), ALICE, U256::from(500u64), T0 + 10 * DAY).unwrap();
            api::cohort_out(storage.clone(), ALICE, U256::from(300u64), T0 + 20 * DAY).unwrap();
            let blob = FidelityContract::new(storage.clone())
                .cohorts_ct_of(ALICE)
                .unwrap();
            let league = api::league_at(storage.clone(), ALICE, T0 + 100 * DAY).unwrap();
            (blob, league)
        })
    };
    let a = run();
    let b = run();
    assert!(!a.0.is_empty());
    assert_eq!(
        a.0, b.0,
        "cohort ciphertext must be byte-identical across runs"
    );
    assert_eq!(a.1, b.1, "league must be identical across runs");
}

#[test]
fn precompile_dispatch_query_auth_and_metadata() {
    use crate::precompile::{dispatch, IFidelity};
    use alloy_primitives::Bytes;
    use alloy_sol_types::{SolCall, SolInterface};

    with_env(|storage| {
        let (sk, account) = evm_signer(0x77);
        api::cohort_in(storage.clone(), account, U256::from(1_000u64), T0).unwrap();
        let expiry = T0 + 400 * DAY;
        let query_ts = T0 + 100 * DAY;

        // getFidelityIndexAt through the ABI dispatch with a valid owner
        // authorization -> decoded RCFI is positive.
        let sig = query_auth(&sk, account, expiry);
        let call =
            IFidelity::IFidelityCalls::getFidelityIndexAt(IFidelity::getFidelityIndexAtCall {
                account,
                timestamp: query_ts,
                expiry,
                signature: Bytes::from(sig),
            })
            .abi_encode();
        let out = dispatch(storage.clone(), &call, Address::ZERO, U256::ZERO).unwrap();
        let rcfi = IFidelity::getFidelityIndexAtCall::abi_decode_returns(&out).unwrap();
        assert!(rcfi > U256::ZERO);

        // A signature from a different key is rejected by the dispatch.
        let (other, _) = evm_signer(0x78);
        let forged = query_auth(&other, account, expiry);
        let bad =
            IFidelity::IFidelityCalls::getFidelityIndexAt(IFidelity::getFidelityIndexAtCall {
                account,
                timestamp: query_ts,
                expiry,
                signature: Bytes::from(forged),
            })
            .abi_encode();
        // ...and surfaced as a RevertBytes carrying the reason (not an opaque Fatal,
        // which eth_call drops as data-less "missing revert data").
        let err = dispatch(storage.clone(), &bad, Address::ZERO, U256::ZERO).unwrap_err();
        assert!(
            matches!(
                &err,
                outbe_primitives::error::PrecompileError::RevertBytes(bytes)
                    if bytes.starts_with(&<IFidelity::EnclaveQueryError as alloy_sol_types::SolError>::SELECTOR)
            ),
            "query failure must surface as EnclaveQueryError RevertBytes, got {err:?}"
        );

        // Plaintext metadata needs no authorization.
        let min_call =
            IFidelity::IFidelityCalls::minLeague(IFidelity::minLeagueCall {}).abi_encode();
        let out = dispatch(storage.clone(), &min_call, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IFidelity::minLeagueCall::abi_decode_returns(&out).unwrap(),
            MIN_LEAGUE
        );
        let max_call =
            IFidelity::IFidelityCalls::maxLeague(IFidelity::maxLeagueCall {}).abi_encode();
        let out = dispatch(storage.clone(), &max_call, Address::ZERO, U256::ZERO).unwrap();
        assert_eq!(
            IFidelity::maxLeagueCall::abi_decode_returns(&out).unwrap(),
            MAX_LEAGUE
        );
    });
}

mod enclave_responses {
    use alloy_primitives::{Address, B256, U256};
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_primitives::error::{PrecompileError, Result};
    use outbe_tee::protocol::{
        fidelity_cohort_attestation_preimage, fidelity_query_attestation_preimage,
        fidelity_snapshot_attestation_preimage, EnclaveResponse, FidelityCohortResult,
        FidelityLeagueEntry, FidelityOpOutcome, FidelityQueryResult,
    };

    use crate::enclave_client::{validate_answer, LeagueSnapshot};

    const HASH: B256 = B256::repeat_byte(7);
    const OTHER_HASH: B256 = B256::repeat_byte(8);

    fn signer() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    fn attestation_pub() -> [u8; 32] {
        signer().verifying_key().to_bytes()
    }

    fn fatal<T: core::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(PrecompileError::EnclaveUnavailable(message)) => message,
            other => panic!("expected an EnclaveUnavailable error, got {other:?}"),
        }
    }

    fn enclave_error() -> EnclaveResponse {
        EnclaveResponse::Error {
            message: "boom".into(),
        }
    }

    fn cohort(tag_flip: bool) -> FidelityCohortResult {
        let mut result = FidelityCohortResult {
            outcome: FidelityOpOutcome {
                new_blob: vec![1, 2, 3],
                qualified_start_initialized: Some(5),
                league: 3,
            },
            inputs_canonical_hash: HASH,
            attestation_tag: Vec::new(),
        };
        let preimage = fidelity_cohort_attestation_preimage(HASH, &result);
        result.attestation_tag = signer().sign(&preimage).to_bytes().to_vec();
        result.attestation_tag[0] ^= u8::from(tag_flip);
        result
    }

    fn query(tag_flip: bool) -> FidelityQueryResult {
        let mut result = FidelityQueryResult {
            rcfi: U256::from(11),
            efficiency: U256::from(12),
            league: 4,
            inputs_canonical_hash: HASH,
            attestation_tag: Vec::new(),
        };
        let preimage = fidelity_query_attestation_preimage(HASH, &result);
        result.attestation_tag = signer().sign(&preimage).to_bytes().to_vec();
        result.attestation_tag[0] ^= u8::from(tag_flip);
        result
    }

    fn snapshot(hash: B256, tag_flip: bool) -> EnclaveResponse {
        let leagues = vec![FidelityLeagueEntry {
            owner: Address::repeat_byte(1),
            league: 2,
            rcfi: U256::ZERO,
        }];
        let preimage = fidelity_snapshot_attestation_preimage(hash, &leagues);
        let mut attestation_tag = signer().sign(&preimage).to_bytes().to_vec();
        attestation_tag[0] ^= u8::from(tag_flip);
        EnclaveResponse::FidelityLeaguesSnapshotted {
            leagues,
            inputs_canonical_hash: hash,
            attestation_tag,
        }
    }

    fn cohort_response(result: FidelityCohortResult) -> EnclaveResponse {
        EnclaveResponse::FidelityCohortApplied {
            result: Box::new(result),
        }
    }

    fn query_response(result: FidelityQueryResult) -> EnclaveResponse {
        EnclaveResponse::FidelityIndexQueried {
            result: Box::new(result),
        }
    }

    #[test]
    fn cohort_response_needs_its_variant_input_hash_and_attestation() {
        let key = attestation_pub();
        let validate =
            |hash, response| validate_answer::<FidelityCohortResult>(hash, &key, response);
        assert_eq!(
            validate(HASH, cohort_response(cohort(false))).unwrap(),
            cohort(false)
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, cohort_response(cohort(false)))),
            "tee_enclave_nondeterminism"
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, cohort_response(cohort(true)))),
            "tee_enclave_nondeterminism"
        );
        assert!(fatal(validate(HASH, cohort_response(cohort(true))))
            .starts_with("tee_fidelity_attestation_invalid: "));
        assert_eq!(
            fatal(validate(HASH, enclave_error())),
            "enclave ApplyFidelityCohortOp error: boom"
        );
        assert!(fatal(validate(HASH, query_response(query(false))))
            .starts_with("unexpected enclave response: "));
    }

    #[test]
    fn snapshot_response_needs_its_variant_input_hash_and_attestation() {
        let key = attestation_pub();
        let validate = |hash, response| {
            validate_answer::<LeagueSnapshot>(hash, &key, response).map(|snapshot| snapshot.leagues)
        };
        assert_eq!(
            validate(HASH, snapshot(HASH, false)).unwrap(),
            vec![FidelityLeagueEntry {
                owner: Address::repeat_byte(1),
                league: 2,
                rcfi: U256::ZERO,
            }]
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, snapshot(HASH, false))),
            "tee_enclave_nondeterminism"
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, snapshot(HASH, true))),
            "tee_enclave_nondeterminism"
        );
        assert!(fatal(validate(HASH, snapshot(HASH, true)))
            .starts_with("tee_fidelity_attestation_invalid: "));
        assert_eq!(
            fatal(validate(HASH, enclave_error())),
            "enclave SnapshotFidelityLeagues error: boom"
        );
        assert!(fatal(validate(HASH, cohort_response(cohort(false))))
            .starts_with("unexpected enclave response: "));
    }

    #[test]
    fn query_response_needs_its_variant_input_hash_and_attestation() {
        let key = attestation_pub();
        let validate =
            |hash, response| validate_answer::<FidelityQueryResult>(hash, &key, response);
        assert_eq!(
            validate(HASH, query_response(query(false))).unwrap(),
            query(false)
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, query_response(query(false)))),
            "tee_enclave_nondeterminism"
        );
        assert_eq!(
            fatal(validate(OTHER_HASH, query_response(query(true)))),
            "tee_enclave_nondeterminism"
        );
        assert!(fatal(validate(HASH, query_response(query(true))))
            .starts_with("tee_fidelity_attestation_invalid: "));
        assert_eq!(
            fatal(validate(HASH, enclave_error())),
            "enclave QueryFidelityIndex error: boom"
        );
        assert!(fatal(validate(HASH, snapshot(HASH, false)))
            .starts_with("unexpected enclave response: "));
    }
}

#[test]
fn wwd_max_fidelity_locks_per_wwd_and_rolls_over_at_metadosis() {
    use outbe_primitives::time::WorldwideDay;

    with_env(|storage| {
        let fc = FidelityContract::new(storage.clone());
        let wwd1 = WorldwideDay::new(20261015);
        let wwd2 = WorldwideDay::new(20261016);

        // Initially no account qualified -> max_fidelity_for_wwd is 0, league is 1.
        assert_eq!(fc.max_fidelity_for_wwd(wwd1).unwrap(), U256::ZERO);
        assert_eq!(
            fc.league_from_rcfi_for_wwd(wwd1, U256::from(500_000_000_000_000_000u64))
                .unwrap(),
            MIN_LEAGUE
        );
        // Running max for wwd1 accumulated 0.5e18, while locked max for wwd1 remains 0.
        assert_eq!(fc.max_fidelity_for_wwd(wwd1).unwrap(), U256::ZERO);
        assert_eq!(
            fc.wwd_running_max_fidelity_of(wwd1).unwrap(),
            U256::from(500_000_000_000_000_000u64)
        );

        // Finalize wwd1 at Metadosis -> rolls 0.5e18 into max_fidelity_prev.
        fc.finalize_wwd_max_fidelity(wwd1, 10 * DAY).unwrap();
        assert_eq!(
            fc.max_fidelity_prev().unwrap(),
            U256::from(500_000_000_000_000_000u64)
        );

        // On wwd2, max_fidelity is now 0.5e18; a user with 0.5e18 gets MAX_LEAGUE (4096),
        // and a user with 0.25e18 gets ~2048.
        assert_eq!(
            fc.league_from_rcfi_for_wwd(wwd2, U256::from(500_000_000_000_000_000u64))
                .unwrap(),
            MAX_LEAGUE
        );
        assert_eq!(
            fc.league_from_rcfi_for_wwd(wwd2, U256::from(250_000_000_000_000_000u64))
                .unwrap(),
            2049
        );
    });
}

#[test]
fn same_day_buy_and_full_sell_resets_qualified_start() {
    use outbe_primitives::time::WorldwideDay;

    with_env(|storage| {
        let fc = FidelityContract::new(storage.clone());
        let user = Address::repeat_byte(0x42);
        let t0 = 10 * DAY;

        // Buy 100 COEN and immediately sell all 100 COEN on the same UTC day.
        api::cohort_in(storage.clone(), user, U256::from(100_000_000u64), t0).unwrap();
        api::cohort_out(storage.clone(), user, U256::from(100_000_000u64), t0 + 60).unwrap();

        // 30 days later, the user should not have accrued personal fidelity.
        let t_later = t0 + 30 * DAY;
        let wwd = WorldwideDay::new(20261015);
        assert_eq!(fc.league_for_wwd(user, wwd, t_later).unwrap(), MIN_LEAGUE);
        assert_eq!(fc.wwd_running_max_fidelity_of(wwd).unwrap(), U256::ZERO);
    });
}
