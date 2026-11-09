//! Compact exact-address routing for existing Outbe precompiles.
//!
//! This module owns only execution facts consumed by the dispatcher: exact address,
//! dispatch adapter and base-gas function. Activation, persistence, warming,
//! sponsorship and future address classes intentionally remain outside this table.

use alloy_primitives::{Address, Bytes, U256};
#[cfg(test)]
use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
use outbe_primitives::{addresses::*, error::Result, storage::StorageHandle};

pub(crate) const EXACT_ADDRESSES: &[Address] = &[
    GRATIS_ADDRESS,
    GRATIS_FACTORY_ADDRESS,
    PROMIS_ADDRESS,
    PROMIS_FACTORY_ADDRESS,
    TRIBUTE_ADDRESS,
    NOD_ADDRESS,
    NOD_FACTORY_ADDRESS,
    GEM_ADDRESS,
    GEM_FACTORY_ADDRESS,
    INTEX_ADDRESS,
    INTEX_FACTORY_ADDRESS,
    DESIS_ADDRESS,
    VAULT_ROUTER_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    TRIBUTE_FACTORY_ADDRESS,
    VALIDATOR_SET_ADDRESS,
    SLASH_INDICATOR_ADDRESS,
    STAKING_ADDRESS,
    REWARDS_ADDRESS,
    AGENT_REWARD_ADDRESS,
    EMISSION_ADDRESS,
    METADOSIS_ADDRESS,
    FIDELITY_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    ORACLE_ADDRESS,
    ZEROFEE_ADDRESS,
    OUTBE_SYSTEM_TX_ADDRESS,
    ZKPROOF_VERIFY_ADDRESS,
    TEE_REGISTRY_ADDRESS,
    L2_REGISTRY_ADDRESS,
    HYPERLANE_CONTROLLER_ADDRESS,
    CROSSCHAIN_BRIDGE_ADDRESS,
    STABLECOIN_FACTORY_ADDRESS,
    STABLECOIN_POLICY_REGISTRY_ADDRESS,
    CODE_REGISTRY_ADDRESS,
    VOTE_ADDRESS,
    UPDATE_ADDRESS,
];

/// Whether a route may be credited native value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValuePolicy {
    /// No selector is payable.
    Reject,
    /// At least one selector is payable.
    Payable,
}

#[derive(Clone, Copy)]
pub(crate) struct ExactRoute {
    address: Address,
}

#[derive(Clone, Copy)]
pub(crate) enum Route {
    Exact(ExactRoute),
    StablecoinClass,
}

pub(crate) struct RouteCall<'input> {
    pub(crate) callee: Address,
    pub(crate) data: &'input [u8],
    pub(crate) caller: Address,
    pub(crate) value: U256,
}

impl Route {
    /// The value policy the boundary applies to this route.
    pub(crate) fn value_policy(self, host: &dyn crate::EvmRuntimeHost) -> ValuePolicy {
        match self {
            Self::Exact(route) => {
                if host.precompile_is_payable(&route.address) {
                    ValuePolicy::Payable
                } else {
                    ValuePolicy::Reject
                }
            }
            Self::StablecoinClass => ValuePolicy::Payable,
        }
    }

    pub(crate) fn base_gas(self, host: &dyn crate::EvmRuntimeHost, input: &[u8]) -> u64 {
        match self {
            Self::Exact(route) => host.precompile_base_gas(&route.address, input),
            Self::StablecoinClass => {
                outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base
            }
        }
    }

    pub(crate) fn dispatch(
        self,
        host: &dyn crate::EvmRuntimeHost,
        storage: StorageHandle,
        call: RouteCall<'_>,
    ) -> Result<Bytes> {
        let address = match self {
            Self::Exact(route) => route.address,
            Self::StablecoinClass => call.callee,
        };
        host.dispatch_precompile(storage, &address, call.data, call.caller, call.value)
    }
}

#[cfg(test)]
impl ExactRoute {
    pub(crate) fn base_gas(self, host: &dyn crate::EvmRuntimeHost, input: &[u8]) -> u64 {
        host.precompile_base_gas(&self.address, input)
    }

    pub(crate) fn dispatch(
        self,
        host: &dyn crate::EvmRuntimeHost,
        storage: StorageHandle,
        call: RouteCall<'_>,
    ) -> Result<Bytes> {
        let RouteCall {
            data,
            caller,
            value,
            ..
        } = call;
        host.dispatch_precompile(storage, &self.address, data, caller, value)
    }
}

pub(crate) fn lookup(address: &Address) -> Option<ExactRoute> {
    EXACT_ADDRESSES
        .contains(address)
        .then_some(ExactRoute { address: *address })
}

pub(crate) fn resolve(address: &Address) -> Option<Route> {
    lookup(address)
        .map(Route::Exact)
        .or_else(|| is_stablecoin_address(*address).then_some(Route::StablecoinClass))
}

const fn addresses_equal(left: Address, right: Address) -> bool {
    let mut index = 0;
    while index < 20 {
        if left.0 .0[index] != right.0 .0[index] {
            return false;
        }
        index += 1;
    }
    true
}

const fn is_ethereum_precompile(address: Address) -> bool {
    let mut index = 0;
    while index < 19 {
        if address.0 .0[index] != 0 {
            return false;
        }
        index += 1;
    }
    address.0 .0[19] >= 1 && address.0 .0[19] <= 10
}

const fn assert_valid_production_routes(addresses: &[Address]) {
    let mut index = 0;
    while index < addresses.len() {
        if is_ethereum_precompile(addresses[index]) {
            panic!("Outbe exact route overlaps an Ethereum precompile");
        }
        if is_stablecoin_address(addresses[index]) {
            panic!("Outbe exact route overlaps the Stablecoin address class");
        }
        let mut other = index + 1;
        while other < addresses.len() {
            if addresses_equal(addresses[index], addresses[other]) {
                panic!("duplicate Outbe exact route");
            }
            other += 1;
        }
        index += 1;
    }
}

const _: () = assert_valid_production_routes(EXACT_ADDRESSES);

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationError {
    Duplicate(Address),
    EthereumOverlap(Address),
}

#[cfg(test)]
fn validate_exact_addresses(addresses: &[Address]) -> std::result::Result<(), ValidationError> {
    for (index, address) in addresses.iter().copied().enumerate() {
        if is_ethereum_precompile(address) {
            return Err(ValidationError::EthereumOverlap(address));
        }
        if addresses[index + 1..].contains(&address) {
            return Err(ValidationError::Duplicate(address));
        }
    }
    Ok(())
}

#[cfg(test)]
fn domain_host() -> crate::test_support::DomainEvmHost {
    crate::test_support::DomainEvmHost::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::b256;
    use alloy_sol_types::{sol, SolCall};
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::stablecoin::StablecoinCreatePayload;
    use outbe_stablecoin::{FactoryTokenInitialization, StablecoinFactoryApi as TokenFactoryApi};
    use outbe_stablecoinfactory::{
        precompile::IStablecoinFactory, FactoryReservation, StablecoinFactoryApi as RegistryApi,
    };
    use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
    use revm::state::Bytecode;

    sol! {
        interface ITeeRegistryRouteTest {
            function isNodeHostEnclaveReady(uint8 rethP2pPrefix, bytes32 rethP2pX)
                external
                view
                returns (bool);
        }
    }

    #[test]
    fn production_exact_routes_validate() {
        assert_eq!(validate_exact_addresses(EXACT_ADDRESSES), Ok(()));
    }

    #[test]
    fn duplicate_and_ethereum_overlap_are_rejected() {
        let duplicate = Address::repeat_byte(0x44);
        assert_eq!(
            validate_exact_addresses(&[duplicate, duplicate]),
            Err(ValidationError::Duplicate(duplicate))
        );

        let ethereum = Address::with_last_byte(1);
        assert_eq!(
            validate_exact_addresses(&[ethereum]),
            Err(ValidationError::EthereumOverlap(ethereum))
        );
    }

    #[test]
    fn tribute_state_read_does_not_require_body_readers() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let call = outbe_tribute::precompile::ITribute::balanceOfCall {
            owner: Address::repeat_byte(1),
        };
        let output = lookup(&TRIBUTE_ADDRESS)
            .unwrap()
            .dispatch(
                &domain_host(),
                StorageHandle::new(&mut provider),
                RouteCall {
                    callee: TRIBUTE_ADDRESS,
                    data: &call.abi_encode(),
                    caller: Address::ZERO,
                    value: U256::ZERO,
                },
            )
            .unwrap();
        assert_eq!(
            outbe_tribute::precompile::ITribute::balanceOfCall::abi_decode_returns(&output)
                .unwrap(),
            U256::ZERO
        );
    }

    #[test]
    fn code_registry_route_uses_standard_base_gas() {
        let route = lookup(&CODE_REGISTRY_ADDRESS).expect("CodeRegistry route");
        assert_eq!(route.base_gas(&domain_host(), &[]), PRECOMPILE_BASE_GAS);
        assert_eq!(
            route.base_gas(&domain_host(), &[0xde, 0xad, 0xbe, 0xef]),
            PRECOMPILE_BASE_GAS
        );
    }

    #[test]
    fn tee_route_accepts_v1_and_rejects_the_legacy_registration_selector() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let route = lookup(&TEE_REGISTRY_ADDRESS).unwrap();

        let v1 = ITeeRegistryRouteTest::isNodeHostEnclaveReadyCall {
            rethP2pPrefix: 2,
            // SEC1 compressed secp256k1 generator point.
            rethP2pX: b256!("79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
        }
        .abi_encode();
        let output = route
            .dispatch(
                &domain_host(),
                StorageHandle::new(&mut provider),
                RouteCall {
                    callee: TEE_REGISTRY_ADDRESS,
                    data: &v1,
                    caller: Address::ZERO,
                    value: U256::ZERO,
                },
            )
            .unwrap();
        assert!(
            !ITeeRegistryRouteTest::isNodeHostEnclaveReadyCall::abi_decode_returns(&output)
                .unwrap()
        );

        // Use the exact legacy signature rather than the Rust method name.
        let mut legacy = vec![0_u8; 4 + 32 * 6];
        legacy[..4].copy_from_slice(
            &alloy_primitives::keccak256(
                b"registerEnclave(uint256,uint256,uint256,uint256,uint256,uint16)",
            )[..4],
        );
        assert!(route
            .dispatch(
                &domain_host(),
                StorageHandle::new(&mut provider),
                RouteCall {
                    callee: TEE_REGISTRY_ADDRESS,
                    data: &legacy,
                    caller: Address::ZERO,
                    value: U256::ZERO,
                },
            )
            .is_err());
    }

    #[test]
    fn policy_route_is_available_from_genesis() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let route = lookup(&STABLECOIN_POLICY_REGISTRY_ADDRESS).unwrap();
        let data = IStablecoinPolicyRegistry::policyExistsCall {
            policyId: U256::from(1u64),
        }
        .abi_encode();

        let output = route
            .dispatch(
                &domain_host(),
                storage,
                RouteCall {
                    callee: STABLECOIN_POLICY_REGISTRY_ADDRESS,
                    data: &data,
                    caller: Address::ZERO,
                    value: U256::ZERO,
                },
            )
            .unwrap();
        assert!(IStablecoinPolicyRegistry::policyExistsCall::abi_decode_returns(&output).unwrap());
    }

    #[test]
    fn factory_route_is_available_from_genesis() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let route = lookup(&STABLECOIN_FACTORY_ADDRESS).unwrap();
        let data = IStablecoinFactory::tokenCountCall {}.abi_encode();

        let output = route
            .dispatch(
                &domain_host(),
                storage,
                RouteCall {
                    callee: STABLECOIN_FACTORY_ADDRESS,
                    data: &data,
                    caller: Address::ZERO,
                    value: U256::ZERO,
                },
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

    fn class_call(callee: Address, data: &[u8]) -> RouteCall<'_> {
        RouteCall {
            callee,
            data,
            caller: Address::ZERO,
            value: U256::ZERO,
        }
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
        let (token_id, token) = outbe_primitives::stablecoin::predict_stablecoin(
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

    /// A Factory-issued token address has no key and no selector that moves
    /// native balance, so a plain transfer there would be unrecoverable.
    #[test]
    fn registered_stablecoin_token_refuses_a_plain_native_transfer() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let token = install_token(storage.clone(), true, true);
        let route = resolve(&token).unwrap();

        assert!(matches!(
            route.dispatch(&domain_host(),
                storage,
                RouteCall {
                    value: U256::from(1u64),
                    ..class_call(token, &[])
                },
            ),
            Err(PrecompileError::Revert(message))
                if message == "stablecoin token cannot receive native value"
        ));
    }

    #[test]
    fn stablecoin_class_is_claimed_and_unknown_members_fail_closed() {
        let token = class_address(0x11);
        assert!(matches!(resolve(&token), Some(Route::StablecoinClass)));

        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let route = resolve(&token).unwrap();
        assert_eq!(
            route
                .dispatch(
                    &domain_host(),
                    storage.clone(),
                    RouteCall {
                        value: U256::from(1u64),
                        ..class_call(token, &[])
                    },
                )
                .unwrap(),
            Bytes::new()
        );
        assert!(matches!(
            route.dispatch(&domain_host(),
                storage,
                class_call(token, &[]),
            ),
            Err(PrecompileError::Revert(message)) if message == "unregistered stablecoin address"
        ));
    }

    #[test]
    fn class_authentication_requires_registration_marker_schema_and_matching_full_id() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let token = install_token(storage.clone(), false, false);
        let route = resolve(&token).unwrap();

        assert!(matches!(
            route.dispatch(&domain_host(),
                storage.clone(),
                class_call(token, &[]),
            ),
            Err(PrecompileError::Revert(message)) if message.contains("marker code mismatch")
        ));

        storage
            .set_code(
                token,
                Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE)),
            )
            .unwrap();
        assert!(matches!(
            route.dispatch(&domain_host(), storage.clone(), class_call(token, &[]),),
            Err(PrecompileError::Revert(_))
        ));
    }

    #[test]
    fn authenticated_class_dispatch_uses_the_actual_callee() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let token = install_token(storage.clone(), true, true);
        let call = outbe_stablecoin::precompile::IStablecoin::symbolCall {};
        let output = resolve(&token)
            .unwrap()
            .dispatch(
                &domain_host(),
                storage.clone(),
                class_call(token, &call.abi_encode()),
            )
            .unwrap();
        assert_eq!(
            outbe_stablecoin::precompile::IStablecoin::symbolCall::abi_decode_returns(&output)
                .unwrap(),
            "EXUSD"
        );

        storage
            .sstore(token, U256::from(2u64), U256::from(0x99u64))
            .unwrap();
        assert!(matches!(
            resolve(&token).unwrap().dispatch(&domain_host(),
                storage,
                class_call(token, &call.abi_encode()),
            ),
            Err(PrecompileError::Revert(message)) if message.contains("identity does not match")
        ));
    }
}
