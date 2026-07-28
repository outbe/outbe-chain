use alloy_primitives::{address, keccak256, Address, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::addresses::{STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::stablecoin::{predict_stablecoin, StablecoinCreatePayload};
use outbe_primitives::stablecoin_fork::{
    STABLECOIN_V1_PROTOCOL_VERSION_RAW, STABLECOIN_V1_SCHEMA_VERSION,
};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{PrecompileStorageProvider, StorageHandle};
use outbe_stablecoinpolicy::ALLOW_ALL_POLICY_ID;

use crate::abi::IStablecoin;
use crate::api::{FactoryTokenInitialization, StablecoinFactoryApi};
use crate::schema::{
    role_key, StablecoinContract, ADMIN_ROLE, CAP_MANAGER_ROLE, COMPLIANCE_ROLE, ENFORCER_ROLE,
    GUARDIAN_ROLE, ISSUER_ROLE, OPERATIONAL_ROLES,
};

const CHAIN_ID: u64 = 7;
const ISSUER: Address = address!("1000000000000000000000000000000000000001");

fn initialization(ticker: &str) -> FactoryTokenInitialization {
    let (token_id, token_address) = predict_stablecoin(
        CHAIN_ID,
        STABLECOIN_FACTORY_ADDRESS,
        ISSUER,
        ticker,
        STABLECOIN_ADDRESS_PREFIX,
    )
    .unwrap();
    FactoryTokenInitialization {
        token_address,
        token_id,
        creation_protocol_version: u64::from(STABLECOIN_V1_PROTOCOL_VERSION_RAW),
        payload: StablecoinCreatePayload {
            issuer: ISSUER,
            name: format!("{ticker} Stablecoin"),
            ticker: ticker.to_owned(),
            iso4217: 840,
            decimals: 6,
            supply_cap: U256::from(1_000_000_000u64),
            policy_id: ALLOW_ALL_POLICY_ID,
        },
    }
}

#[test]
fn layout_is_pinned_to_the_documented_root_slots() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let token = StablecoinContract::new(storage, initialization("USDX").token_address);
        assert_eq!(token.schema_version.slot(), U256::from(0));
        assert_eq!(token.creation_protocol_version.slot(), U256::from(1));
        assert_eq!(token.token_id.slot(), U256::from(2));
        assert_eq!(token.currency.slot(), U256::from(5));
        assert_eq!(token.decimals.slot(), U256::from(6));
        assert_eq!(token.issuer.slot(), U256::from(7));
        assert_eq!(token.supply_cap.slot(), U256::from(8));
        assert_eq!(token.total_supply.slot(), U256::from(9));
        assert_eq!(token.policy_id.slot(), U256::from(10));
        assert_eq!(token.paused.slot(), U256::from(11));
        assert_eq!(token.admin.slot(), U256::from(12));
        assert_eq!(token.pending_admin.slot(), U256::from(13));
        assert_eq!(token.balances.base_slot(), U256::from(14));
        assert_eq!(token.allowances.base_slot(), U256::from(15));
        assert_eq!(token.nonces.base_slot(), U256::from(16));
        assert_eq!(token.roles.base_slot(), U256::from(17));
        assert_eq!(token.frozen.base_slot(), U256::from(18));
    });
}

#[test]
fn role_ids_match_the_frozen_abi_vectors() {
    assert_eq!(ADMIN_ROLE, keccak256("ADMIN"));
    assert_eq!(ISSUER_ROLE, keccak256("ISSUER"));
    assert_eq!(CAP_MANAGER_ROLE, keccak256("CAP_MANAGER"));
    assert_eq!(GUARDIAN_ROLE, keccak256("GUARDIAN"));
    assert_eq!(COMPLIANCE_ROLE, keccak256("COMPLIANCE"));
    assert_eq!(ENFORCER_ROLE, keccak256("ENFORCER"));
}

#[test]
fn factory_initialization_sets_identity_roles_and_zero_supply() {
    let init = initialization("USDX");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        StablecoinFactoryApi::new(storage.clone())
            .initialize(&init)
            .unwrap();
        let token = StablecoinContract::new(storage, init.token_address);

        assert_eq!(
            token.schema_version.read().unwrap(),
            u64::from(STABLECOIN_V1_SCHEMA_VERSION)
        );
        assert_eq!(token.creation_protocol_version.read().unwrap(), 2);
        assert_eq!(token.token_id.read().unwrap(), init.token_id);
        assert_eq!(token.name.read_string().unwrap(), "USDX Stablecoin");
        assert_eq!(token.symbol.read_string().unwrap(), "USDX");
        assert_eq!(token.currency.read().unwrap(), 840);
        assert_eq!(token.decimals.read().unwrap(), 6);
        assert_eq!(token.issuer.read().unwrap(), ISSUER);
        assert_eq!(token.supply_cap.read().unwrap(), init.payload.supply_cap);
        assert_eq!(token.total_supply.read().unwrap(), U256::ZERO);
        assert_eq!(token.policy_id.read().unwrap(), ALLOW_ALL_POLICY_ID);
        assert!(!token.paused.read().unwrap());
        assert_eq!(token.admin.read().unwrap(), ISSUER);
        assert_eq!(token.pending_admin.read().unwrap(), Address::ZERO);
        assert!(token.roles.read(&role_key(ADMIN_ROLE, ISSUER)).unwrap());
        for role in OPERATIONAL_ROLES {
            assert!(token.roles.read(&role_key(role, ISSUER)).unwrap());
        }
    });
}

#[test]
fn token_addresses_are_storage_isolated() {
    let first = initialization("USDX");
    let second = initialization("EURX");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let factory = StablecoinFactoryApi::new(storage.clone());
        factory.initialize(&first).unwrap();
        factory.initialize(&second).unwrap();

        let first_token = StablecoinContract::new(storage.clone(), first.token_address);
        let second_token = StablecoinContract::new(storage, second.token_address);
        assert_eq!(first_token.symbol.read_string().unwrap(), "USDX");
        assert_eq!(second_token.symbol.read_string().unwrap(), "EURX");
        assert_eq!(first_token.token_id.read().unwrap(), first.token_id);
        assert_eq!(second_token.token_id.read().unwrap(), second.token_id);
    });
}

#[test]
fn invalid_or_repeated_initialization_does_not_mutate_state() {
    let valid = initialization("USDX");
    let mut invalid = valid.clone();
    invalid.token_id = B256::repeat_byte(0x44);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);

    StorageHandle::enter(&mut provider, |storage| {
        let factory = StablecoinFactoryApi::new(storage.clone());
        assert!(matches!(
            factory.initialize(&invalid),
            Err(PrecompileError::Revert(_))
        ));
        let token = StablecoinContract::new(storage.clone(), valid.token_address);
        assert_eq!(token.schema_version.read().unwrap(), 0);
        assert!(token.name.is_empty().unwrap());

        factory.initialize(&valid).unwrap();
        assert!(matches!(
            factory.initialize(&valid),
            Err(PrecompileError::Fatal(_))
        ));
        assert_eq!(token.token_id.read().unwrap(), valid.token_id);
        assert_eq!(token.total_supply.read().unwrap(), U256::ZERO);
    });
}

#[test]
fn unknown_schema_fails_closed_with_exact_migration_error() {
    let init = initialization("USDX");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let token = StablecoinContract::new(storage.clone(), init.token_address);
        token.schema_version.write(9).unwrap();
        assert!(matches!(
            StablecoinFactoryApi::new(storage).initialize(&init),
            Err(PrecompileError::Fatal(_))
        ));
        let error = token.identity().unwrap_err();
        let expected = IStablecoin::MigrationRequired {
            storedSchemaVersion: 9,
            activeSchemaVersion: u64::from(STABLECOIN_V1_SCHEMA_VERSION),
        }
        .abi_encode();
        assert!(matches!(
            error,
            PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected
        ));
        assert_eq!(token.schema_version.read().unwrap(), 9);
        assert!(token.name.is_empty().unwrap());
    });
}

#[test]
fn non_pristine_root_is_rejected_without_overwrite() {
    let init = initialization("USDX");
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider
        .sstore(init.token_address, U256::from(8), U256::from(777))
        .unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let factory = StablecoinFactoryApi::new(storage.clone());
        assert!(matches!(
            factory.initialize(&init),
            Err(PrecompileError::Fatal(_))
        ));
        let token = StablecoinContract::new(storage, init.token_address);
        assert_eq!(token.schema_version.read().unwrap(), 0);
        assert_eq!(token.supply_cap.read().unwrap(), U256::from(777));
    });
}

#[test]
fn every_injected_initialization_failure_rolls_back_all_token_state() {
    let init = initialization("USDX");
    let mut measured = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut measured, |storage| {
        StablecoinFactoryApi::new(storage)
            .initialize(&init)
            .unwrap();
    });
    let operation_count = measured.clear_mutation_failure();
    assert!(operation_count > 6);

    for failure_at in 0..operation_count {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.fail_after_mutation_at(failure_at);
        StorageHandle::enter(&mut provider, |storage| {
            assert!(StablecoinFactoryApi::new(storage)
                .initialize(&init)
                .is_err());
        });
        provider.clear_mutation_failure();

        StorageHandle::enter(&mut provider, |storage| {
            let token = StablecoinContract::new(storage, init.token_address);
            assert_eq!(token.schema_version.read().unwrap(), 0);
            assert!(token.name.is_empty().unwrap());
            assert!(token.symbol.is_empty().unwrap());
            assert_eq!(token.token_id.read().unwrap(), B256::ZERO);
            assert_eq!(token.admin.read().unwrap(), Address::ZERO);
            assert!(!token.roles.read(&role_key(ADMIN_ROLE, ISSUER)).unwrap());
            for role in OPERATIONAL_ROLES {
                assert!(!token.roles.read(&role_key(role, ISSUER)).unwrap());
            }
        });
    }
}
