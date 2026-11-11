//! Active TeeRegistry V1 node-enclave registration ABI.
//!
//! A0 routes the production precompile address here exclusively. The global
//! bootstrap views retain their established selectors, while every V1 mutator
//! authenticates the EVM caller against the canonical NodeHost association.

use crate::{
    errors::TeeRegistryError,
    v1::{EnclaveEvidenceV1, NodeHostAssociationV1},
    NodeEnclaveBindingV1, TeeRegistry, V1RegistrationOutcome,
};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::{
    dispatch::{dispatch_call, reject_value, view},
    error::{PrecompileError, Result},
    storage::{gas::PRECOMPILE_BASE_GAS, StorageHandle},
    tee_attestation_v1::{
        AttestationEvidenceV1, RegistryMutatorV1, TeePolicyV1, TeeRegistryGasScheduleV1,
        ValidatorNodeBindingV1,
    },
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};

mod abi;
use abi::{preflight_evidence_mutator_call, RegisterPreflight};

#[cfg(test)]
mod verifier_tests;
#[cfg(test)]
pub(crate) use verifier_tests::{
    dispatch_register_after_verifier_for_test,
    dispatch_register_with_onboarding_after_verifier_for_test,
    dispatch_renew_after_verifier_for_test, dispatch_replace_after_verifier_for_test,
    dispatch_transition_after_verifier_for_test, PostVerifierCall,
};

#[cfg(test)]
mod binding_tests;
#[cfg(test)]
mod preflight_tests;

/// TeeRegistry V1 never accepts native token value on any selector.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Feature-gated V1 ABI entry point. Mutations bind the transaction caller to
/// the canonical address-to-NodeHost association before replay handling.
pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    let mutator = evidence_mutator(data);
    let mutation_preflight = mutation_preflight(&storage, data, mutator)?;
    let active_policy = admitted_policy(&storage, data, mutator, mutation_preflight)?;
    dispatch_call(
        data,
        ITeeRegistryV1::ITeeRegistryV1Calls::abi_decode,
        |call| {
            let mut registry = TeeRegistry::new(storage);
            dispatch_registry_call(
                &mut registry,
                call,
                ActivePolicyCall {
                    caller,
                    preflight: mutation_preflight,
                    policy: active_policy.as_ref(),
                },
            )
        },
    )
}

fn evidence_mutator(data: &[u8]) -> Option<RegistryMutatorV1> {
    match data.get(..4) {
        Some(selector) if selector == ITeeRegistryV1::registerEnclaveCall::SELECTOR => {
            Some(RegistryMutatorV1::RegisterEnclave)
        }
        Some(selector) if selector == ITeeRegistryV1::renewEnclaveCall::SELECTOR => {
            Some(RegistryMutatorV1::RenewEnclave)
        }
        Some(selector) if selector == ITeeRegistryV1::replaceEnclaveBindingCall::SELECTOR => {
            Some(RegistryMutatorV1::ReplaceEnclaveBinding)
        }
        Some(selector)
            if selector == ITeeRegistryV1::transitionEnclaveMeasurementCall::SELECTOR =>
        {
            Some(RegistryMutatorV1::TransitionEnclaveMeasurement)
        }
        Some(selector) if selector == ITeeRegistryV1::prepareEnclaveUpgradeCall::SELECTOR => {
            Some(RegistryMutatorV1::PrepareEnclaveUpgrade)
        }
        _ => None,
    }
}

fn mutation_preflight<'a>(
    storage: &StorageHandle<'_>,
    data: &'a [u8],
    mutator: Option<RegistryMutatorV1>,
) -> Result<Option<RegisterPreflight<'a>>> {
    if mutator.is_some() {
        if storage.is_static()? {
            return Err(PrecompileError::WriteProtection);
        }
        Ok(Some(preflight_evidence_mutator_call(data)?))
    } else {
        Ok(None)
    }
}

fn admitted_policy(
    storage: &StorageHandle<'_>,
    data: &[u8],
    mutator: Option<RegistryMutatorV1>,
    mutation_preflight: Option<RegisterPreflight<'_>>,
) -> Result<Option<TeePolicyV1>> {
    if let (Some(kind), Some(preflight)) = (mutator, mutation_preflight) {
        let registry = TeeRegistry::new(storage.clone());
        let policy = registry.policy_for_evidence_v1(
            preflight.evidence,
            matches!(
                kind,
                RegistryMutatorV1::TransitionEnclaveMeasurement
                    | RegistryMutatorV1::PrepareEnclaveUpgrade
            ),
        )?;
        deduct_mutator_protocol_gas(storage, kind, data.len(), preflight.evidence.len(), &policy)?;
        Ok(Some(policy))
    } else {
        Ok(None)
    }
}

fn dispatch_registry_call(
    registry: &mut TeeRegistry<'_>,
    call: ITeeRegistryV1::ITeeRegistryV1Calls,
    active_policy_call: ActivePolicyCall<'_>,
) -> Result<Bytes> {
    use ITeeRegistryV1::ITeeRegistryV1Calls::*;
    let caller = active_policy_call.caller;
    match call {
        isBootstrapped(call) => view(call, |_| registry.is_bootstrapped()),
        tributeOfferPublicKey(call) => view(call, |_| {
            registry
                .offer_public_key()
                .map(|value| U256::from_be_bytes(value.0))
        }),
        policyHash(call) => view(call, |_| {
            registry
                .policy_hash()
                .map(|value| U256::from_be_bytes(value.0))
        }),
        keyEpoch(call) => view(call, |_| registry.key_epoch().map(U256::from)),
        tributeOfferEpoch(call) => view(call, |_| registry.tribute_offer_epoch().map(U256::from)),
        activePolicyV1(call) => active_policy_view(registry, call),
        enclaveUpgradeV1(call) => enclave_upgrade_view(registry, call),
        stagedSuccessorPolicyV1(call) => staged_successor_policy_view(registry, call),
        registerEnclave(_) => register_enclave(registry, active_policy_call),
        renewEnclave(_) => ActivePolicyMutator::Renew.dispatch(registry, active_policy_call),
        replaceEnclaveBinding(_) => {
            ActivePolicyMutator::Replace.dispatch(registry, active_policy_call)
        }
        transitionEnclaveMeasurement(_) => {
            transition_enclave_measurement(registry, active_policy_call)
        }
        prepareEnclaveUpgrade(_) => prepare_enclave_upgrade(registry, active_policy_call),
        cancelEnclaveUpgrade(call) => {
            registry.cancel_enclave_upgrade_v1(
                caller,
                call.nodeIdHash,
                call.expectedContextHash,
            )?;
            Ok(Bytes::new())
        }
        pendingEnclaveUpgrade(call) => pending_enclave_upgrade_view(registry, call),
        validatorEnclaveBinding(call) => view(call, |call| {
            Ok(binding_view(
                registry.validator_enclave_binding_v1(call.validator)?,
            ))
        }),
        nodeHostEnclaveBinding(call) => view(call, |call| {
            Ok(binding_view(registry.node_host_enclave_binding_v1(
                full_node_public_key(call.rethP2pPrefix, call.rethP2pX),
            )?))
        }),
        isValidatorEnclaveReady(call) => view(call, |call| {
            registry.is_validator_enclave_ready_v1(call.validator)
        }),
        isNodeHostEnclaveReady(call) => view(call, |call| {
            registry.is_node_host_enclave_ready_v1(full_node_public_key(
                call.rethP2pPrefix,
                call.rethP2pX,
            ))
        }),
    }
}

fn register_enclave(registry: &mut TeeRegistry<'_>, call: ActivePolicyCall<'_>) -> Result<Bytes> {
    let caller = call.caller;
    let policy = call
        .policy
        .ok_or(TeeRegistryError::RegistrationPreflightBypassed)?;
    let preflight = call
        .preflight
        .ok_or(TeeRegistryError::RegistrationPreflightBypassed)?;
    let (node_signature, enclave_signature) = preflight.signatures()?;
    let RegistrationAssociation {
        binding,
        validator_signature,
        node_binding_signature,
    } = registration_association(preflight)?;
    let (node_id_hash, _recipient_x25519) = registration_onboarding_target(preflight.evidence)?;
    let onboarding = registry.register_enclave_with_onboarding_v1(
        EnclaveEvidenceV1 {
            caller,
            evidence: preflight.evidence,
            node_signature: &node_signature,
            enclave_signature: &enclave_signature,
        },
        NodeHostAssociationV1 {
            binding: &binding,
            validator_signature: &validator_signature,
            node_binding_signature: &node_binding_signature,
        },
        policy,
    )?;
    registry.emit_verified_onboarding_artifact_v1(&onboarding, node_id_hash)?;
    let outcome = onboarding.registration;
    Ok(Bytes::from(
        ITeeRegistryV1::registerEnclaveCall::abi_encode_returns(&matches!(
            outcome,
            V1RegistrationOutcome::Created
        )),
    ))
}

fn transition_enclave_measurement(
    registry: &mut TeeRegistry<'_>,
    call: ActivePolicyCall<'_>,
) -> Result<Bytes> {
    let caller = call.caller;
    let preflight = call
        .preflight
        .ok_or(TeeRegistryError::MeasurementTransitionPreflightBypassed)?;
    let (node_signature, enclave_signature) = preflight.signatures()?;
    let outcome = registry.transition_enclave_measurement_with_staged_policy_v1(
        caller,
        preflight.evidence,
        &node_signature,
        &enclave_signature,
    )?;
    Ok(Bytes::from(
        ITeeRegistryV1::transitionEnclaveMeasurementCall::abi_encode_returns(&matches!(
            outcome,
            V1RegistrationOutcome::Created
        )),
    ))
}

fn prepare_enclave_upgrade(
    registry: &mut TeeRegistry<'_>,
    call: ActivePolicyCall<'_>,
) -> Result<Bytes> {
    let caller = call.caller;
    let preflight = call
        .preflight
        .ok_or(TeeRegistryError::UpgradePreflightMissing)?;
    let node_signature = preflight
        .node_signature
        .try_into()
        .map_err(|_| TeeRegistryError::NodeSignaturePreflight)?;
    let enclave_signature = preflight
        .enclave_signature
        .try_into()
        .map_err(|_| TeeRegistryError::EnclaveSignaturePreflight)?;
    let outcome = registry.prepare_enclave_upgrade_v1(
        caller,
        preflight.evidence,
        &node_signature,
        &enclave_signature,
    )?;
    Ok(Bytes::from(
        ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_encode_returns(&matches!(
            outcome,
            V1RegistrationOutcome::Created
        )),
    ))
}

fn active_policy_view(
    registry: &TeeRegistry<'_>,
    call: ITeeRegistryV1::activePolicyV1Call,
) -> Result<Bytes> {
    view(call, |_| {
        registry
            .active_policy_v1()?
            .encode_canonical()
            .map(Bytes::from)
            .map_err(TeeRegistryError::active_v1_policy_encoding_failed)
    })
}

fn enclave_upgrade_view(
    registry: &TeeRegistry<'_>,
    call: ITeeRegistryV1::enclaveUpgradeV1Call,
) -> Result<Bytes> {
    view(call, |_| {
        let upgrade = registry.enclave_upgrade_v1()?;
        Ok(ITeeRegistryV1::enclaveUpgradeV1Return {
            proposalId: upgrade.proposal_id,
            activationHeight: upgrade.activation_height,
            mrenclave: upgrade.mrenclave,
            successorPolicyHash: upgrade.successor_policy_hash,
            predecessorPolicyHash: upgrade.predecessor_policy_hash,
        })
    })
}

fn staged_successor_policy_view(
    registry: &TeeRegistry<'_>,
    call: ITeeRegistryV1::stagedSuccessorPolicyV1Call,
) -> Result<Bytes> {
    view(call, |_| {
        let Some((proposal_id, policy)) = registry.staged_successor_policy_v1()? else {
            return Ok(ITeeRegistryV1::stagedSuccessorPolicyV1Return {
                exists: false,
                proposalId: U256::ZERO,
                policy: Bytes::new(),
            });
        };
        let policy = policy
            .encode_canonical()
            .map(Bytes::from)
            .map_err(TeeRegistryError::staged_successor_v1_policy_encoding_failed)?;
        Ok(ITeeRegistryV1::stagedSuccessorPolicyV1Return {
            exists: true,
            proposalId: proposal_id,
            policy,
        })
    })
}

fn pending_enclave_upgrade_view(
    registry: &TeeRegistry<'_>,
    call: ITeeRegistryV1::pendingEnclaveUpgradeCall,
) -> Result<Bytes> {
    view(call, |call| {
        let n = call.nodeIdHash;
        Ok(ITeeRegistryV1::pendingEnclaveUpgradeReturn {
            contextHash: registry.upgrade_candidate_context.read(&n)?,
            validUntil: registry.upgrade_candidate_expiry.read(&n)?,
            sourceBindingId: registry.upgrade_candidate_source.read(&n)?,
            targetHash: registry.upgrade_candidate_target.read(&n)?,
            nonce: registry.upgrade_candidate_nonce.read(&n)?,
        })
    })
}

struct RegistrationAssociation {
    binding: ValidatorNodeBindingV1,
    validator_signature: [u8; 65],
    node_binding_signature: [u8; 65],
}

fn registration_association(preflight: RegisterPreflight<'_>) -> Result<RegistrationAssociation> {
    let binding = ValidatorNodeBindingV1::decode_canonical(
        preflight
            .validator_node_binding
            .ok_or(TeeRegistryError::RegistrationBindingPreflightBypassed)?,
    )
    .map_err(TeeRegistryError::validator_node_host_binding_not_canonical)?;
    let validator_signature: [u8; 65] = preflight
        .validator_signature
        .ok_or(TeeRegistryError::ValidatorSignaturePreflightBypassed)?
        .try_into()
        .map_err(|_| TeeRegistryError::PreflightValidatorSignatureMismatch)?;
    let node_binding_signature: [u8; 65] = preflight
        .node_binding_signature
        .ok_or(TeeRegistryError::NodeHostBindingSignaturePreflightBypassed)?
        .try_into()
        .map_err(|_| TeeRegistryError::PreflightNodeHostBindingSignatureMismatch)?;
    Ok(RegistrationAssociation {
        binding,
        validator_signature,
        node_binding_signature,
    })
}

#[derive(Clone, Copy)]
enum ActivePolicyMutator {
    Renew,
    Replace,
}

struct ActivePolicyCall<'a> {
    caller: Address,
    preflight: Option<RegisterPreflight<'a>>,
    policy: Option<&'a TeePolicyV1>,
}

impl ActivePolicyMutator {
    fn dispatch(self, registry: &mut TeeRegistry<'_>, call: ActivePolicyCall<'_>) -> Result<Bytes> {
        let ActivePolicyCall {
            caller,
            preflight,
            policy,
        } = call;
        let label = match self {
            Self::Renew => "renewal",
            Self::Replace => "replacement",
        };
        let policy = policy.ok_or(TeeRegistryError::MutatorPreflightBypassed(label))?;
        let preflight = preflight.ok_or(TeeRegistryError::MutatorPreflightBypassed(label))?;
        let (node_signature, enclave_signature) = preflight.signatures()?;
        let outcome = match self {
            Self::Renew => registry.renew_enclave_with_active_policy_v1(
                EnclaveEvidenceV1 {
                    caller,
                    evidence: preflight.evidence,
                    node_signature: &node_signature,
                    enclave_signature: &enclave_signature,
                },
                policy,
            ),
            Self::Replace => registry.replace_enclave_binding_with_active_policy_v1(
                EnclaveEvidenceV1 {
                    caller,
                    evidence: preflight.evidence,
                    node_signature: &node_signature,
                    enclave_signature: &enclave_signature,
                },
                policy,
            ),
        }?;
        let created = matches!(outcome, V1RegistrationOutcome::Created);
        let encoded = match self {
            Self::Renew => ITeeRegistryV1::renewEnclaveCall::abi_encode_returns(&created),
            Self::Replace => {
                ITeeRegistryV1::replaceEnclaveBindingCall::abi_encode_returns(&created)
            }
        };
        Ok(Bytes::from(encoded))
    }
}

fn registration_onboarding_target(evidence: &[u8]) -> Result<(B256, [u8; 32])> {
    let decoded = AttestationEvidenceV1::decode_canonical(evidence)
        .map_err(TeeRegistryError::attestation_evidence_not_canonical)?;
    let intent = match decoded {
        AttestationEvidenceV1::Dcap(value) => value.intent,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
    };
    let node_id_hash = intent
        .node_id
        .node_id_hash()
        .map_err(TeeRegistryError::registration_node_identity_invalid)?;
    Ok((node_id_hash, intent.recipient_x25519))
}

fn deduct_mutator_protocol_gas(
    storage: &StorageHandle<'_>,
    kind: RegistryMutatorV1,
    input_len: usize,
    evidence_len: usize,
    policy: &TeePolicyV1,
) -> Result<()> {
    let schedule = TeeRegistryGasScheduleV1::normative();
    let maximum_transaction_gas = schedule
        .maximum_transaction_gas(
            kind,
            input_len,
            evidence_len,
            policy.measurement_rules.len(),
            policy.attestation_mode,
        )
        .map_err(TeeRegistryError::invalid_v1_registry_gas_dimensions)?;
    let intrinsic = schedule
        .maximum_calldata_intrinsic_gas(input_len)
        .map_err(TeeRegistryError::invalid_v1_calldata_gas_dimensions)?;
    let protocol = maximum_transaction_gas
        .checked_sub(intrinsic)
        .ok_or(TeeRegistryError::ProtocolGasUnderflow)?;
    let storage_allowance = schedule.mutator_storage_gas_allowance(kind);
    let prepaid_protocol = protocol
        .checked_sub(storage_allowance)
        .ok_or(TeeRegistryError::MutatorStorageAllowanceExceedsFixedGas)?;
    let dispatch_charge = prepaid_protocol
        .checked_sub(PRECOMPILE_BASE_GAS)
        .ok_or(TeeRegistryError::ProtocolGasBelowBaseCharge)?;
    storage.deduct_gas(dispatch_charge)
}

fn full_node_public_key(prefix: u8, x: B256) -> [u8; 33] {
    let mut public = [0_u8; 33];
    public[0] = prefix;
    public[1..].copy_from_slice(x.as_slice());
    public
}

fn binding_view(binding: Option<NodeEnclaveBindingV1>) -> NodeEnclaveBindingV1View {
    match binding {
        Some(binding) => (&binding).into(),
        None => NodeEnclaveBindingV1View::default(),
    }
}

#[cfg(test)]
mod tests;
