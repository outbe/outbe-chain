use super::*;
use crate::v1::{NodeEnclaveBindingV1, NodeHostAssociationV1, VerifiedIntentV1};

sol! {
    interface IRegisterEnclaveV1Test {
        function registerEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature,
            bytes calldata validatorNodeBinding,
            bytes calldata validatorSignature,
            bytes calldata nodeBindingSignature
        ) external returns (bool);

        function renewEnclave(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function replaceEnclaveBinding(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);

        function transitionEnclaveMeasurement(
            bytes calldata evidence,
            bytes calldata nodeSignature,
            bytes calldata enclaveSignature
        ) external returns (bool);
    }
}

const CHAIN_ID: u64 = TESTNET_CHAIN_ID;

pub(super) const NOW: u64 = 10_000;

const MRENCLAVE: B256 = B256::repeat_byte(0x81);

const MRSIGNER: B256 = B256::repeat_byte(0x82);

pub(super) const CONSENSUS_KEY: [u8; 48] = [0x32; 48];

const NODE_HOST_NOISE_X25519: [u8; 32] = [0xa5; 32];

pub(super) const OFFER_PUBLIC: [u8; 32] = [0xb1; 32];

pub(super) fn policy(genesis_hash: B256, statuses: PlatformTcbStatusSetV1) -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(CHAIN_ID).to_be_bytes(),
        genesis_hash,
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x71),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: statuses,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x72),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: MRENCLAVE,
            mrsigner: MRSIGNER,
            isv_prod_id: 7,
            minimum_isv_svn: 3,
            admit_from_height: 1,
            admit_until_height_exclusive: 100,
        }],
    }
}

/// The test policy of `genesis_hash` that admits up-to-date platforms and
/// platforms that need hardening. It also gives the Intel DCAP v3 quote header
/// to the policy fixture of the `v1_precompile` tests.
pub(crate) fn hardening_policy(genesis_hash: B256) -> TeePolicyV1 {
    policy(
        genesis_hash,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
    )
}

/// The test policy with a maximum lease of one hour.
pub(super) fn capped_lease_policy(genesis_hash: B256) -> TeePolicyV1 {
    let mut policy = hardening_policy(genesis_hash);
    policy.maximum_lease = 3_600;
    policy
}

/// The test policy with a lease of exactly one hour.
pub(super) fn fixed_lease_policy(genesis_hash: B256) -> TeePolicyV1 {
    let mut policy = hardening_policy(genesis_hash);
    policy.minimum_lease = 3_600;
    policy.maximum_lease = 3_600;
    policy
}

/// Policy version 2 after `current`. It activates at height 50.
pub(crate) fn successor_policy(current: &TeePolicyV1) -> TeePolicyV1 {
    let mut successor = current.clone();
    successor.policy_version = 2;
    successor.activation_height = 50;
    successor.predecessor_policy_hash = current.policy_hash().unwrap();
    successor
}

/// [`successor_policy`] whose measurement rules admit from height 50 until
/// height 500.
pub(super) fn windowed_successor(current: &TeePolicyV1) -> TeePolicyV1 {
    let mut successor = successor_policy(current);
    for rule in &mut successor.measurement_rules {
        rule.admit_from_height = 50;
        rule.admit_until_height_exclusive = 500;
    }
    successor
}

/// [`windowed_successor`] that admits only `mrenclave`.
pub(super) fn measurement_successor(current: &TeePolicyV1, mrenclave: B256) -> TeePolicyV1 {
    let mut successor = windowed_successor(current);
    for rule in &mut successor.measurement_rules {
        rule.mrenclave = mrenclave;
    }
    successor
}

/// A new registry on `storage` with `policy` installed as the initial policy.
pub(super) fn installed_registry<'a>(
    storage: StorageHandle<'a>,
    policy: &TeePolicyV1,
) -> TeeRegistry<'a> {
    let mut registry = TeeRegistry::new(storage);
    registry.install_initial_policy_v1(policy).unwrap();
    registry
}

/// Runs `test` on a new chain of the genesis of `policy`. Before `test`, it
/// runs `before_install` and then installs `policy`. Returns the provider of
/// the chain.
fn run_on_new_chain(
    policy: &TeePolicyV1,
    before_install: impl FnOnce(StorageHandle<'_>),
    test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
) -> HashMapStorageProvider {
    let mut provider = storage(policy.genesis_hash);
    StorageHandle::enter(&mut provider, |storage| {
        before_install(storage.clone());
        let registry = installed_registry(storage.clone(), policy);
        test(storage, registry);
    });
    provider
}

/// Runs `test` on a new chain of the genesis of `policy` with `policy`
/// installed. Returns the provider of the chain.
pub(super) fn run_installed(
    policy: &TeePolicyV1,
    test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
) -> HashMapStorageProvider {
    run_on_new_chain(policy, |_| {}, test)
}

/// Runs `test` on a new chain of the genesis of `policy`. Before `test`, it
/// registers `node_signer` as a validator with [`CONSENSUS_KEY`] and installs
/// `policy`. Returns the provider of the chain.
pub(super) fn run_as_validator_of(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
) -> HashMapStorageProvider {
    run_on_new_chain(
        policy,
        |storage| register_validator(storage, node_signer, CONSENSUS_KEY),
        test,
    )
}

/// [`run_as_validator_of`] that registers the lifecycle binding of `initial`
/// before `test`.
pub(super) fn run_as_validator_with_binding_of(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    initial: VerifiedIntentV1<'_>,
    test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
) -> HashMapStorageProvider {
    run_as_validator_of(policy, node_signer, |storage, mut registry| {
        register_same_key_node_for_lifecycle_test(&mut registry, node_signer, initial).unwrap();
        test(storage, registry);
    })
}

/// The enclave binding that `registry` stores for `validator`.
pub(super) fn validator_binding(
    registry: &TeeRegistry<'_>,
    validator: Address,
) -> NodeEnclaveBindingV1 {
    registry
        .validator_enclave_binding_v1(validator)
        .unwrap()
        .unwrap()
}

pub(super) fn storage(genesis_hash: B256) -> HashMapStorageProvider {
    storage_for_chain(CHAIN_ID, genesis_hash)
}

pub(super) fn storage_for_chain(chain_id: u64, genesis_hash: B256) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new_with_chain_identity(chain_id, genesis_hash);
    storage.set_block_number(10);
    storage.set_timestamp(U256::from(NOW));
    storage
}

pub(super) fn register_validator(
    storage: StorageHandle<'_>,
    signer: &OutbeEvmSigner,
    consensus_key: [u8; 48],
) {
    ValidatorSet::new(storage)
        .register_validator(Address::ZERO, signer.address(), &consensus_key)
        .expect("genesis-owner validator registration");
}

pub(super) fn reth_p2p_public_for_evm_signer(node_signer: &OutbeEvmSigner) -> [u8; 33] {
    let proof_hash = B256::repeat_byte(0xA7);
    let proof = node_signer.sign_hash(&proof_hash).unwrap();
    let proof_signature = k256::ecdsa::Signature::from_slice(&proof[..64]).unwrap();
    let proof_recovery = k256::ecdsa::RecoveryId::from_byte(proof[64]).unwrap();
    k256::ecdsa::VerifyingKey::recover_from_prehash(
        proof_hash.as_slice(),
        &proof_signature,
        proof_recovery,
    )
    .unwrap()
    .to_encoded_point(true)
    .as_bytes()
    .try_into()
    .unwrap()
}

pub(super) fn initialization_manifest_for_intent(
    intent: &RegistrationIntentV1,
    challenge: [u8; 32],
) -> EnclaveInitializationManifestV1 {
    EnclaveInitializationManifestV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        attestation_mode: intent.attestation_mode,
        node_id: intent.node_id.clone(),
        initialization_challenge: challenge,
        node_host_noise_x25519: NODE_HOST_NOISE_X25519,
        recipient_x25519: intent.recipient_x25519,
        attestation_ed25519: intent.attestation_ed25519,
        noise_responder_x25519: intent.noise_responder_x25519,
    }
}

/// The initial DCAP registration intent under `policy` for the enclave of
/// `manifest`, with the binding of `binding_seed`.
pub(super) fn initial_intent_for_manifest(
    policy: &TeePolicyV1,
    manifest: &EnclaveInitializationManifestV1,
    binding_seed: u8,
) -> RegistrationIntentV1 {
    RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: AttestationMode::DcapRequired,
        policy_hash: policy.policy_hash().unwrap(),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(binding_seed),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: NOW + 3_600,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    }
}

#[derive(Clone, Copy)]
pub(super) struct EnclaveBindingSeeds {
    binding: u8,
    key: u8,
}

impl EnclaveBindingSeeds {
    pub(super) const fn new(binding: u8, key: u8) -> Self {
        Self { binding, key }
    }
}

/// A validator with its node key, its enclave key and its initial
/// registration intent under `policy`.
pub(super) struct LifecycleValidator {
    pub(super) policy: TeePolicyV1,
    pub(super) node_signer: OutbeEvmSigner,
    pub(super) enclave_signer: ed25519_dalek::SigningKey,
    pub(super) initial: RegistrationIntentV1,
}

impl LifecycleValidator {
    /// Derives the node key from `node_seed` and the enclave key from
    /// `enclave_seed`. Then makes the initial intent of the two keys under
    /// `policy`.
    pub(super) fn new(
        policy: TeePolicyV1,
        node_seed: u8,
        enclave_seed: u8,
        seeds: EnclaveBindingSeeds,
    ) -> Self {
        let node_signer = OutbeEvmSigner::from_secret_bytes([node_seed; 32]).unwrap();
        let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[enclave_seed; 32]);
        let initial = registration_intent(&policy, &node_signer, &enclave_signer, seeds);
        Self {
            policy,
            node_signer,
            enclave_signer,
            initial,
        }
    }

    /// Signs `intent` with the node key and the enclave key.
    pub(super) fn sign<'a>(&self, intent: &'a RegistrationIntentV1) -> SignedIntent<'a> {
        self.sign_with_enclave(intent, &self.enclave_signer)
    }

    /// Signs `intent` with the node key and `enclave_signer`.
    pub(super) fn sign_with_enclave<'a>(
        &self,
        intent: &'a RegistrationIntentV1,
        enclave_signer: &ed25519_dalek::SigningKey,
    ) -> SignedIntent<'a> {
        SignedIntent::by_validator(intent, &self.node_signer, enclave_signer)
    }

    /// Signs the initial intent.
    pub(super) fn signed_initial(&self) -> SignedIntent<'_> {
        self.sign(&self.initial)
    }

    /// [`renewal_then_replacement`] for the initial intent under the policy.
    pub(super) fn renewal_then_replacement(
        &self,
        replacement_enclave: &ed25519_dalek::SigningKey,
        seeds: EnclaveBindingSeeds,
        requested_valid_until: u64,
    ) -> (RegistrationIntentV1, RegistrationIntentV1) {
        renewal_then_replacement(
            &self.initial,
            &self.policy,
            replacement_enclave,
            seeds,
            requested_valid_until,
        )
    }

    /// The authorization of the validator for its own node of `intent`.
    pub(super) fn association(&self, intent: &RegistrationIntentV1) -> NodeAssociation {
        validator_node_binding_authorization_for_evm_node(
            intent,
            &self.node_signer,
            &self.node_signer,
        )
    }

    /// The `registerEnclave` calldata for `intent`. The node key and
    /// `enclave_signer` sign it, and the validator authorizes its own node.
    pub(super) fn register_call(
        &self,
        intent: &RegistrationIntentV1,
        enclave_signer: &ed25519_dalek::SigningKey,
        evidence: &[u8],
    ) -> Vec<u8> {
        let signed = self.sign_with_enclave(intent, enclave_signer);
        let association = self.association(intent);
        register_calldata(evidence, &signed, &association)
    }

    /// The enclave binding that `registry` stores for the validator.
    pub(super) fn stored_binding(&self, registry: &TeeRegistry<'_>) -> NodeEnclaveBindingV1 {
        validator_binding(registry, self.node_signer.address())
    }

    /// Runs `rejected` on `registry`. Asserts that the stored binding after
    /// `rejected` equals the stored binding before it.
    pub(super) fn assert_binding_unchanged_by(
        &self,
        registry: &mut TeeRegistry<'_>,
        rejected: impl FnOnce(&mut TeeRegistry<'_>),
    ) {
        let before = self.stored_binding(registry);
        rejected(registry);
        assert_eq!(self.stored_binding(registry), before);
    }

    /// [`run_as_validator_of`] for the node key under the policy.
    pub(super) fn run_as_validator(
        &self,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        run_as_validator_of(&self.policy, &self.node_signer, test)
    }

    /// Runs `test` on a new chain of the policy genesis. Before `test`, it
    /// registers the node key as a validator, then registers `other_node` with
    /// `other_consensus_key`, and then installs the policy. Returns the provider
    /// of the chain.
    pub(super) fn run_with_second_validator(
        &self,
        other_node: &OutbeEvmSigner,
        other_consensus_key: [u8; 48],
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        run_on_new_chain(
            &self.policy,
            |storage| {
                register_validator(storage.clone(), &self.node_signer, CONSENSUS_KEY);
                register_validator(storage, other_node, other_consensus_key);
            },
            test,
        )
    }

    /// Runs `test` on a new chain of the policy genesis with the policy
    /// installed and the lifecycle binding of `initial` registered. Returns the
    /// provider of the chain.
    pub(super) fn run_with_binding(
        &self,
        initial: VerifiedIntentV1<'_>,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        run_installed(&self.policy, |storage, mut registry| {
            register_same_key_node_for_lifecycle_test(&mut registry, &self.node_signer, initial)
                .unwrap();
            test(storage, registry);
        })
    }

    /// [`run_as_validator_with_binding_of`] for the node key under the policy.
    pub(super) fn run_as_validator_with_binding(
        &self,
        initial: VerifiedIntentV1<'_>,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        run_as_validator_with_binding_of(&self.policy, &self.node_signer, initial, test)
    }

    /// [`Self::run_with_binding`] that sets the block timestamp to `timestamp`
    /// before `test`.
    pub(super) fn run_with_binding_at(
        &self,
        timestamp: u64,
        initial: VerifiedIntentV1<'_>,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        self.run_with_binding(initial, |storage, registry| {
            storage.set_block_timestamp(U256::from(timestamp)).unwrap();
            test(storage, registry);
        })
    }

    /// [`Self::run_as_validator_with_binding`] that sets the block timestamp to
    /// `timestamp` before `test`.
    pub(super) fn run_as_validator_with_binding_at(
        &self,
        timestamp: u64,
        initial: VerifiedIntentV1<'_>,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        self.run_as_validator_with_binding(initial, |storage, registry| {
            storage.set_block_timestamp(U256::from(timestamp)).unwrap();
            test(storage, registry);
        })
    }

    /// The chain of [`Self::run_with_binding_at`] without a test.
    pub(super) fn provider_with_binding_at(
        &self,
        timestamp: u64,
        initial: VerifiedIntentV1<'_>,
    ) -> HashMapStorageProvider {
        self.run_with_binding_at(timestamp, initial, |_, _| {})
    }

    /// The chain of [`Self::run_as_validator_with_binding_at`] without a test.
    pub(super) fn validator_provider_with_binding_at(
        &self,
        timestamp: u64,
        initial: VerifiedIntentV1<'_>,
    ) -> HashMapStorageProvider {
        self.run_as_validator_with_binding_at(timestamp, initial, |_, _| {})
    }
}

/// A full node with its P2P node key, its enclave key and its initial
/// registration intent under `policy`.
pub(super) struct LifecycleFullNode {
    pub(super) policy: TeePolicyV1,
    pub(super) node_signer: k256::ecdsa::SigningKey,
    pub(super) enclave_signer: ed25519_dalek::SigningKey,
    pub(super) initial: RegistrationIntentV1,
}

impl LifecycleFullNode {
    /// Derives the node key from `node_seed` and the enclave key from
    /// `enclave_seed`. Then makes the initial intent of the two keys under
    /// `policy`.
    pub(super) fn new(
        policy: TeePolicyV1,
        node_seed: u8,
        enclave_seed: u8,
        seeds: EnclaveBindingSeeds,
    ) -> Self {
        let node_signer = k256::ecdsa::SigningKey::from_bytes((&[node_seed; 32]).into()).unwrap();
        let enclave_signer = ed25519_dalek::SigningKey::from_bytes(&[enclave_seed; 32]);
        let initial = full_node_registration_intent(&policy, &node_signer, &enclave_signer, seeds);
        Self {
            policy,
            node_signer,
            enclave_signer,
            initial,
        }
    }

    /// Signs `intent` with the node key and the enclave key.
    pub(super) fn sign<'a>(&self, intent: &'a RegistrationIntentV1) -> SignedIntent<'a> {
        self.sign_with_enclave(intent, &self.enclave_signer)
    }

    /// Signs `intent` with the node key and `enclave_signer`.
    pub(super) fn sign_with_enclave<'a>(
        &self,
        intent: &'a RegistrationIntentV1,
        enclave_signer: &ed25519_dalek::SigningKey,
    ) -> SignedIntent<'a> {
        SignedIntent::by_full_node(intent, &self.node_signer, enclave_signer)
    }

    /// Signs the initial intent.
    pub(super) fn signed_initial(&self) -> SignedIntent<'_> {
        self.sign(&self.initial)
    }

    /// [`renewal_then_replacement`] for the initial intent under the policy.
    pub(super) fn renewal_then_replacement(
        &self,
        replacement_enclave: &ed25519_dalek::SigningKey,
        seeds: EnclaveBindingSeeds,
        requested_valid_until: u64,
    ) -> (RegistrationIntentV1, RegistrationIntentV1) {
        renewal_then_replacement(
            &self.initial,
            &self.policy,
            replacement_enclave,
            seeds,
            requested_valid_until,
        )
    }

    /// [`run_installed`] for the policy.
    pub(super) fn run_installed(
        &self,
        test: impl FnOnce(StorageHandle<'_>, TeeRegistry<'_>),
    ) -> HashMapStorageProvider {
        run_installed(&self.policy, test)
    }

    /// The enclave binding that `registry` stores for the node of the initial
    /// intent.
    pub(super) fn stored_binding(&self, registry: &TeeRegistry<'_>) -> NodeEnclaveBindingV1 {
        registry
            .node_host_enclave_binding_v1(full_node_public(&self.initial))
            .unwrap()
            .unwrap()
    }

    /// The admission of the initial intent by `admission_signer`.
    pub(super) fn association(&self, admission_signer: &OutbeEvmSigner) -> NodeAssociation {
        validator_node_binding_authorization_for_p2p_node(
            &self.initial,
            admission_signer,
            &self.node_signer,
        )
    }

    /// The `registerEnclave` calldata for the initial intent with the admission
    /// of `admission_signer`.
    pub(super) fn register_call(
        &self,
        admission_signer: &OutbeEvmSigner,
        evidence: &[u8],
    ) -> Vec<u8> {
        let signed = self.signed_initial();
        let association = self.association(admission_signer);
        register_calldata(evidence, &signed, &association)
    }
}

pub(super) fn registration_intent(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
) -> RegistrationIntentV1 {
    initial_registration_intent(
        policy,
        || NodeIdV1 {
            reth_p2p_public: reth_p2p_public_for_evm_signer(node_signer),
        },
        enclave_signer,
        seeds,
    )
}

pub(super) fn full_node_registration_intent(
    policy: &TeePolicyV1,
    node_signer: &k256::ecdsa::SigningKey,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
) -> RegistrationIntentV1 {
    let reth_p2p_public = node_signer.verifying_key().to_encoded_point(true);
    initial_registration_intent(
        policy,
        || NodeIdV1 {
            reth_p2p_public: reth_p2p_public.as_bytes().try_into().unwrap(),
        },
        enclave_signer,
        seeds,
    )
}

fn initial_registration_intent(
    policy: &TeePolicyV1,
    node_id: impl FnOnce() -> NodeIdV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
) -> RegistrationIntentV1 {
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        attestation_mode: AttestationMode::DcapRequired,
        node_id: node_id(),
        initialization_challenge: [0xa6; 32],
        node_host_noise_x25519: NODE_HOST_NOISE_X25519,
        recipient_x25519: [seeds.key; 32],
        attestation_ed25519: enclave_signer.verifying_key().to_bytes(),
        noise_responder_x25519: [seeds.key.wrapping_add(1); 32],
    };
    let intent = initial_intent_for_manifest(policy, &manifest, seeds.binding);
    manifest.validate_intent_binding(&intent).unwrap();
    intent
}

/// An intent with the node and enclave signatures over its hash.
pub(super) struct SignedIntent<'a> {
    pub(super) intent: &'a RegistrationIntentV1,
    pub(super) node_signature: [u8; 65],
    pub(super) enclave_signature: [u8; 64],
}

impl<'a> SignedIntent<'a> {
    /// Signs `intent` with a validator EVM node key and `enclave_signer`.
    pub(super) fn by_validator(
        intent: &'a RegistrationIntentV1,
        node_signer: &OutbeEvmSigner,
        enclave_signer: &ed25519_dalek::SigningKey,
    ) -> Self {
        let (node_signature, enclave_signature) = signatures(intent, node_signer, enclave_signer);
        Self {
            intent,
            node_signature,
            enclave_signature,
        }
    }

    /// Signs `intent` with a full-node P2P key and `enclave_signer`.
    pub(super) fn by_full_node(
        intent: &'a RegistrationIntentV1,
        node_signer: &k256::ecdsa::SigningKey,
        enclave_signer: &ed25519_dalek::SigningKey,
    ) -> Self {
        let (node_signature, enclave_signature) =
            full_node_signatures(intent, node_signer, enclave_signer);
        Self {
            intent,
            node_signature,
            enclave_signature,
        }
    }

    /// [`Self::verified`] with the default capability for `verdict`.
    pub(super) fn with_verdict(&self, verdict: DcapVerdictV1) -> VerifiedIntentV1<'_> {
        self.verified(PostVerifierDcapCapabilityV1::new(verdict))
    }

    /// The verifier output for this intent with `verdict` and the evidence hash
    /// `0xED..`. That evidence hash differs from the default evidence hash, so a
    /// replay conflicts with an earlier default submission.
    pub(super) fn with_conflicting_evidence(&self, verdict: DcapVerdictV1) -> VerifiedIntentV1<'_> {
        self.verified(PostVerifierDcapCapabilityV1::with_evidence_hash(
            verdict,
            B256::repeat_byte(0xED),
        ))
    }

    /// The verifier output for this intent with `capability`.
    pub(super) fn verified(
        &self,
        capability: PostVerifierDcapCapabilityV1,
    ) -> VerifiedIntentV1<'_> {
        VerifiedIntentV1 {
            intent: self.intent,
            node_signature: &self.node_signature,
            enclave_signature: &self.enclave_signature,
            capability,
        }
    }
}

/// A validator-to-node binding with the validator and node signatures.
pub(super) struct NodeAssociation {
    pub(super) binding: ValidatorNodeBindingV1,
    pub(super) validator_signature: [u8; 65],
    pub(super) node_binding_signature: [u8; 65],
}

impl NodeAssociation {
    /// The registry input for this association.
    pub(super) fn input(&self) -> NodeHostAssociationV1<'_> {
        NodeHostAssociationV1 {
            binding: &self.binding,
            validator_signature: &self.validator_signature,
            node_binding_signature: &self.node_binding_signature,
        }
    }
}

/// The 65-byte recoverable signature of the P2P key `node_signer` over `hash`.
fn p2p_recoverable_signature(node_signer: &k256::ecdsa::SigningKey, hash: B256) -> [u8; 65] {
    let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = node_signer
        .sign_prehash(hash.as_slice())
        .expect("test P2P key signs the prehash");
    let mut recoverable = [0_u8; 65];
    recoverable[..64].copy_from_slice(signature.to_bytes().as_slice());
    recoverable[64] = recovery.to_byte();
    recoverable
}

pub(super) fn full_node_signatures(
    intent: &RegistrationIntentV1,
    node_signer: &k256::ecdsa::SigningKey,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> ([u8; 65], [u8; 64]) {
    let hash = intent.intent_hash().unwrap();
    (
        p2p_recoverable_signature(node_signer, hash),
        enclave_signer.sign(hash.as_slice()).to_bytes(),
    )
}

/// The binding of the node of `intent` to the validator `admission_signer`.
/// The validator signs the binding hash, and then `node_binding_signature`
/// signs it for the node.
fn node_association(
    intent: &RegistrationIntentV1,
    admission_signer: &OutbeEvmSigner,
    node_binding_signature: impl FnOnce(B256) -> [u8; 65],
) -> NodeAssociation {
    let binding = ValidatorNodeBindingV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        validator: admission_signer.address().into_array(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
    };
    let binding_hash = binding.binding_hash().unwrap();
    let validator_signature = admission_signer.sign_hash(&binding_hash).unwrap();
    let node_binding_signature = node_binding_signature(binding_hash);
    NodeAssociation {
        binding,
        validator_signature,
        node_binding_signature,
    }
}

pub(super) fn validator_node_binding_authorization_for_p2p_node(
    intent: &RegistrationIntentV1,
    admission_signer: &OutbeEvmSigner,
    node_signer: &k256::ecdsa::SigningKey,
) -> NodeAssociation {
    node_association(intent, admission_signer, |binding_hash| {
        p2p_recoverable_signature(node_signer, binding_hash)
    })
}

pub(super) fn validator_node_binding_authorization_for_evm_node(
    intent: &RegistrationIntentV1,
    admission_signer: &OutbeEvmSigner,
    node_signer: &OutbeEvmSigner,
) -> NodeAssociation {
    node_association(intent, admission_signer, |binding_hash| {
        node_signer.sign_hash(&binding_hash).unwrap()
    })
}

pub(super) fn full_node_public(intent: &RegistrationIntentV1) -> [u8; 33] {
    intent.node_id.reth_p2p_public
}

pub(super) fn signatures(
    intent: &RegistrationIntentV1,
    node_signer: &OutbeEvmSigner,
    enclave_signer: &ed25519_dalek::SigningKey,
) -> ([u8; 65], [u8; 64]) {
    let hash = intent.intent_hash().unwrap();
    (
        node_signer.sign_hash(&hash).unwrap(),
        enclave_signer.sign(hash.as_slice()).to_bytes(),
    )
}

pub(super) fn register_same_key_node_for_lifecycle_test(
    registry: &mut TeeRegistry<'_>,
    node_signer: &OutbeEvmSigner,
    verified: VerifiedIntentV1<'_>,
) -> Result<V1RegistrationOutcome, PrecompileError> {
    let association = validator_node_binding_authorization_for_evm_node(
        verified.intent,
        node_signer,
        node_signer,
    );
    registry.register_enclave_and_bind_after_verifier_for_test(verified, association.input())
}

pub(super) fn verdict(status: DcapPlatformTcbStatusV1) -> DcapVerdictV1 {
    DcapVerdictV1 {
        mrenclave: MRENCLAVE,
        mrsigner: MRSIGNER,
        isv_prod_id: 7,
        isv_svn: 4,
        pck_ca: DcapPckCaV1::Processor,
        fmspc: [0x91; 6],
        pce_id: 2,
        platform_tcb_status: status,
        advisory_ids: Vec::new(),
        tcb_evaluation_data_number: 17,
        qe_tcb_evaluation_data_number: 17,
        collateral_valid_until: NOW + 7_200,
    }
}

/// An up-to-date verdict whose collateral stays valid until
/// `collateral_valid_until`.
pub(super) fn up_to_date_verdict_until(collateral_valid_until: u64) -> DcapVerdictV1 {
    let mut verdict = verdict(DcapPlatformTcbStatusV1::UpToDate);
    verdict.collateral_valid_until = collateral_valid_until;
    verdict
}

pub(super) fn renewal_intent(
    current: &RegistrationIntentV1,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = AttestationOperationV1::RenewEnclave;
    intent.registration_version += 1;
    intent.renewal_nonce += 1;
    intent.requested_valid_until = requested_valid_until;
    intent
}

/// The renewal of `current` for one maximum lease of `policy` from the current
/// deadline.
pub(super) fn max_lease_renewal(
    current: &RegistrationIntentV1,
    policy: &TeePolicyV1,
) -> RegistrationIntentV1 {
    renewal_intent(
        current,
        current.requested_valid_until + policy.maximum_lease,
    )
}

/// The maximum-lease renewal of `initial` under `policy`, and then the
/// replacement of that renewal with `replacement_enclave`.
fn renewal_then_replacement(
    initial: &RegistrationIntentV1,
    policy: &TeePolicyV1,
    replacement_enclave: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
    requested_valid_until: u64,
) -> (RegistrationIntentV1, RegistrationIntentV1) {
    let renewal = max_lease_renewal(initial, policy);
    let replacement =
        replacement_intent(&renewal, replacement_enclave, seeds, requested_valid_until);
    (renewal, replacement)
}

/// `current` changed to its next binding for `operation`: the binding of
/// `binding_seed`, the next binding and registration versions, and a lease
/// until `requested_valid_until`.
pub(super) fn next_binding_intent(
    current: &RegistrationIntentV1,
    operation: AttestationOperationV1,
    binding_seed: u8,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = current.clone();
    intent.operation = operation;
    intent.binding_id = B256::repeat_byte(binding_seed);
    intent.binding_version += 1;
    intent.registration_version += 1;
    intent.requested_valid_until = requested_valid_until;
    intent
}

pub(super) fn replacement_intent(
    current: &RegistrationIntentV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = next_binding_intent(
        current,
        AttestationOperationV1::ReplaceEnclaveBinding,
        seeds.binding,
        requested_valid_until,
    );
    intent.recipient_x25519 = [seeds.key; 32];
    intent.attestation_ed25519 = enclave_signer.verifying_key().to_bytes();
    intent.noise_responder_x25519 = [seeds.key.wrapping_add(1); 32];
    intent.enclave_id = intent.derived_enclave_id().unwrap();
    let manifest = initialization_manifest_for_intent(&intent, [0xa7; 32]);
    assert_eq!(
        manifest.node_host_authorization_hash().unwrap(),
        current.node_host_authorization_hash
    );
    manifest.validate_intent_binding(&intent).unwrap();
    intent
}

pub(super) fn measurement_transition_intent(
    current: &RegistrationIntentV1,
    next_policy: &TeePolicyV1,
    enclave_signer: &ed25519_dalek::SigningKey,
    seeds: EnclaveBindingSeeds,
    requested_valid_until: u64,
) -> RegistrationIntentV1 {
    let mut intent = replacement_intent(current, enclave_signer, seeds, requested_valid_until);
    intent.operation = AttestationOperationV1::TransitionEnclaveMeasurement;
    intent.transition_nonce += 1;
    intent.policy_hash = next_policy.policy_hash().unwrap();
    intent
}

/// Asserts that `result` is a deterministic revert whose message contains
/// `expected`.
pub(super) fn assert_reverts<T: std::fmt::Debug>(
    result: Result<T, PrecompileError>,
    expected: &str,
) {
    let message = revert_message(result.unwrap_err());
    assert!(message.contains(expected), "{message}");
}

/// Asserts that the first `mutate` call on `registry` creates the binding.
/// Then runs `between` on `registry` and asserts that an exact replay of the
/// same call is idempotent.
pub(super) fn assert_created_then_idempotent(
    registry: &mut TeeRegistry<'_>,
    mut mutate: impl FnMut(&mut TeeRegistry<'_>) -> Result<V1RegistrationOutcome, PrecompileError>,
    between: impl FnOnce(&mut TeeRegistry<'_>),
) {
    assert_eq!(mutate(registry).unwrap(), V1RegistrationOutcome::Created);
    between(registry);
    assert_eq!(mutate(registry).unwrap(), V1RegistrationOutcome::Idempotent);
}

/// Enables production storage-gas metering and sets the gas limit to
/// `u64::MAX`.
pub(crate) fn meter_production_gas(provider: &mut HashMapStorageProvider) {
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(u64::MAX);
}

/// Runs `dispatch` on a proposer, a validator and a follower, in that order.
/// Each replica starts from the chain that `new_chain` makes. Then it meters
/// production gas and runs `dispatch`. Returns each provider with its outcome,
/// in the same order. This function does not assert.
pub(super) fn run_on_three_replicas<T>(
    new_chain: impl Fn() -> HashMapStorageProvider,
    dispatch: impl Fn(StorageHandle<'_>) -> T,
) -> [(HashMapStorageProvider, T); 3] {
    let execute_replica = || {
        let mut provider = new_chain();
        meter_production_gas(&mut provider);
        let outcome = StorageHandle::enter(&mut provider, &dispatch);
        (provider, outcome)
    };
    let proposer = execute_replica();
    let validator = execute_replica();
    let follower = execute_replica();
    [proposer, validator, follower]
}

/// Asserts that `provider` metered at least one storage read and exactly
/// `writes` storage writes (`message` on failure). Returns the metered reads.
pub(crate) fn assert_metered_writes(
    provider: &HashMapStorageProvider,
    writes: u64,
    message: &str,
) -> u64 {
    let (reads, metered_writes) = provider.metered_storage_operations();
    assert!(reads > 0);
    assert_eq!(metered_writes, writes, "{message}");
    reads
}

/// The normative gas budget of one registry call.
pub(crate) struct NormativeBudget {
    /// The maximum transaction gas.
    pub(crate) maximum: u64,
    /// The maximum calldata intrinsic gas.
    pub(crate) intrinsic: u64,
    /// The storage-gas allowance inside the fixed charge.
    pub(crate) allowance: u64,
}

/// The [`NormativeBudget`] of one `kind` call with `calldata_len` bytes of
/// calldata and `evidence_len` bytes of evidence under `policy`.
pub(crate) fn normative_budget(
    kind: RegistryMutatorV1,
    calldata_len: usize,
    evidence_len: usize,
    policy: &TeePolicyV1,
) -> NormativeBudget {
    let schedule = TeeRegistryGasScheduleV1::normative();
    NormativeBudget {
        maximum: schedule
            .maximum_transaction_gas(
                kind,
                calldata_len,
                evidence_len,
                policy.measurement_rules.len(),
                policy.attestation_mode,
            )
            .unwrap(),
        intrinsic: schedule
            .maximum_calldata_intrinsic_gas(calldata_len)
            .unwrap(),
        allowance: schedule.mutator_storage_gas_allowance(kind),
    }
}

/// One metered registry call: its mutator kind and its ABI calldata.
pub(crate) struct MeteredCall<'a> {
    pub(crate) kind: RegistryMutatorV1,
    pub(crate) calldata: &'a [u8],
}

/// Asserts that the metered storage gas of `provider` stays inside the storage
/// allowance of `calls`. Also asserts that the charged gas equals the
/// normative maximum less the unused storage allowance.
pub(crate) fn assert_normative_gas(
    provider: &HashMapStorageProvider,
    calls: &[MeteredCall<'_>],
    evidence_len: usize,
    policy: &TeePolicyV1,
) {
    let mut maximum = 0_u64;
    let mut intrinsic = 0_u64;
    let mut allowance = 0_u64;
    for call in calls {
        let budget = normative_budget(call.kind, call.calldata.len(), evidence_len, policy);
        maximum += budget.maximum;
        intrinsic += budget.intrinsic;
        allowance += budget.allowance;
    }
    let overhead = 200 * calls.len() as u64;
    let (reads, writes) = provider.metered_storage_operations();
    let storage_gas = reads * 100 + writes * 5_000;
    assert!(storage_gas <= allowance);
    assert_eq!(
        intrinsic + overhead + provider.gas_used(),
        maximum - allowance + storage_gas
    );
    assert!(intrinsic + overhead + provider.gas_used() <= maximum);
}

/// Asserts that every replica has the storage, events, metered operations and
/// gas of `proposer`.
pub(super) fn assert_replicas_match(
    proposer: &HashMapStorageProvider,
    replicas: &[&HashMapStorageProvider],
) {
    for replica in replicas {
        assert_eq!(replica.storage, proposer.storage);
        assert_eq!(replica.get_ordered_events(), proposer.get_ordered_events());
        assert_eq!(
            replica.metered_storage_operations(),
            proposer.metered_storage_operations()
        );
        assert_eq!(replica.gas_used(), proposer.gas_used());
    }
}

/// The calldata of the `kind` mutator call with `evidence` and the signatures
/// of `signed`. Only the mutators without a node association have this form.
pub(super) fn evidence_mutator_calldata(
    kind: RegistryMutatorV1,
    evidence: &[u8],
    signed: &SignedIntent<'_>,
) -> Vec<u8> {
    let evidence = evidence.to_vec().into();
    let node_signature = signed.node_signature.to_vec().into();
    let enclave_signature = signed.enclave_signature.to_vec().into();
    match kind {
        RegistryMutatorV1::RenewEnclave => IRegisterEnclaveV1Test::renewEnclaveCall {
            evidence,
            nodeSignature: node_signature,
            enclaveSignature: enclave_signature,
        }
        .abi_encode(),
        RegistryMutatorV1::ReplaceEnclaveBinding => {
            IRegisterEnclaveV1Test::replaceEnclaveBindingCall {
                evidence,
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode()
        }
        RegistryMutatorV1::TransitionEnclaveMeasurement => {
            IRegisterEnclaveV1Test::transitionEnclaveMeasurementCall {
                evidence,
                nodeSignature: node_signature,
                enclaveSignature: enclave_signature,
            }
            .abi_encode()
        }
        RegistryMutatorV1::RegisterEnclave | RegistryMutatorV1::PrepareEnclaveUpgrade => {
            panic!("{kind:?} has no evidence-only test calldata")
        }
    }
}

/// The `registerEnclave` calldata for `signed` with `association` and
/// `evidence`.
pub(super) fn register_calldata(
    evidence: &[u8],
    signed: &SignedIntent<'_>,
    association: &NodeAssociation,
) -> Vec<u8> {
    IRegisterEnclaveV1Test::registerEnclaveCall {
        evidence: evidence.to_vec().into(),
        nodeSignature: signed.node_signature.to_vec().into(),
        enclaveSignature: signed.enclave_signature.to_vec().into(),
        validatorNodeBinding: association.binding.encode_canonical().unwrap().into(),
        validatorSignature: association.validator_signature.to_vec().into(),
        nodeBindingSignature: association.node_binding_signature.to_vec().into(),
    }
    .abi_encode()
}

/// DCAP evidence for `intent` with `quote`, the canonical collateral fixture of
/// `outbe_tee` and `transition_key_ready_proof`.
pub(super) fn synthetic_dcap_evidence(
    intent: &RegistrationIntentV1,
    quote: Vec<u8>,
    transition_key_ready_proof: Option<TransitionKeyReadyProofV1>,
) -> AttestationEvidenceV1 {
    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
        intent: intent.clone(),
        quote,
        components: outbe_tee::test_utils::canonical_dcap_collateral_fixture(),
        transition_key_ready_proof,
    })
}

pub(super) fn revert_message(error: PrecompileError) -> String {
    match error {
        PrecompileError::Revert(message) => message,
        other => panic!("expected deterministic revert, got {other:?}"),
    }
}
