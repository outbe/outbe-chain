use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use ed25519_dalek::Signer as _;
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_primitives::{
    chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID},
    error::PrecompileError,
    signer::OutbeEvmSigner,
    storage::{hashmap::HashMapStorageProvider, PrecompileStorageProvider, StorageHandle},
    tee_attestation_v1::{
        AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapCollateralComponentV1,
        DcapCollateralKind, DcapEvidenceV1, EnclaveInitializationManifestV1, NodeIdV1,
        PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1, RegistryMutatorV1,
        TeeMeasurementRuleV1, TeePolicyV1, TeeRegistryGasScheduleV1, TransitionKeyReadyProofV1,
        ValidatorNodeBindingV1,
    },
};
use outbe_tee::dcap_protocol::{
    DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapPckCaV1, DcapPlatformTcbStatusV1,
    DcapVerdictV1,
};
use outbe_tee::finalized_admission::{
    TEE_REGISTRY_KEY_EPOCH_SLOT_V1, TEE_REGISTRY_NODE_BINDING_ID_SLOT_V1,
    TEE_REGISTRY_NODE_ENCLAVE_ID_SLOT_V1, TEE_REGISTRY_NODE_INTENT_HASH_SLOT_V1,
    TEE_REGISTRY_NODE_POLICY_HASH_SLOT_V1, TEE_REGISTRY_NODE_RECIPIENT_X25519_SLOT_V1,
    TEE_REGISTRY_NODE_VALID_UNTIL_SLOT_V1, TEE_REGISTRY_OFFER_EPOCH_SLOT_V1,
    TEE_REGISTRY_OFFER_PUBLIC_SLOT_V1,
};
use outbe_validatorset::contract::ValidatorSet;

use crate::{
    runtime::TeeBootstrapData,
    schema::TeeRegistry,
    v1::{
        OfferKeySealedForRegistryV1, PostVerifierDcapCapabilityV1, V1OnboardingOutcome,
        V1RegistrationOutcome,
    },
    v1_precompile::{
        dispatch_register_after_verifier_for_test,
        dispatch_register_with_onboarding_after_verifier_for_test,
        dispatch_renew_after_verifier_for_test, dispatch_replace_after_verifier_for_test,
        dispatch_transition_after_verifier_for_test,
    },
};

mod fixtures;
use fixtures::{
    full_node_public, full_node_registration_intent, full_node_signatures,
    initialization_manifest_for_intent, measurement_transition_intent, policy,
    register_same_key_node_for_lifecycle_test, register_validator, registration_intent,
    renewal_intent, replacement_intent, reth_p2p_public_for_evm_signer, revert_message, signatures,
    storage, storage_for_chain, validator_node_binding_authorization_for_evm_node,
    validator_node_binding_authorization_for_p2p_node, verdict, IRegisterEnclaveV1Test,
    CONSENSUS_KEY, NOW, OFFER_PUBLIC,
};

mod layout;
mod lease;
mod onboarding;
mod policy;
mod registration;
mod rejoin;
mod replacement;
mod replica_parity;
mod transitions;
