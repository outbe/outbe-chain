//! Exact-address and stablecoin-class dispatch through the production runtime host.

use alloy_primitives::{b256, Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_stablecoin::{FactoryTokenInitialization, StablecoinFactoryApi as TokenFactoryApi};
use outbe_stablecoinfactory::{
    precompile::IStablecoinFactory, FactoryReservation, StablecoinFactoryApi as RegistryApi,
};
use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
use outbe_system_common::addresses::{
    STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS, STABLECOIN_MARKER_CODE,
    STABLECOIN_POLICY_REGISTRY_ADDRESS, TEE_REGISTRY_ADDRESS, TRIBUTE_ADDRESS,
};
use outbe_system_common::stablecoin::StablecoinCreatePayload;
use revm::state::Bytecode;

sol! {
    interface ITeeRegistryRouteTest {
        function isNodeHostEnclaveReady(uint8 rethP2pPrefix, bytes32 rethP2pX)
            external
            view
            returns (bool);
    }
}

fn dispatch_at(
    storage: StorageHandle<'_>,
    address: Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_runtime::OutbeRuntime::new().dispatch_precompile(storage, &address, data, caller, value)
}

#[test]
fn tribute_state_read_does_not_require_body_readers() {
    let mut provider = HashMapStorageProvider::new(1);
    let call = outbe_tribute::precompile::ITribute::balanceOfCall {
        owner: Address::repeat_byte(1),
    };
    let output = dispatch_at(
        StorageHandle::new(&mut provider),
        TRIBUTE_ADDRESS,
        &call.abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    assert_eq!(
        outbe_tribute::precompile::ITribute::balanceOfCall::abi_decode_returns(&output).unwrap(),
        U256::ZERO
    );
}

#[test]
fn tee_route_accepts_v1_and_rejects_the_legacy_registration_selector() {
    let mut provider = HashMapStorageProvider::new(1);
    let v1 = ITeeRegistryRouteTest::isNodeHostEnclaveReadyCall {
        rethP2pPrefix: 2,
        rethP2pX: b256!("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
    }
    .abi_encode();
    let output = dispatch_at(
        StorageHandle::new(&mut provider),
        TEE_REGISTRY_ADDRESS,
        &v1,
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    assert!(
        !ITeeRegistryRouteTest::isNodeHostEnclaveReadyCall::abi_decode_returns(&output).unwrap()
    );

    let mut legacy = vec![0_u8; 4 + 32 * 6];
    legacy[..4].copy_from_slice(
        &alloy_primitives::keccak256(
            b"registerEnclave(uint256,uint256,uint256,uint256,uint256,uint16)",
        )[..4],
    );
    assert!(dispatch_at(
        StorageHandle::new(&mut provider),
        TEE_REGISTRY_ADDRESS,
        &legacy,
        Address::ZERO,
        U256::ZERO,
    )
    .is_err());
}

#[test]
fn policy_route_is_available_from_genesis() {
    let mut provider = HashMapStorageProvider::new(1);
    let data = IStablecoinPolicyRegistry::policyExistsCall {
        policyId: U256::from(1u64),
    }
    .abi_encode();
    let output = dispatch_at(
        StorageHandle::new(&mut provider),
        STABLECOIN_POLICY_REGISTRY_ADDRESS,
        &data,
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    assert!(IStablecoinPolicyRegistry::policyExistsCall::abi_decode_returns(&output).unwrap());
}

#[test]
fn factory_route_is_available_from_genesis() {
    let mut provider = HashMapStorageProvider::new(1);
    let data = IStablecoinFactory::tokenCountCall {}.abi_encode();
    let output = dispatch_at(
        StorageHandle::new(&mut provider),
        STABLECOIN_FACTORY_ADDRESS,
        &data,
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    assert_eq!(
        IStablecoinFactory::tokenCountCall::abi_decode_returns(&output).unwrap(),
        U256::ZERO
    );
}

fn class_address(last: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[..2].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    bytes[19] = last;
    Address::from(bytes)
}

fn install_token(storage: StorageHandle<'_>, marker: bool, initialize: bool) -> Address {
    let issuer = Address::repeat_byte(0x44);
    let payload = StablecoinCreatePayload {
        issuer,
        name: "Example Dollar".into(),
        ticker: "EXUSD".into(),
        iso4217: 840,
        decimals: 6,
        supply_cap: U256::from(1_000_000u64),
        policy_id: U256::from(1u64),
    };
    let (token_id, token) = outbe_system_common::stablecoin::predict_stablecoin(
        storage.chain_id().unwrap(),
        STABLECOIN_FACTORY_ADDRESS,
        issuer,
        &payload.ticker,
        STABLECOIN_ADDRESS_PREFIX,
    )
    .unwrap();
    let reservation = FactoryReservation {
        proposal_id: U256::from(1u64),
        token_id,
        ticker: payload.ticker.clone(),
        token,
    };
    RegistryApi::reserve(storage.clone(), &reservation).unwrap();
    RegistryApi::consume(storage.clone(), reservation.proposal_id).unwrap();
    if marker {
        storage
            .set_code(
                token,
                Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE)),
            )
            .unwrap();
    }
    if initialize {
        TokenFactoryApi::new(storage.clone())
            .initialize(&FactoryTokenInitialization {
                token_address: token,
                token_id,
                creation_protocol_version: 0,
                payload,
            })
            .unwrap();
    }
    token
}

#[test]
fn registered_stablecoin_token_refuses_a_plain_native_transfer() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    let token = install_token(storage.clone(), true, true);
    assert!(matches!(
        dispatch_at(storage, token, &[], Address::ZERO, U256::from(1u64)),
        Err(PrecompileError::Revert(message))
            if message == "stablecoin token cannot receive native value"
    ));
}

#[test]
fn stablecoin_class_is_claimed_and_unknown_members_fail_closed() {
    let token = class_address(0x11);
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    assert_eq!(
        dispatch_at(storage.clone(), token, &[], Address::ZERO, U256::from(1u64),).unwrap(),
        Bytes::new()
    );
    assert!(matches!(
        dispatch_at(storage, token, &[], Address::ZERO, U256::ZERO),
        Err(PrecompileError::Revert(message)) if message == "unregistered stablecoin address"
    ));
}

#[test]
fn class_authentication_requires_registration_marker_schema_and_matching_full_id() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    let token = install_token(storage.clone(), false, false);
    assert!(matches!(
        dispatch_at(storage.clone(), token, &[], Address::ZERO, U256::ZERO),
        Err(PrecompileError::Revert(message)) if message.contains("marker code mismatch")
    ));

    storage
        .set_code(
            token,
            Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE)),
        )
        .unwrap();
    assert!(matches!(
        dispatch_at(storage, token, &[], Address::ZERO, U256::ZERO),
        Err(PrecompileError::Revert(_))
    ));
}

#[test]
fn authenticated_class_dispatch_uses_the_actual_callee() {
    let mut provider = HashMapStorageProvider::new(1);
    let storage = StorageHandle::new(&mut provider);
    let token = install_token(storage.clone(), true, true);
    let call = outbe_stablecoin::precompile::IStablecoin::symbolCall {};
    let output = dispatch_at(
        storage.clone(),
        token,
        &call.abi_encode(),
        Address::ZERO,
        U256::ZERO,
    )
    .unwrap();
    assert_eq!(
        outbe_stablecoin::precompile::IStablecoin::symbolCall::abi_decode_returns(&output).unwrap(),
        "EXUSD"
    );

    storage
        .sstore(token, U256::from(2u64), U256::from(0x99u64))
        .unwrap();
    assert!(matches!(
        dispatch_at(storage, token, &call.abi_encode(), Address::ZERO, U256::ZERO),
        Err(PrecompileError::Revert(message)) if message.contains("identity does not match")
    ));
}
