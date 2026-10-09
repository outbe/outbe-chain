//! Active TeeRegistry V1 node-enclave registration ABI.
//!
//! A0 routes the production precompile address here exclusively. The global
//! bootstrap views retain their established selectors, while every V1 mutator
//! authenticates the EVM caller against the canonical NodeHost association.

use crate::v1::{EnclaveEvidenceV1, NodeHostAssociationV1};
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

use crate::{NodeEnclaveBindingV1, TeeRegistry, V1RegistrationOutcome};

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

outbe_primitives::impl_tee_registry_binding_v1_mapping!(NodeEnclaveBindingV1);

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
        .ok_or_else(|| PrecompileError::Fatal("V1 registration preflight was bypassed".into()))?;
    let preflight = call
        .preflight
        .ok_or_else(|| PrecompileError::Fatal("V1 registration preflight was bypassed".into()))?;
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
    let preflight = call.preflight.ok_or_else(|| {
        PrecompileError::Fatal("V1 measurement-transition preflight was bypassed".into())
    })?;
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
        .ok_or_else(|| PrecompileError::Fatal("upgrade preflight missing".into()))?;
    let node_signature = preflight
        .node_signature
        .try_into()
        .map_err(|_| PrecompileError::Fatal("node signature preflight".into()))?;
    let enclave_signature = preflight
        .enclave_signature
        .try_into()
        .map_err(|_| PrecompileError::Fatal("enclave signature preflight".into()))?;
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
            .map_err(|error| {
                PrecompileError::Fatal(format!("active V1 policy cannot be encoded: {error}"))
            })
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
            .map_err(|error| {
                PrecompileError::Fatal(format!(
                    "staged successor V1 policy cannot be encoded: {error}"
                ))
            })?;
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
    let binding =
        ValidatorNodeBindingV1::decode_canonical(preflight.validator_node_binding.ok_or_else(
            || PrecompileError::Fatal("V1 registration binding preflight was bypassed".into()),
        )?)
        .map_err(|error| {
            PrecompileError::Revert(format!(
                "validator NodeHost binding is not canonical: {error}"
            ))
        })?;
    let validator_signature: [u8; 65] = preflight
        .validator_signature
        .ok_or_else(|| {
            PrecompileError::Fatal("V1 validator signature preflight was bypassed".into())
        })?
        .try_into()
        .map_err(|_| PrecompileError::Fatal("preflight validator signature mismatch".into()))?;
    let node_binding_signature: [u8; 65] = preflight
        .node_binding_signature
        .ok_or_else(|| {
            PrecompileError::Fatal("V1 NodeHost binding signature preflight was bypassed".into())
        })?
        .try_into()
        .map_err(|_| {
            PrecompileError::Fatal("preflight NodeHost binding signature mismatch".into())
        })?;
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
        let policy = policy
            .ok_or_else(|| PrecompileError::Fatal(format!("V1 {label} preflight was bypassed")))?;
        let preflight = preflight
            .ok_or_else(|| PrecompileError::Fatal(format!("V1 {label} preflight was bypassed")))?;
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
    let decoded = AttestationEvidenceV1::decode_canonical(evidence).map_err(|error| {
        PrecompileError::Revert(format!("attestation evidence is not canonical: {error}"))
    })?;
    let intent = match decoded {
        AttestationEvidenceV1::Dcap(value) => value.intent,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
    };
    let node_id_hash = intent.node_id.node_id_hash().map_err(|error| {
        PrecompileError::Revert(format!("registration node identity is invalid: {error}"))
    })?;
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
        .map_err(|error| {
            PrecompileError::Revert(format!("invalid V1 registry gas dimensions: {error}"))
        })?;
    let intrinsic = schedule
        .maximum_calldata_intrinsic_gas(input_len)
        .map_err(|error| {
            PrecompileError::Revert(format!("invalid V1 calldata gas dimensions: {error}"))
        })?;
    let protocol = maximum_transaction_gas
        .checked_sub(intrinsic)
        .ok_or_else(|| PrecompileError::Fatal("V1 protocol gas underflow".into()))?;
    let storage_allowance = schedule.mutator_storage_gas_allowance(kind);
    let prepaid_protocol = protocol.checked_sub(storage_allowance).ok_or_else(|| {
        PrecompileError::Fatal("V1 mutator storage allowance exceeds fixed gas".into())
    })?;
    let dispatch_charge = prepaid_protocol
        .checked_sub(PRECOMPILE_BASE_GAS)
        .ok_or_else(|| {
            PrecompileError::Fatal("V1 protocol gas is below the precompile base charge".into())
        })?;
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
mod tests {
    use super::*;
    use crate::v1_tests::{
        assert_metered_writes, assert_normative_gas, hardening_policy, meter_production_gas,
        normative_budget, successor_policy, MeteredCall,
    };
    use alloy_sol_types::SolValue;
    use outbe_primitives::{
        chain::TESTNET_CHAIN_ID,
        storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider},
        tee_attestation_v1::{
            AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
            NodeIdV1, RegistrationIntentV1, ResourceScheduleV1, TeeMeasurementRuleV1, TeePolicyV1,
            MAX_ATTESTATION_EVIDENCE_BYTES,
        },
    };

    const GENESIS: B256 = B256::repeat_byte(0x31);
    const CHAIN_ID: u64 = TESTNET_CHAIN_ID;

    fn policy() -> TeePolicyV1 {
        let resources = ResourceScheduleV1::normative().unwrap();
        TeePolicyV1 {
            intel_root_der_hash: B256::repeat_byte(0x43),
            resource_schedule_hash: resources.schedule_hash().unwrap(),
            measurement_rules: vec![TeeMeasurementRuleV1 {
                mrenclave: B256::repeat_byte(0x45),
                mrsigner: B256::repeat_byte(0x46),
                isv_prod_id: 7,
                minimum_isv_svn: 2,
                admit_from_height: 1,
                admit_until_height_exclusive: 1_000,
            }],
            ..hardening_policy(GENESIS)
        }
    }

    /// A new chain at `block_number` with `policy` installed.
    fn installed_provider(policy: &TeePolicyV1, block_number: u64) -> HashMapStorageProvider {
        let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
        provider.set_block_number(block_number);
        provider
            .enter(|storage| TeeRegistry::new(storage).install_initial_policy_v1(policy))
            .unwrap();
        provider
    }

    /// Dispatches `input` from the caller `caller_byte` on `provider` and asserts
    /// a revert (`message` on failure).
    fn assert_dispatch_reverts(
        provider: &mut HashMapStorageProvider,
        input: &[u8],
        caller_byte: u8,
        message: &str,
    ) {
        let result = provider.enter(|storage| {
            dispatch(
                storage,
                input,
                Address::repeat_byte(caller_byte),
                U256::ZERO,
            )
        });
        assert!(
            matches!(result, Err(PrecompileError::Revert(_))),
            "{message}: {result:?}"
        );
    }

    /// The metered charge of the first reverted register dispatch.
    struct PrechargedRevert {
        /// The metered policy reads of the dispatch.
        policy_reads: u64,
        /// The precharge plus the gas of the policy reads.
        exact_dispatch_gas: u64,
    }

    impl PrechargedRevert {
        /// Asserts that the reverted register dispatch on `provider` metered
        /// policy reads and no write, and that it charged `dispatch_charge` plus
        /// the gas of those reads.
        fn assert_metered(provider: &HashMapStorageProvider, dispatch_charge: u64) -> Self {
            let policy_reads = assert_metered_writes(
                provider,
                0,
                "a rejected register must not write registry state",
            );
            let exact_dispatch_gas = dispatch_charge + policy_reads * 100;
            assert_eq!(provider.gas_used(), exact_dispatch_gas);
            Self {
                policy_reads,
                exact_dispatch_gas,
            }
        }

        /// Sets the gas limit of `provider` to the exact dispatch gas and repeats
        /// the reverted dispatch of `input` from `caller_byte`. Asserts the same
        /// gas and the same metered reads.
        fn assert_exact_gas_reverts(
            &self,
            provider: &mut HashMapStorageProvider,
            input: &[u8],
            caller_byte: u8,
            message: &str,
        ) {
            provider.set_gas_limit(self.exact_dispatch_gas);
            assert_dispatch_reverts(provider, input, caller_byte, message);
            assert_eq!(provider.gas_used(), self.exact_dispatch_gas);
            assert_eq!(
                provider.metered_storage_operations(),
                (self.policy_reads, 0)
            );
        }
    }

    #[test]
    fn staged_successor_view_is_canonical_and_distinguishes_absence() {
        let current = policy();
        let mut provider = installed_provider(&current, 10);
        let call = ITeeRegistryV1::stagedSuccessorPolicyV1Call {};
        let empty = provider
            .enter(|storage| dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        let empty =
            ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&empty).unwrap();
        assert!(!empty.exists);
        assert!(empty.proposalId.is_zero());
        assert!(empty.policy.is_empty());

        let successor = successor_policy(&current);
        provider
            .enter(|storage| {
                TeeRegistry::new(storage).stage_successor_policy_v1(U256::from(7), &successor)
            })
            .unwrap();
        let encoded = provider
            .enter(|storage| dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        let staged =
            ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&encoded).unwrap();
        assert!(staged.exists);
        assert_eq!(staged.proposalId, U256::from(7));
        assert_eq!(
            TeePolicyV1::decode_canonical(&staged.policy).unwrap(),
            successor
        );
    }

    fn call(evidence: Vec<u8>, node_len: usize, enclave_len: usize) -> Vec<u8> {
        mutator_call(
            RegistryMutatorV1::RegisterEnclave,
            evidence,
            node_len,
            enclave_len,
        )
    }

    fn mutator_call(
        kind: RegistryMutatorV1,
        evidence: Vec<u8>,
        node_len: usize,
        enclave_len: usize,
    ) -> Vec<u8> {
        let node_signature = Bytes::from(vec![0x51; node_len]);
        let enclave_signature = Bytes::from(vec![0x52; enclave_len]);
        match kind {
            RegistryMutatorV1::PrepareEnclaveUpgrade => ITeeRegistryV1::prepareEnclaveUpgradeCall {
                evidence: Bytes::from(evidence),
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode(),
            RegistryMutatorV1::RegisterEnclave => ITeeRegistryV1::registerEnclaveCall {
                evidence: Bytes::from(evidence),
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
                validatorNodeBinding: Bytes::from(vec![
                    0x53;
                    ValidatorNodeBindingV1::CANONICAL_LEN
                ]),
                validatorSignature: Bytes::from(vec![0x54; 65]),
                nodeBindingSignature: Bytes::from(vec![0x55; 65]),
            }
            .abi_encode(),
            RegistryMutatorV1::RenewEnclave => ITeeRegistryV1::renewEnclaveCall {
                evidence: Bytes::from(evidence),
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode(),
            RegistryMutatorV1::ReplaceEnclaveBinding => ITeeRegistryV1::replaceEnclaveBindingCall {
                evidence: Bytes::from(evidence),
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode(),
            RegistryMutatorV1::TransitionEnclaveMeasurement => {
                ITeeRegistryV1::transitionEnclaveMeasurementCall {
                    evidence: Bytes::from(evidence),
                    nodeSignature: node_signature,
                    enclaveSignature: enclave_signature,
                }
                .abi_encode()
            }
        }
    }

    fn canonical_dcap_evidence(kind: RegistryMutatorV1) -> Vec<u8> {
        let policy = policy();
        let node_key = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let reth_p2p_public = node_key.verifying_key().to_encoded_point(true);
        let mut intent = RegistrationIntentV1 {
            chain_id: policy.chain_id,
            genesis_hash: policy.genesis_hash,
            operation: match kind {
                RegistryMutatorV1::PrepareEnclaveUpgrade => {
                    AttestationOperationV1::PrepareEnclaveUpgrade
                }
                RegistryMutatorV1::RegisterEnclave => AttestationOperationV1::RegisterEnclave,
                RegistryMutatorV1::RenewEnclave => AttestationOperationV1::RenewEnclave,
                RegistryMutatorV1::TransitionEnclaveMeasurement => {
                    AttestationOperationV1::TransitionEnclaveMeasurement
                }
                RegistryMutatorV1::ReplaceEnclaveBinding => {
                    AttestationOperationV1::ReplaceEnclaveBinding
                }
            },
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: policy.policy_hash().unwrap(),
            node_id: NodeIdV1 {
                reth_p2p_public: reth_p2p_public.as_bytes().try_into().unwrap(),
            },
            enclave_id: B256::repeat_byte(1),
            binding_id: B256::repeat_byte(2),
            binding_version: 1,
            registration_version: 0,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 7_200,
            recipient_x25519: [3; 32],
            attestation_ed25519: [4; 32],
            noise_responder_x25519: [5; 32],
            node_host_authorization_hash: B256::repeat_byte(6),
        };
        intent.enclave_id = intent.derived_enclave_id().unwrap();
        AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: vec![7],
            components: outbe_tee::test_utils::canonical_dcap_collateral_fixture(),
            transition_key_ready_proof: None,
        })
        .encode_canonical()
        .unwrap()
    }

    #[test]
    fn canonical_register_preflight_is_borrowed_and_rejects_noncanonical_framing() {
        let encoded = call(vec![1, 2, 3], 65, 64);
        assert_eq!(
            preflight_evidence_mutator_call(&encoded).unwrap().evidence,
            [1, 2, 3]
        );

        let mut wrong_offset = encoded.clone();
        wrong_offset[35] = 0x80;
        assert!(preflight_evidence_mutator_call(&wrong_offset).is_err());

        let mut nonzero_padding = encoded;
        let evidence_value_start = 4 + 96 + 32;
        nonzero_padding[evidence_value_start + 3] = 1;
        assert!(preflight_evidence_mutator_call(&nonzero_padding).is_err());
        assert!(preflight_evidence_mutator_call(&call(vec![1], 64, 64)).is_err());
        assert!(preflight_evidence_mutator_call(&call(vec![1], 65, 63)).is_err());
    }

    #[test]
    fn register_precharges_exact_protocol_gas_before_evidence_decode() {
        let mut provider = installed_provider(&policy(), 1);
        meter_production_gas(&mut provider);

        // Canonical outer ABI, deliberately malformed canonical evidence. The
        // decoder rejection must occur only after the complete QVL/register
        // protocol charge is reserved.
        let input = call(vec![1, 1, 0, 0, 0, 0], 65, 64);
        let budget = normative_budget(
            RegistryMutatorV1::RegisterEnclave,
            input.len(),
            6,
            &policy(),
        );
        let dispatch_charge =
            budget.maximum - budget.intrinsic - PRECOMPILE_BASE_GAS - budget.allowance;

        // Actual production-shaped storage gas consumes the allowance already
        // included in `register_fixed`. It must never sit above the normative
        // maximum. The malformed canonical evidence is rejected before verifier
        // invocation. No state write is reachable.
        assert_dispatch_reverts(
            &mut provider,
            &input,
            0x77,
            "malformed canonical evidence framing must revert",
        );
        let precharged = PrechargedRevert::assert_metered(&provider, dispatch_charge);
        assert_normative_gas(
            &provider,
            &[MeteredCall {
                kind: RegistryMutatorV1::RegisterEnclave,
                calldata: &input,
            }],
            6,
            &policy(),
        );

        precharged.assert_exact_gas_reverts(
            &mut provider,
            &input,
            0x88,
            "exact-gas malformed evidence must still revert",
        );

        provider.set_gas_limit(precharged.exact_dispatch_gas - 1);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
        assert_eq!(provider.gas_used(), precharged.policy_reads * 100);
        assert_eq!(
            provider.metered_storage_operations(),
            (precharged.policy_reads, 0)
        );
    }

    #[test]
    fn gramine_direct_dev_register_precharge_uses_active_policy_mode() {
        let mut active_policy = policy();
        active_policy.attestation_mode = AttestationMode::GramineDirectDev;
        let mut provider = installed_provider(&active_policy, 1);
        meter_production_gas(&mut provider);

        // Canonical outer ABI with malformed development evidence reaches the
        // mode-selected precharge and then reverts during evidence decoding.
        // The exact GramineDirectDev maximum must be sufficient; charging the
        // production DCAP schedule here makes the reachable dev transaction
        // consume its entire signed gas limit before validation.
        let input = call(vec![1, 1, 0, 0, 0, 0], 65, 64);
        let budget = normative_budget(
            RegistryMutatorV1::RegisterEnclave,
            input.len(),
            6,
            &active_policy,
        );
        let dispatch_charge =
            budget.maximum - budget.intrinsic - PRECOMPILE_BASE_GAS - budget.allowance;

        assert_dispatch_reverts(
            &mut provider,
            &input,
            0xA7,
            "malformed development evidence must revert after its mode-selected precharge",
        );
        let precharged = PrechargedRevert::assert_metered(&provider, dispatch_charge);

        precharged.assert_exact_gas_reverts(
            &mut provider,
            &input,
            0xA8,
            "exact GramineDirectDev gas must reach canonical evidence rejection",
        );
    }

    #[test]
    fn unavailable_verifier_never_mutates_or_fails_open_for_renewal_or_replacement() {
        for kind in [
            RegistryMutatorV1::RenewEnclave,
            RegistryMutatorV1::ReplaceEnclaveBinding,
        ] {
            let mut provider = installed_provider(&policy(), 1);
            meter_production_gas(&mut provider);

            let evidence = canonical_dcap_evidence(kind);
            let evidence_len = evidence.len();
            let input = mutator_call(kind, evidence, 65, 64);
            let budget = normative_budget(kind, input.len(), evidence_len, &policy());

            let result = provider
                .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0xA1), U256::ZERO));
            assert!(
                matches!(result, Err(PrecompileError::Fatal(_))),
                "unexpected pre-verifier result for {kind:?}: {result:?}"
            );
            let reads = assert_metered_writes(
                &provider,
                0,
                "verifier outage must not extend registry state",
            );
            let storage_gas = reads * 100;
            assert_eq!(
                budget.intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used(),
                budget.maximum - budget.allowance + storage_gas
            );
            assert!(storage_gas <= budget.allowance);
            assert!(budget.intrinsic + PRECOMPILE_BASE_GAS + provider.gas_used() <= budget.maximum);
        }
    }

    #[test]
    fn cap_plus_one_rejects_before_policy_read_or_gas_charge() {
        let input = call(vec![0; MAX_ATTESTATION_EVIDENCE_BYTES + 1], 65, 64);
        let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
        provider.set_gas_limit(u64::MAX);
        let result = provider
            .enter(|storage| dispatch(storage, &input, Address::repeat_byte(0x99), U256::ZERO));
        assert!(matches!(result, Err(PrecompileError::Revert(_))));
        assert_eq!(provider.gas_used(), 0);
    }

    #[test]
    fn empty_binding_views_are_fixed_and_not_ready() {
        let validator = Address::repeat_byte(0x61);
        let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS);
        let ready_call = ITeeRegistryV1::isValidatorEnclaveReadyCall { validator };
        let ready = provider
            .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        assert!(!bool::abi_decode(&ready).unwrap());

        let binding_call = ITeeRegistryV1::validatorEnclaveBindingCall { validator };
        let encoded = provider
            .enter(|storage| {
                dispatch(
                    storage,
                    &binding_call.abi_encode(),
                    Address::ZERO,
                    U256::ZERO,
                )
            })
            .unwrap();
        let binding = NodeEnclaveBindingV1View::abi_decode(&encoded).unwrap();
        assert!(!binding.exists);
        assert_eq!(binding.nodeIdHash, B256::ZERO);
        assert_eq!(encoded.len(), 24 * 32);

        let p2p_signing = k256::ecdsa::SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
        let encoded_public = p2p_signing.verifying_key().to_encoded_point(true);
        let reth_p2p_prefix = encoded_public.as_bytes()[0];
        let reth_p2p_x = B256::from_slice(&encoded_public.as_bytes()[1..]);
        let ready_call = ITeeRegistryV1::isNodeHostEnclaveReadyCall {
            rethP2pPrefix: reth_p2p_prefix,
            rethP2pX: reth_p2p_x,
        };
        let ready = provider
            .enter(|storage| dispatch(storage, &ready_call.abi_encode(), Address::ZERO, U256::ZERO))
            .unwrap();
        assert!(!bool::abi_decode(&ready).unwrap());

        let binding_call = ITeeRegistryV1::nodeHostEnclaveBindingCall {
            rethP2pPrefix: reth_p2p_prefix,
            rethP2pX: reth_p2p_x,
        };
        let encoded = provider
            .enter(|storage| {
                dispatch(
                    storage,
                    &binding_call.abi_encode(),
                    Address::ZERO,
                    U256::ZERO,
                )
            })
            .unwrap();
        assert!(
            !NodeEnclaveBindingV1View::abi_decode(&encoded)
                .unwrap()
                .exists
        );

        let invalid = ITeeRegistryV1::isNodeHostEnclaveReadyCall {
            rethP2pPrefix: 0x04,
            rethP2pX: reth_p2p_x,
        };
        assert!(provider
            .enter(|storage| dispatch(storage, &invalid.abi_encode(), Address::ZERO, U256::ZERO))
            .is_err());
    }
}
