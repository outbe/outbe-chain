//! Compact exact-address routing for existing Outbe precompiles.
//!
//! This module owns only execution facts consumed by the dispatcher: exact address,
//! dispatch adapter and base-gas function. Activation, persistence, warming,
//! sponsorship and future address classes intentionally remain outside this table.

#[cfg(feature = "test-utils")]
use alloy_primitives::B256;
use alloy_primitives::{Address, Bytes, U256};
#[cfg(feature = "test-utils")]
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
    pub(crate) fn value_policy(self) -> ValuePolicy {
        match self {
            Self::Exact(route) => {
                if crate::active_evm_runtime_host().precompile_is_payable(&route.address) {
                    ValuePolicy::Payable
                } else {
                    ValuePolicy::Reject
                }
            }
            Self::StablecoinClass => ValuePolicy::Payable,
        }
    }

    pub(crate) fn base_gas(self, input: &[u8]) -> u64 {
        match self {
            Self::Exact(route) => {
                crate::active_evm_runtime_host().precompile_base_gas(&route.address, input)
            }
            Self::StablecoinClass => {
                outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base
            }
        }
    }

    pub(crate) fn dispatch(self, storage: StorageHandle, call: RouteCall<'_>) -> Result<Bytes> {
        let address = match self {
            Self::Exact(route) => route.address,
            Self::StablecoinClass => call.callee,
        };
        crate::active_evm_runtime_host().dispatch_precompile(
            storage,
            &address,
            call.data,
            call.caller,
            call.value,
        )
    }
}

#[cfg(test)]
impl ExactRoute {
    pub(crate) fn base_gas(self, input: &[u8]) -> u64 {
        crate::active_evm_runtime_host().precompile_base_gas(&self.address, input)
    }

    pub(crate) fn dispatch(self, storage: StorageHandle, call: RouteCall<'_>) -> Result<Bytes> {
        let RouteCall {
            data,
            caller,
            value,
            ..
        } = call;
        crate::active_evm_runtime_host().dispatch_precompile(
            storage,
            &self.address,
            data,
            caller,
            value,
        )
    }
}

pub(crate) fn lookup(address: &Address) -> Option<ExactRoute> {
    EXACT_ADDRESSES
        .contains(address)
        .then_some(ExactRoute { address: *address })
}

#[cfg(feature = "test-utils")]
pub(crate) type DispatchFn = fn(StorageHandle, &[u8], Address, U256) -> Result<Bytes>;
#[cfg(feature = "test-utils")]
pub(crate) type BaseGasFn = fn(&[u8]) -> u64;

#[cfg(feature = "test-utils")]
const fn policy_is_payable(policy: ValuePolicy) -> bool {
    matches!(policy, ValuePolicy::Payable)
}

#[cfg(feature = "test-utils")]
fn default_base_gas(_input: &[u8]) -> u64 {
    PRECOMPILE_BASE_GAS
}

#[cfg(feature = "test-utils")]
#[derive(Clone, Copy)]
struct DomainExactRoute {
    dispatch: DispatchFn,
    base_gas: BaseGasFn,
    value_policy: ValuePolicy,
    payable_selectors: &'static [[u8; 4]],
}

#[cfg(feature = "test-utils")]
impl DomainExactRoute {
    fn dispatch(
        self,
        storage: StorageHandle,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        outbe_primitives::dispatch::reject_value_unless_payable(
            data,
            self.payable_selectors,
            &value,
        )?;
        (self.dispatch)(storage, data, caller, value)
    }
}

#[cfg(feature = "test-utils")]
fn stablecoin_policy_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinpolicy::precompile::dispatch(storage, data, caller, value)
}

#[cfg(feature = "test-utils")]
fn stablecoin_factory_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinfactory::precompile::dispatch(storage, data, caller, value)
}

#[cfg(feature = "test-utils")]
fn stablecoin_class_dispatch(storage: StorageHandle, call: RouteCall<'_>) -> Result<Bytes> {
    let RouteCall {
        callee: token,
        data,
        caller,
        value,
    } = call;
    let plain_native_transfer = data.is_empty() && !value.is_zero();
    let registered = read_stablecoin_registration(&storage, token, plain_native_transfer)?;
    let Some(factory_token_id) = registered else {
        if plain_native_transfer {
            return Ok(Bytes::new());
        }
        return Err(crate::errors::PrecompileRouteError::UnregisteredStablecoinAddress.into());
    };
    if plain_native_transfer {
        return Err(crate::errors::PrecompileRouteError::StablecoinCannotReceiveNativeValue.into());
    }
    authenticate_registered_stablecoin(&storage, token, factory_token_id)?;
    outbe_stablecoin::precompile::dispatch(storage, token, data, caller, value)
}

#[cfg(feature = "test-utils")]
fn read_stablecoin_registration(
    storage: &StorageHandle,
    token: Address,
    plain_native_transfer: bool,
) -> Result<Option<B256>> {
    match outbe_stablecoinfactory::StablecoinFactoryApi::token_id_of(storage.clone(), token) {
        Ok(registered) => Ok(registered),
        Err(_) if plain_native_transfer => {
            Err(crate::errors::PrecompileRouteError::StablecoinRegistryUnavailable.into())
        }
        Err(error) => Err(error),
    }
}

#[cfg(feature = "test-utils")]
fn authenticate_registered_stablecoin(
    storage: &StorageHandle,
    token: Address,
    factory_token_id: B256,
) -> Result<()> {
    let expected_address = outbe_primitives::stablecoin::stablecoin_address(
        factory_token_id,
        STABLECOIN_ADDRESS_PREFIX,
    );
    if expected_address != token {
        return Err(
            crate::errors::PrecompileRouteError::StablecoinReverseIdMismatch { token }.into(),
        );
    }

    storage.with_account_info(token, |info| {
        let marker = info
            .code
            .as_ref()
            .map(revm::state::Bytecode::original_bytes)
            .unwrap_or_default();
        if marker.as_ref() != STABLECOIN_MARKER_CODE {
            return Err(
                crate::errors::PrecompileRouteError::StablecoinMarkerCodeMismatch { token }.into(),
            );
        }
        Ok(())
    })?;

    if let Err(error) = outbe_stablecoin::api::schema_version(storage.clone(), token) {
        if crate::errors::is_revert_error(&error) {
            return Err(
                crate::errors::PrecompileRouteError::StablecoinSchemaNotInitialized { token }
                    .into(),
            );
        }
        return Err(error);
    }

    let identity = outbe_stablecoin::api::identity(storage.clone(), token)?;
    if identity.token_id != factory_token_id {
        return Err(
            crate::errors::PrecompileRouteError::StablecoinIdentityMismatch { token }.into(),
        );
    }

    Ok(())
}

#[cfg(feature = "test-utils")]
fn vote_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_vote::precompile::dispatch_with_handlers(
        storage,
        data,
        caller,
        value,
        outbe_vote::handlers::registry(),
    )
}

#[cfg(feature = "test-utils")]
macro_rules! define_domain_routes {
    ($($address:path => ($dispatch:expr, $gas:expr, $value_policy:expr, $payable_selectors:expr)),+ $(,)?) => {
        $(
            const _: () = assert!(
                policy_is_payable($value_policy) == !$payable_selectors.is_empty(),
                concat!(
                    "route ValuePolicy disagrees with the dispatch module's PAYABLE_SELECTORS for ",
                    stringify!($address),
                ),
            );
        )+

        fn domain_route(address: &Address) -> Option<DomainExactRoute> {
            match *address {
                $(
                    $address => Some(DomainExactRoute {
                        dispatch: $dispatch,
                        base_gas: $gas,
                        value_policy: $value_policy,
                        payable_selectors: $payable_selectors,
                    }),
                )+
                _ => None,
            }
        }
    };
}

#[cfg(feature = "test-utils")]
define_domain_routes! {
    GRATIS_ADDRESS => (outbe_gratis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gratis::precompile::PAYABLE_SELECTORS),
    GRATIS_FACTORY_ADDRESS => (outbe_gratisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gratisfactory::precompile::PAYABLE_SELECTORS),
    PROMIS_ADDRESS => (outbe_promis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promis::precompile::PAYABLE_SELECTORS),
    PROMIS_FACTORY_ADDRESS => (outbe_promisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promisfactory::precompile::PAYABLE_SELECTORS),
    TRIBUTE_ADDRESS => (outbe_tribute::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_tribute::precompile::PAYABLE_SELECTORS),
    NOD_ADDRESS => (outbe_nod::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_nod::precompile::PAYABLE_SELECTORS),
    NOD_FACTORY_ADDRESS => (outbe_nodfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_nodfactory::precompile::PAYABLE_SELECTORS),
    GEM_ADDRESS => (outbe_gem::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gem::precompile::PAYABLE_SELECTORS),
    GEM_FACTORY_ADDRESS => (outbe_gemfactory::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_gemfactory::precompile::PAYABLE_SELECTORS),
    INTEX_ADDRESS => (outbe_intex::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_intex::precompile::PAYABLE_SELECTORS),
    INTEX_FACTORY_ADDRESS => (outbe_intexfactory::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_intexfactory::precompile::PAYABLE_SELECTORS),
    DESIS_ADDRESS => (outbe_desis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_desis::precompile::PAYABLE_SELECTORS),
    VAULT_ROUTER_ADDRESS => (outbe_vaultrouter::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_vaultrouter::precompile::PAYABLE_SELECTORS),
    CREDIS_ADDRESS => (outbe_credis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_credis::precompile::PAYABLE_SELECTORS),
    CREDIS_FACTORY_ADDRESS => (outbe_credisfactory::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_credisfactory::precompile::PAYABLE_SELECTORS),
    CCA_REGISTRY_ADDRESS => (outbe_ccaregistry::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_ccaregistry::precompile::PAYABLE_SELECTORS),
    TRIBUTE_FACTORY_ADDRESS => (outbe_tributefactory::precompile::dispatch, outbe_tributefactory::precompile::base_gas, ValuePolicy::Reject, outbe_tributefactory::precompile::PAYABLE_SELECTORS),
    VALIDATOR_SET_ADDRESS => (outbe_validatorset::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_validatorset::precompile::PAYABLE_SELECTORS),
    SLASH_INDICATOR_ADDRESS => (outbe_slashindicator::precompile::dispatch, outbe_slashindicator::precompile::base_gas, ValuePolicy::Reject, outbe_slashindicator::precompile::PAYABLE_SELECTORS),
    STAKING_ADDRESS => (outbe_staking::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_staking::precompile::PAYABLE_SELECTORS),
    REWARDS_ADDRESS => (outbe_rewards::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_rewards::precompile::PAYABLE_SELECTORS),
    AGENT_REWARD_ADDRESS => (outbe_agentreward::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_agentreward::precompile::PAYABLE_SELECTORS),
    EMISSION_ADDRESS => (outbe_emission::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_emission::precompile::PAYABLE_SELECTORS),
    METADOSIS_ADDRESS => (outbe_metadosis::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_metadosis::precompile::PAYABLE_SELECTORS),
    FIDELITY_ADDRESS => (outbe_fidelity::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_fidelity::precompile::PAYABLE_SELECTORS),
    PROMIS_LIMIT_ADDRESS => (outbe_promislimit::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_promislimit::precompile::PAYABLE_SELECTORS),
    ORACLE_ADDRESS => (outbe_oracle::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_oracle::precompile::PAYABLE_SELECTORS),
    ZEROFEE_ADDRESS => (outbe_zerofee::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_zerofee::precompile::PAYABLE_SELECTORS),
    OUTBE_SYSTEM_TX_ADDRESS => (crate::sys_tx_precompile::dispatch_system_tx_route, default_base_gas, ValuePolicy::Reject, crate::sys_tx_precompile::PAYABLE_SELECTORS),
    TEE_REGISTRY_ADDRESS => (outbe_teeregistry::v1_precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_teeregistry::v1_precompile::PAYABLE_SELECTORS),
    L2_REGISTRY_ADDRESS => (outbe_l2registry::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_l2registry::precompile::PAYABLE_SELECTORS),
    HYPERLANE_CONTROLLER_ADDRESS => (outbe_hyperlanecontroller::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_hyperlanecontroller::precompile::PAYABLE_SELECTORS),
    CROSSCHAIN_BRIDGE_ADDRESS => (outbe_crosschainbridge::precompile::dispatch, default_base_gas, ValuePolicy::Payable, outbe_crosschainbridge::precompile::PAYABLE_SELECTORS),
    STABLECOIN_FACTORY_ADDRESS => (stablecoin_factory_dispatch, default_base_gas, ValuePolicy::Reject, outbe_stablecoinfactory::precompile::PAYABLE_SELECTORS),
    STABLECOIN_POLICY_REGISTRY_ADDRESS => (stablecoin_policy_dispatch, default_base_gas, ValuePolicy::Reject, outbe_stablecoinpolicy::precompile::PAYABLE_SELECTORS),
    CODE_REGISTRY_ADDRESS => (outbe_coderegistry::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_coderegistry::precompile::PAYABLE_SELECTORS),
    VOTE_ADDRESS => (vote_dispatch, default_base_gas, ValuePolicy::Payable, outbe_vote::precompile::PAYABLE_SELECTORS),
    UPDATE_ADDRESS => (outbe_update::precompile::dispatch, default_base_gas, ValuePolicy::Reject, outbe_update::precompile::PAYABLE_SELECTORS),
}

#[cfg(feature = "test-utils")]
pub(crate) fn dispatch_domain_precompile(
    storage: StorageHandle,
    address: &Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    if let Some(route) = domain_route(address) {
        return route.dispatch(storage, data, caller, value);
    }
    if is_stablecoin_address(*address) {
        return stablecoin_class_dispatch(
            storage,
            RouteCall {
                callee: *address,
                data,
                caller,
                value,
            },
        );
    }
    Err(outbe_primitives::error::PrecompileError::Fatal(
        "precompile route is not installed".into(),
    ))
}

#[cfg(feature = "test-utils")]
pub(crate) fn domain_is_payable(address: &Address) -> bool {
    if let Some(route) = domain_route(address) {
        return policy_is_payable(route.value_policy);
    }
    is_stablecoin_address(*address)
}

#[cfg(feature = "test-utils")]
pub(crate) fn domain_base_gas(address: &Address, input: &[u8]) -> u64 {
    if let Some(route) = domain_route(address) {
        return (route.base_gas)(input);
    }
    if is_stablecoin_address(*address) {
        return outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base;
    }
    PRECOMPILE_BASE_GAS
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
        assert_eq!(route.base_gas(&[]), PRECOMPILE_BASE_GAS);
        assert_eq!(
            route.base_gas(&[0xde, 0xad, 0xbe, 0xef]),
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
            route.dispatch(
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
            route.dispatch(
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
            route.dispatch(
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
            route.dispatch(storage.clone(), class_call(token, &[]),),
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
            .dispatch(storage.clone(), class_call(token, &call.abi_encode()))
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
            resolve(&token).unwrap().dispatch(
                storage,
                class_call(token, &call.abi_encode()),
            ),
            Err(PrecompileError::Revert(message)) if message.contains("identity does not match")
        ));
    }
}
