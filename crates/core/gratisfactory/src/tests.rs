//! Confidential gratisfactory tests driven by the in-process enclave engine
//! (`outbe_gratis::enclave_client::test_enclave`). The tests decrypt the ciphertext with
//! the account's view key, exactly as a client would, to assert balances. Writes carry a
//! `ModifyAuth` bound to the account's op-nonce.

use alloy_primitives::{address, Address, Bytes, FixedBytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolInterface};

use outbe_core_common::erc::ERC165_INTERFACE_ID;
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratis::{decrypt_balance, decrypt_pledged, derive_view_key};
use outbe_gratis::{GratisOp, ModifyAuth};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS};
use outbe_system_common::time::SECONDS_PER_DAY;
use outbe_system_common::units::checked_protocol_to_native;

use outbe_fidelity::enclave_client::test_enclave as fidelity_enclave;
use outbe_fidelity::{MAX_LEAGUE, MIN_LEAGUE};

use crate::precompile::{dispatch, IGratisFactory};
use crate::runtime;

const CHAIN_ID: u64 = 1;
const CREATED_AT: u64 = 1_700_000_000;

fn alice() -> Address {
    address!("0x1111111111111111111111111111111111111111")
}
fn chain_b256() -> B256 {
    B256::from(U256::from(CHAIN_ID))
}

/// Build the modify authorization a client holding `owner`'s modify key sends for
/// `op` on `amount` at `op_nonce`.
fn auth(op: GratisOp, owner: Address, amount: U256, op_nonce: u64) -> ModifyAuth {
    test_enclave::modify_auth(outbe_gratis::ModifyOperation {
        account: owner,
        op,
        amount,
        op_nonce,
        chain_id: chain_b256(),
    })
}

fn view_balance(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::balance_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_balance(&vk, a, &blob).unwrap()
}

/// Mint gratis and drop the fidelity outcome, so no cohort is persisted.
/// `outbe_gratis::api::mint` is visible only inside that crate's own tests.
fn mint_without_cohort(storage: StorageHandle<'_>, amount: U256) {
    let now = storage.now_secs().unwrap();
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        alice(),
        outbe_fidelity::api::FidelityCohortOp::In,
        now,
    )
    .unwrap();
    outbe_gratis::api::mint_with_fidelity(
        storage,
        alice(),
        amount,
        auth(GratisOp::Mint, alice(), amount, 0),
        section,
    )
    .unwrap();
}

/// Give `account` a positive Fidelity index so `pledge_gratis` clears the
/// eligibility gate.
fn seed_fidelity(storage: StorageHandle<'_>, account: Address) {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    outbe_fidelity::api::cohort_in(
        storage,
        account,
        U256::from(100u64),
        CREATED_AT - ONE_YEAR_SECS,
    )
    .unwrap();
}

fn test_storage() -> HashMapStorageProvider {
    test_enclave::install();
    fidelity_enclave::install();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(CREATED_AT));
    storage
}

fn with_env<R>(f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut storage = test_storage();
    let out = StorageHandle::enter(&mut storage, f);
    fidelity_enclave::uninstall();
    test_enclave::uninstall();
    out
}

#[test]
fn mine_mints_gratis_and_records_fidelity_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        let later = CREATED_AT + ONE_YEAR_SECS;
        // No cohort yet: no account has qualified, so the league is the floor.
        let league_before =
            outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_before, MIN_LEAGUE);

        runtime::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap();

        assert_eq!(view_balance(&storage, alice()), amount);
        // The acquisition cohort was recorded: sole holder, no sales -> top league.
        let league_after = outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap();
        assert_eq!(league_after, MAX_LEAGUE);
    });
}

#[test]
fn mine_rejects_zero_amount() {
    with_env(|storage| {
        let err = runtime::mint(
            storage.clone(),
            alice(),
            U256::ZERO,
            auth(GratisOp::Mint, alice(), U256::ZERO, 0),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratis::errors::GratisError::Rejected(
                    "amount and account must be nonzero".into(),
                )
            )
            .to_string()
        );
    });
}

#[test]
fn mine_coen_burns_gratis_mints_native_and_records_sale_cohort() {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        mint_without_cohort(storage.clone(), amount);
        outbe_fidelity::api::cohort_in(
            storage.clone(),
            alice(),
            amount,
            CREATED_AT - ONE_YEAR_SECS,
        )
        .unwrap();
        let league_before = outbe_fidelity::api::league(storage.clone(), alice()).unwrap();
        assert_eq!(league_before, MAX_LEAGUE);

        // mineCoen burns gratis (op = Burn) at op-nonce 1.
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        let minted = IGratisFactory::mineCoenCall::abi_decode_returns(&out).unwrap();
        let native_amount = checked_protocol_to_native(amount).unwrap();
        assert_eq!(minted, native_amount);

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(storage.balance(alice()).unwrap(), native_amount);

        // Fully sold -> efficiency 0 -> league drops to the floor.
        let league_after = outbe_fidelity::api::league(storage.clone(), alice()).unwrap();
        assert_eq!(league_after, MIN_LEAGUE);
    });
}

#[test]
fn mine_coen_rejects_insufficient_balance() {
    with_env(|storage| {
        mint_without_cohort(storage.clone(), U256::from(100u64));

        let amount = U256::from(200u64);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert_eq!(
            err.to_string(),
            outbe_primitives::error::PrecompileError::from(
                outbe_gratis::errors::GratisError::Rejected(
                    "insufficient balance or balance overflow".into()
                )
            )
            .to_string()
        );

        // Atomic revert: no COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), U256::from(100u64));
    });
}

#[test]
fn supports_interface() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes(ERC165_INTERFACE_ID),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();
        assert!(IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::supportsInterface(
                IGratisFactory::supportsInterfaceCall {
                    interfaceId: FixedBytes([0xde, 0xad, 0xbe, 0xef]),
                },
            )
            .abi_encode(),
        );
        let out = dispatch(storage, &call, alice(), U256::ZERO).unwrap();
        assert!(!IGratisFactory::supportsInterfaceCall::abi_decode_returns(&out).unwrap());
    });
}

#[test]
fn rejects_msg_value() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: U256::from(1u64),
                mac: FixedBytes([0u8; 32]),
                opNonce: 0,
            })
            .abi_encode(),
        );
        let err = dispatch(storage, &call, alice(), U256::from(1u64)).unwrap_err();
        assert!(err.to_string().contains("non-payable"), "got: {err}");
    });
}

const PLEDGED: u64 = 100;

fn view_pledged(s: &StorageHandle<'_>, a: Address) -> U256 {
    let vk = derive_view_key(&test_enclave::state_key(), a).unwrap();
    let blob = outbe_gratis::api::pledged_ct(s.clone(), a).unwrap();
    if blob.is_empty() {
        return U256::ZERO;
    }
    decrypt_pledged(&vk, a, &blob).unwrap()
}

#[test]
fn pledged_gratis_cannot_be_mined_into_coen() {
    with_env(|storage| {
        let minted = U256::from(1_000u64);
        runtime::mint(
            storage.clone(),
            alice(),
            minted,
            auth(GratisOp::Mint, alice(), minted, 0),
        )
        .unwrap();
        seed_fidelity(storage.clone(), alice());
        let pledged = U256::from(PLEDGED);
        let sealed = test_enclave::sealed_pledge(alice(), pledged, U256::ONE, CHAIN_ID);
        outbe_gratis::api::pledge_sealed(&storage, &sealed, U256::ONE, pledged).unwrap();
        let liquid = minted - pledged;
        let too_much = liquid + U256::ONE;
        assert!(runtime::mine_coen(
            storage.clone(),
            alice(),
            too_much,
            auth(GratisOp::Burn, alice(), too_much, 1),
        )
        .is_err());
        assert_eq!(view_pledged(&storage, alice()), pledged);
        runtime::mine_coen(
            storage.clone(),
            alice(),
            liquid,
            auth(GratisOp::Burn, alice(), liquid, 1),
        )
        .unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), pledged);
        outbe_gratis::api::release_pledged(&storage, alice(), pledged).unwrap();
        runtime::mine_coen(
            storage.clone(),
            alice(),
            pledged,
            auth(GratisOp::Burn, alice(), pledged, 2),
        )
        .unwrap();
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(view_pledged(&storage, alice()), U256::ZERO);
    });
}

#[test]
fn encrypted_nod_mint_preserves_nonce_and_combined_fidelity_state() {
    with_env(|storage| {
        let amount = U256::from(123);
        let nod = encrypted_nod(amount);
        let authorization = auth(GratisOp::Mint, alice(), amount, 0);
        crate::api::mint_encrypted_nod(storage.clone(), &nod, authorization.clone()).unwrap();
        assert_eq!(view_balance(&storage, alice()), amount);
        assert_eq!(
            outbe_gratis::api::op_nonce(storage.clone(), alice()).unwrap(),
            1
        );
        let fidelity = outbe_fidelity::FidelityContract::new(storage.clone());
        let cohorts = fidelity.cohorts_ct_of(alice()).unwrap();
        assert!(!cohorts.is_empty());
        let balance = crate::api::encrypted_balance(storage.clone(), alice()).unwrap();
        assert!(crate::api::mint_encrypted_nod(storage.clone(), &nod, authorization).is_err());
        assert_eq!(
            crate::api::encrypted_balance(storage.clone(), alice()).unwrap(),
            balance
        );
        assert_eq!(fidelity.cohorts_ct_of(alice()).unwrap(), cohorts);
        assert_eq!(outbe_gratis::api::op_nonce(storage, alice()).unwrap(), 1);
    });
}

/// One second past the largest block timestamp that fits in `u64`.
fn encrypted_nod(amount: U256) -> outbe_core_common::nod_encryption::EncryptedNodV1 {
    use outbe_core_common::nod_encryption::NodTermsV1;
    use outbe_system_common::time::WorldwideDay;
    let day = WorldwideDay::new(20250115);
    outbe_nod::nod_encryption::encrypt_nod(
        &outbe_tee::resident::StateKeyHandle::for_test([0x5a; 32]),
        NodTermsV1 {
            chain_id: CHAIN_ID,
            nod_id: U256::from_be_bytes([7; 32]),
            owner: alice(),
            emission_day: day.value(),
            entry_price_minor: U256::ONE,
            issuance_currency: 840,
            reference_currency: 978,
        },
        amount,
        amount,
    )
    .unwrap()
}

fn timestamp_above_u64() -> U256 {
    U256::from(u64::MAX) + U256::from(1)
}

fn assert_timestamp_overflow(err: impl std::fmt::Display) {
    let err = err.to_string();
    assert!(err.contains("timestamp exceeds u64"), "got: {err}");
}

#[test]
fn mint_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();
        let amount = U256::from(1_000u64);
        let err = runtime::mint(
            storage.clone(),
            alice(),
            amount,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap_err();
        assert_timestamp_overflow(err);
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
    });
}

#[test]
fn mine_coen_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        let amount = U256::from(1_000u64);
        mint_without_cohort(storage.clone(), amount);
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        let err = dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap_err();
        assert_timestamp_overflow(err);

        // No COEN minted, gratis untouched.
        assert_eq!(storage.balance(alice()).unwrap(), U256::ZERO);
        assert_eq!(view_balance(&storage, alice()), amount);
    });
}

/// Gratis burn and native COEN mint are one sale: both logs are present together.
#[test]
fn mine_coen_emits_gratis_burned_and_coen_mined() {
    let mut provider = test_storage();
    let amount = U256::from(1_000u64);
    StorageHandle::enter(&mut provider, |storage| {
        mint_without_cohort(storage.clone(), amount);
        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: amount,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), amount, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        dispatch(storage, &call, alice(), U256::ZERO).unwrap();
    });
    let burned: Vec<_> = provider
        .get_events(GRATIS_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_gratis::precompile::IGratis::GratisBurned::decode_log_data(log).ok()
        })
        .collect();
    let mined: Vec<_> = provider
        .get_events(GRATIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGratisFactory::CoenMinedFromGratis::decode_log_data(log).ok())
        .collect();
    fidelity_enclave::uninstall();
    test_enclave::uninstall();

    assert_eq!(burned.len(), 1);
    assert_eq!(burned[0].account, alice());
    assert_eq!(burned[0].encryptedAmountMinor.len(), 92);
    assert_eq!(&burned[0].encryptedAmountMinor[8..12], b"GRA2");
    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].account, alice());
    assert_eq!(
        mined[0].amountMinor,
        checked_protocol_to_native(amount).unwrap()
    );
}

#[test]
fn encrypted_nod_mint_reverts_when_the_timestamp_exceeds_u64() {
    with_env(|storage| {
        let amount = U256::from(123);
        let nod = encrypted_nod(amount);
        storage.set_block_timestamp(timestamp_above_u64()).unwrap();

        let err = crate::api::mint_encrypted_nod(
            storage.clone(),
            &nod,
            auth(GratisOp::Mint, alice(), amount, 0),
        )
        .unwrap_err();
        assert_timestamp_overflow(err);
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(outbe_gratis::api::op_nonce(storage, alice()).unwrap(), 0);
    });
}

fn token_deltas(logs: &[alloy_primitives::LogData], account: Address) -> Vec<(bool, U256)> {
    use outbe_gratis::precompile::IGratis;
    let vk = derive_view_key(&test_enclave::state_key(), account).unwrap();
    let mut deltas = Vec::new();
    for log in logs {
        let opened = if let Ok(event) = IGratis::GratisMinedFromNod::decode_log_data(log) {
            Some((true, event.encryptedAmountMinor))
        } else if let Ok(event) = IGratis::GratisMinedFromPromis::decode_log_data(log) {
            Some((true, event.encryptedAmountMinor))
        } else if let Ok(event) = IGratis::GratisBurned::decode_log_data(log) {
            Some((false, event.encryptedAmountMinor))
        } else {
            None
        };
        if let Some((mint, blob)) = opened {
            let amount = outbe_gratis::decrypt_balance(&vk, account, &blob).unwrap();
            deltas.push((mint, amount));
        }
    }
    deltas
}

fn mine_gratis_call(gratis_minor: U256, op_nonce: u64) -> Bytes {
    Bytes::from(
        IGratisFactory::IGratisFactoryCalls::mineGratis(IGratisFactory::mineGratisCall {
            mac: FixedBytes(auth(GratisOp::Mint, alice(), gratis_minor, op_nonce).mac),
            opNonce: op_nonce,
        })
        .abi_encode(),
    )
}

/// Dispatch `mineGratis` the way the payable route does: credit `value` to the
/// factory first.
fn mine_gratis(storage: &StorageHandle<'_>, call: &Bytes, value: U256) -> Result<U256, String> {
    storage
        .increase_balance(GRATIS_FACTORY_ADDRESS, value)
        .unwrap();
    dispatch(storage.clone(), call, alice(), value)
        .map(|out| IGratisFactory::mineGratisCall::abi_decode_returns(&out).unwrap())
        .map_err(|err| err.to_string())
}

fn promis_limit(storage: &StorageHandle<'_>) -> U256 {
    outbe_promislimit::PromisLimitContract::new(storage.clone())
        .promis_limit()
        .unwrap()
}

/// 2 COEN in, 2 Gratis out. The COEN is burned into the Promis Limit and the
/// mint records the acquisition cohort.
#[test]
fn mine_gratis_burns_coen_into_the_promis_limit_and_mints_gratis() {
    const ONE_YEAR_SECS: u64 = 365 * SECONDS_PER_DAY;
    let mut provider = test_storage();
    let gratis_minor = U256::from(2_000_000u64);
    let value = checked_protocol_to_native(gratis_minor).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let minted = mine_gratis(&storage, &mine_gratis_call(gratis_minor, 0), value).unwrap();
        assert_eq!(minted, gratis_minor);
        assert_eq!(view_balance(&storage, alice()), gratis_minor);
        assert_eq!(storage.balance(GRATIS_FACTORY_ADDRESS).unwrap(), U256::ZERO);
        assert_eq!(promis_limit(&storage), gratis_minor);
        let later = CREATED_AT + ONE_YEAR_SECS;
        assert_eq!(
            outbe_fidelity::api::league_at(storage.clone(), alice(), later).unwrap(),
            MAX_LEAGUE
        );
    });
    let mined: Vec<_> = provider
        .get_events(GRATIS_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IGratisFactory::GratisMinedFromCoen::decode_log_data(log).ok())
        .collect();
    let added: Vec<_> = provider
        .get_events(outbe_system_common::addresses::PROMIS_LIMIT_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_promislimit::precompile::IPromisLimit::PromisLimitAdded::decode_log_data(log).ok()
        })
        .collect();
    let deltas = token_deltas(provider.get_events(GRATIS_ADDRESS), alice());
    fidelity_enclave::uninstall();
    test_enclave::uninstall();

    assert_eq!(mined.len(), 1);
    assert_eq!(mined[0].account, alice());
    assert_eq!(mined[0].gratisMinor, gratis_minor);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].source, 47);
    assert_eq!(added[0].amountMinor, gratis_minor);
    assert_eq!(deltas, vec![(true, gratis_minor)]);
}

#[test]
fn mine_gratis_rejects_zero_and_fractional_coen() {
    with_env(|storage| {
        let invalid = outbe_primitives::error::PrecompileError::from(
            crate::errors::GratisFactoryError::InvalidCoenAmount,
        )
        .to_string();
        let one = U256::ONE;
        let fractional = checked_protocol_to_native(one).unwrap() + U256::ONE;
        for value in [U256::ZERO, fractional] {
            let err = mine_gratis(&storage, &mine_gratis_call(one, 0), value).unwrap_err();
            assert_eq!(err, invalid);
        }
        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(promis_limit(&storage), U256::ZERO);
    });
}

/// A MAC over a different amount fails the mint, and the burn reverts with it.
#[test]
fn mine_gratis_with_a_bad_mac_burns_nothing() {
    with_env(|storage| {
        let gratis_minor = U256::from(1_000u64);
        let value = checked_protocol_to_native(gratis_minor).unwrap();
        let call = mine_gratis_call(gratis_minor + U256::ONE, 0);
        mine_gratis(&storage, &call, value).unwrap_err();

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(promis_limit(&storage), U256::ZERO);
        assert_eq!(storage.balance(GRATIS_FACTORY_ADDRESS).unwrap(), value);
    });
}

/// `mineGratis` then `mineCoen` of the same amount restores the native balance.
#[test]
fn mine_gratis_then_mine_coen_round_trips() {
    with_env(|storage| {
        let gratis_minor = U256::from(1_000u64);
        let value = checked_protocol_to_native(gratis_minor).unwrap();
        mine_gratis(&storage, &mine_gratis_call(gratis_minor, 0), value).unwrap();

        let call = Bytes::from(
            IGratisFactory::IGratisFactoryCalls::mineCoen(IGratisFactory::mineCoenCall {
                gratisMinor: gratis_minor,
                mac: FixedBytes(auth(GratisOp::Burn, alice(), gratis_minor, 1).mac),
                opNonce: 1,
            })
            .abi_encode(),
        );
        dispatch(storage.clone(), &call, alice(), U256::ZERO).unwrap();

        assert_eq!(view_balance(&storage, alice()), U256::ZERO);
        assert_eq!(storage.balance(alice()).unwrap(), value);
    });
}
