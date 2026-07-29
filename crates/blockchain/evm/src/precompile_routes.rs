//! Compact exact-address routing for existing Outbe precompiles.
//!
//! This module owns only execution facts consumed by the dispatcher: exact address,
//! dispatch adapter and base-gas function. Activation, persistence, warming,
//! sponsorship and future address classes intentionally remain outside this table.

use alloy_primitives::{Address, Bytes, U256};
use outbe_compressed_entities::ExecutionScope;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_primitives::{
    addresses::*,
    error::{PrecompileError, Result},
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
};

pub(crate) type DispatchFn = fn(StorageHandle, &[u8], Address, U256) -> Result<Bytes>;
type ReaderDispatchFn =
    fn(StorageHandle, &ExecutionScope, &RuntimeBodyReaders, &[u8], Address, U256) -> Result<Bytes>;
pub(crate) type BaseGasFn = fn(&[u8]) -> u64;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReaderMode {
    None,
    Required,
    Optional,
}

#[derive(Clone, Copy)]
enum DispatchAdapter {
    Basic(DispatchFn),
    ReadersRequired(ReaderDispatchFn),
    ReadersOptional {
        without_readers: DispatchFn,
        with_readers: ReaderDispatchFn,
    },
}

#[derive(Clone, Copy)]
pub(crate) struct ExactRoute {
    dispatch: DispatchAdapter,
    base_gas: BaseGasFn,
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
    pub(crate) fn base_gas(self, input: &[u8]) -> u64 {
        match self {
            Self::Exact(route) => route.base_gas(input),
            Self::StablecoinClass => {
                outbe_primitives::stablecoin_fork::STABLECOIN_V1_GAS.dispatch_base
            }
        }
    }

    pub(crate) fn dispatch(
        self,
        storage: StorageHandle,
        execution_scope: &ExecutionScope,
        readers: Option<&RuntimeBodyReaders>,
        call: RouteCall<'_>,
    ) -> Result<Bytes> {
        match self {
            Self::Exact(route) => route.dispatch(
                storage,
                execution_scope,
                readers,
                call.data,
                call.caller,
                call.value,
            ),
            Self::StablecoinClass => {
                stablecoin_class_dispatch(storage, call.callee, call.data, call.caller, call.value)
            }
        }
    }
}

impl ExactRoute {
    pub(crate) fn base_gas(self, input: &[u8]) -> u64 {
        (self.base_gas)(input)
    }

    #[cfg(test)]
    pub(crate) const fn reader_mode(self) -> ReaderMode {
        match self.dispatch {
            DispatchAdapter::Basic(_) => ReaderMode::None,
            DispatchAdapter::ReadersRequired(_) => ReaderMode::Required,
            DispatchAdapter::ReadersOptional { .. } => ReaderMode::Optional,
        }
    }

    pub(crate) fn dispatch(
        self,
        storage: StorageHandle,
        execution_scope: &ExecutionScope,
        readers: Option<&RuntimeBodyReaders>,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        match (self.dispatch, readers) {
            (DispatchAdapter::Basic(dispatch), _) => dispatch(storage, data, caller, value),
            (DispatchAdapter::ReadersRequired(dispatch), Some(readers)) => {
                dispatch(storage, execution_scope, readers, data, caller, value)
            }
            (DispatchAdapter::ReadersRequired(_), None) => Err(PrecompileError::Fatal(
                "execution body read authority was not supplied".into(),
            )),
            (DispatchAdapter::ReadersOptional { with_readers, .. }, Some(readers)) => {
                with_readers(storage, execution_scope, readers, data, caller, value)
            }
            (
                DispatchAdapter::ReadersOptional {
                    without_readers, ..
                },
                None,
            ) => without_readers(storage, data, caller, value),
        }
    }
}

fn default_base_gas(_input: &[u8]) -> u64 {
    PRECOMPILE_BASE_GAS
}

fn stablecoin_policy_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinpolicy::precompile::dispatch(storage, data, caller, value)
}

fn stablecoin_factory_dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_stablecoinfactory::precompile::dispatch(storage, data, caller, value)
}

fn stablecoin_class_dispatch(
    storage: StorageHandle,
    token: Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    // Reserving the address class must not make native value unspendable. An
    // empty-calldata transfer uses only revm's ordinary CALL balance semantics;
    // it neither invokes the token ABI nor falls through to account bytecode.
    if data.is_empty() && !value.is_zero() {
        return Ok(Bytes::new());
    }

    let Some(factory_token_id) =
        outbe_stablecoinfactory::StablecoinFactoryApi::token_id_of(storage.clone(), token)?
    else {
        return Err(PrecompileError::Revert(
            "unregistered stablecoin address".into(),
        ));
    };

    let expected_address = outbe_primitives::stablecoin::stablecoin_address(
        factory_token_id,
        STABLECOIN_ADDRESS_PREFIX,
    );
    if expected_address != token {
        return Err(PrecompileError::Fatal(format!(
            "stablecoin Factory reverse id does not derive callee {token}"
        )));
    }

    storage.with_account_info(token, |info| {
        let marker = info
            .code
            .as_ref()
            .map(revm::state::Bytecode::original_bytes)
            .unwrap_or_default();
        if marker.as_ref() != STABLECOIN_MARKER_CODE {
            return Err(PrecompileError::Fatal(format!(
                "stablecoin {token} marker code mismatch"
            )));
        }
        Ok(())
    })?;

    if let Err(error) = outbe_stablecoin::api::schema_version(storage.clone(), token) {
        return match error {
            PrecompileError::Revert(_) | PrecompileError::RevertBytes(_) => {
                Err(PrecompileError::Fatal(format!(
                    "stablecoin {token} schema is not initialized for V1"
                )))
            }
            other => Err(other),
        };
    }

    let identity = outbe_stablecoin::api::identity(storage.clone(), token)?;
    if identity.token_id != factory_token_id {
        return Err(PrecompileError::Fatal(format!(
            "stablecoin {token} identity does not match Factory registration"
        )));
    }

    outbe_stablecoin::precompile::dispatch(storage, token, data, caller, value)
}

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
        crate::handlers::vote::registry(),
    )
}

macro_rules! define_exact_routes {
    ($($address:path => ($dispatch:expr, $gas:expr)),+ $(,)?) => {
        pub(crate) const EXACT_ADDRESSES: &[Address] = &[$($address),+];

        pub(crate) fn lookup(address: &Address) -> Option<ExactRoute> {
            match *address {
                $(
                    $address => Some(ExactRoute {
                        dispatch: $dispatch,
                        base_gas: $gas,
                    }),
                )+
                _ => None,
            }
        }
    };
}

// The only declaration of existing exact routes. Constant match patterns make a
// duplicate address an unreachable-pattern error under the workspace's denied
// warnings; the const validator below independently rejects duplicates and Ethereum
// precompile overlap during compilation.
define_exact_routes! {
    GRATIS_ADDRESS => (DispatchAdapter::Basic(outbe_gratis::precompile::dispatch), default_base_gas),
    GRATIS_FACTORY_ADDRESS => (DispatchAdapter::Basic(outbe_gratisfactory::precompile::dispatch), default_base_gas),
    PROMIS_ADDRESS => (DispatchAdapter::Basic(outbe_promis::precompile::dispatch), default_base_gas),
    PROMIS_FACTORY_ADDRESS => (DispatchAdapter::Basic(outbe_promisfactory::precompile::dispatch), default_base_gas),
    TRIBUTE_ADDRESS => (DispatchAdapter::ReadersRequired(outbe_tribute::precompile::dispatch), default_base_gas),
    NOD_ADDRESS => (DispatchAdapter::ReadersRequired(outbe_nod::precompile::dispatch), default_base_gas),
    NOD_FACTORY_ADDRESS => (DispatchAdapter::ReadersRequired(outbe_nodfactory::precompile::dispatch), default_base_gas),
    GEM_ADDRESS => (DispatchAdapter::Basic(outbe_gem::precompile::dispatch), default_base_gas),
    GEM_FACTORY_ADDRESS => (DispatchAdapter::Basic(outbe_gemfactory::precompile::dispatch), default_base_gas),
    INTEX_ADDRESS => (DispatchAdapter::Basic(outbe_intex::precompile::dispatch), default_base_gas),
    INTEX_FACTORY_ADDRESS => (DispatchAdapter::Basic(outbe_intexfactory::precompile::dispatch), default_base_gas),
    DESIS_ADDRESS => (DispatchAdapter::Basic(outbe_desis::precompile::dispatch), default_base_gas),
    VAULT_PROVIDER_ADDRESS => (DispatchAdapter::Basic(outbe_vaultprovider::precompile::dispatch), default_base_gas),
    CREDIS_ADDRESS => (DispatchAdapter::Basic(outbe_credis::precompile::dispatch), default_base_gas),
    CREDIS_FACTORY_ADDRESS => (DispatchAdapter::Basic(outbe_credisfactory::precompile::dispatch), default_base_gas),
    TRIBUTE_FACTORY_ADDRESS => (DispatchAdapter::ReadersRequired(outbe_tributefactory::precompile::dispatch), outbe_tributefactory::precompile::base_gas),
    VALIDATOR_SET_ADDRESS => (DispatchAdapter::Basic(outbe_validatorset::precompile::dispatch), default_base_gas),
    SLASH_INDICATOR_ADDRESS => (DispatchAdapter::Basic(outbe_slashindicator::precompile::dispatch), outbe_slashindicator::precompile::base_gas),
    STAKING_ADDRESS => (DispatchAdapter::Basic(outbe_staking::precompile::dispatch), default_base_gas),
    REWARDS_ADDRESS => (DispatchAdapter::Basic(outbe_rewards::precompile::dispatch), default_base_gas),
    AGENT_REWARD_ADDRESS => (DispatchAdapter::Basic(outbe_agentreward::precompile::dispatch), default_base_gas),
    METADOSIS_ADDRESS => (DispatchAdapter::Basic(outbe_metadosis::precompile::dispatch), default_base_gas),
    FIDELITY_ADDRESS => (DispatchAdapter::Basic(outbe_fidelity::precompile::dispatch), default_base_gas),
    PROMIS_LIMIT_ADDRESS => (DispatchAdapter::Basic(outbe_promislimit::precompile::dispatch), default_base_gas),
    ORACLE_ADDRESS => (DispatchAdapter::Basic(outbe_oracle::precompile::dispatch), default_base_gas),
    ZEROFEE_ADDRESS => (DispatchAdapter::Basic(outbe_zerofee::precompile::dispatch), default_base_gas),
    OUTBE_SYSTEM_TX_ADDRESS => (DispatchAdapter::ReadersOptional {
        without_readers: crate::begin_block_precompile::dispatch,
        with_readers: crate::begin_block_precompile::dispatch_with_readers,
    }, default_base_gas),
    DEBUG_SUBCALL_PRECOMPILE_ADDRESS => (DispatchAdapter::Basic(crate::debug_subcall::dispatch), default_base_gas),
    ZKPROOF_POSEIDON_ADDRESS => (DispatchAdapter::Basic(outbe_zkproof::dispatch_poseidon), outbe_zkproof::poseidon_base_gas),
    ZKPROOF_GROTH16_ADDRESS => (DispatchAdapter::Basic(outbe_zkproof::dispatch_groth16), outbe_zkproof::groth16_base_gas),
    TEE_REGISTRY_ADDRESS => (DispatchAdapter::Basic(outbe_teeregistry::precompile::dispatch), default_base_gas),
    L2_REGISTRY_ADDRESS => (DispatchAdapter::Basic(outbe_l2registry::precompile::dispatch), default_base_gas),
    STABLECOIN_FACTORY_ADDRESS => (DispatchAdapter::Basic(stablecoin_factory_dispatch), default_base_gas),
    STABLECOIN_POLICY_REGISTRY_ADDRESS => (DispatchAdapter::Basic(stablecoin_policy_dispatch), default_base_gas),
    GOVERNANCE_ADDRESS => (DispatchAdapter::Basic(outbe_governance::precompile::dispatch), default_base_gas),
    VOTE_ADDRESS => (DispatchAdapter::Basic(vote_dispatch), default_base_gas),
    UPDATE_ADDRESS => (DispatchAdapter::Basic(outbe_update::precompile::dispatch), default_base_gas),
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
    use alloy_sol_types::SolCall;
    use outbe_primitives::stablecoin::StablecoinCreatePayload;
    use outbe_stablecoin::{FactoryTokenInitialization, StablecoinFactoryApi as TokenFactoryApi};
    use outbe_stablecoinfactory::{
        precompile::IStablecoinFactory, FactoryReservation, StablecoinFactoryApi as RegistryApi,
    };
    use outbe_stablecoinpolicy::precompile::IStablecoinPolicyRegistry;
    use revm::state::Bytecode;

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
    fn required_reader_route_fails_before_module_dispatch_when_authority_is_absent() {
        let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let error = lookup(&TRIBUTE_ADDRESS)
            .unwrap()
            .dispatch(
                storage,
                &ExecutionScope::new(),
                None,
                &[],
                Address::ZERO,
                U256::ZERO,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            PrecompileError::Fatal(message)
                if message == "execution body read authority was not supplied"
        ));
    }

    #[test]
    fn reader_modes_match_existing_execution_requirements() {
        for address in [
            TRIBUTE_ADDRESS,
            TRIBUTE_FACTORY_ADDRESS,
            NOD_ADDRESS,
            NOD_FACTORY_ADDRESS,
        ] {
            assert_eq!(
                lookup(&address).unwrap().reader_mode(),
                ReaderMode::Required
            );
        }
        assert_eq!(
            lookup(&OUTBE_SYSTEM_TX_ADDRESS).unwrap().reader_mode(),
            ReaderMode::Optional
        );
        assert_eq!(
            lookup(&VOTE_ADDRESS).unwrap().reader_mode(),
            ReaderMode::None
        );
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
                &ExecutionScope::new(),
                None,
                &data,
                Address::ZERO,
                U256::ZERO,
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
                &ExecutionScope::new(),
                None,
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
                    &ExecutionScope::new(),
                    None,
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
                &ExecutionScope::new(),
                None,
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
                &ExecutionScope::new(),
                None,
                class_call(token, &[]),
            ),
            Err(PrecompileError::Fatal(message)) if message.contains("marker code mismatch")
        ));

        storage
            .set_code(
                token,
                Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE)),
            )
            .unwrap();
        assert!(matches!(
            route.dispatch(
                storage.clone(),
                &ExecutionScope::new(),
                None,
                class_call(token, &[]),
            ),
            Err(PrecompileError::Fatal(_))
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
                storage.clone(),
                &ExecutionScope::new(),
                None,
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
            resolve(&token).unwrap().dispatch(
                storage,
                &ExecutionScope::new(),
                None,
                class_call(token, &call.abi_encode()),
            ),
            Err(PrecompileError::Fatal(message)) if message.contains("identity does not match")
        ));
    }
}
