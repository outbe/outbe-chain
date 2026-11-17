use super::fixtures::assert_revert_bytes;
use super::*;
use crate::v1::NodeEnclaveBindingV1;
use alloy_sol_types::SolError;
use outbe_primitives::{
    tee_attestation_v1::TrustedNetworkDescriptorV1, tee_registry_abi_v1::ITeeRegistryV1,
};
use outbe_tee::{
    in_process::{HardwareProofSourceV1, InProcessTeeContext, ProofSourceV1},
    load_replacement_candidate_submission, persist_replacement_candidate_submission,
    resident::ResidentTeeKeys,
    NodeHostIdentityV1, ReplacementCandidateSubmissionV1,
};
use std::{path::Path, sync::Arc};

// Explicit test-only quote capability; registry verification still uses its existing verdict fixture.
struct SyntheticProofSource;
impl HardwareProofSourceV1 for SyntheticProofSource {
    fn code_identity(&self) -> outbe_tee::in_process::Result<(B256, B256, u16, u16)> {
        Ok((B256::ZERO, B256::ZERO, 0, 0))
    }
    fn quote(&self, report_data: [u8; 64]) -> outbe_tee::in_process::Result<Vec<u8>> {
        let mut quote = vec![0; outbe_tee::quote::MIN_QUOTE_LEN];
        let offset = quote.len() - report_data.len();
        quote[offset..].copy_from_slice(&report_data);
        Ok(quote)
    }
}

fn resident_enclave(
    seed: u8,
    identity: NodeHostIdentityV1,
    node_host_noise_x25519: [u8; 32],
) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let binding = identity.network_binding;
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: binding.chain_id,
        genesis_hash: binding.genesis_hash,
        attestation_mode: binding.attestation_mode,
        node_id: NodeIdV1 {
            reth_p2p_public: identity.reth_p2p_public,
        },
        initialization_challenge: [seed; 32],
        node_host_noise_x25519,
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        TrustedNetworkDescriptorV1 {
            network_binding: binding,
            genesis_consensus_keys: Vec::new(),
        },
        ProofSourceV1::Hardware(Arc::new(SyntheticProofSource)),
    )
    .unwrap()
}

/// Seed only public NodeHost records; resident identities never persist their TEE keys.
fn stage_public_replacement_records(
    root: &Path,
    active: &EnclaveInitializationManifestV1,
    candidate: &EnclaveInitializationManifestV1,
) {
    use outbe_tee::node_host::{NODE_HOST_MANIFEST_V1, NODE_HOST_REPLACEMENT_CANDIDATE_V1};
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let manifest = candidate.encode_canonical().unwrap();
    // The frozen candidate record is version || predecessor hash || u16 length || manifest.
    let mut record = vec![1];
    record.extend_from_slice(active.authorization_hash().unwrap().as_slice());
    record.extend_from_slice(&u16::try_from(manifest.len()).unwrap().to_be_bytes());
    record.extend_from_slice(&manifest);
    for (name, bytes) in [
        (NODE_HOST_MANIFEST_V1, active.encode_canonical().unwrap()),
        (NODE_HOST_REPLACEMENT_CANDIDATE_V1, record),
    ] {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(name))
            .unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
    }
}

/// `initial` changed to replace its binding with the candidate enclave of
/// `candidate_manifest`.
fn replacement_intent_for_candidate(
    initial: &RegistrationIntentV1,
    candidate_manifest: &EnclaveInitializationManifestV1,
) -> RegistrationIntentV1 {
    let mut replacement = next_binding_intent(
        initial,
        AttestationOperationV1::ReplaceEnclaveBinding,
        0x68,
        NOW + 3_600,
    );
    replacement.enclave_id = candidate_manifest.enclave_id().unwrap();
    replacement.recipient_x25519 = candidate_manifest.recipient_x25519;
    replacement.attestation_ed25519 = candidate_manifest.attestation_ed25519;
    replacement.noise_responder_x25519 = candidate_manifest.noise_responder_x25519;
    replacement.node_host_authorization_hash =
        candidate_manifest.node_host_authorization_hash().unwrap();
    replacement
}

/// Persists the candidate submission, asserts that it reloads exactly and
/// returns it with its decoded DCAP evidence.
fn persist_exact_submission(
    node_data_dir: &Path,
    evidence: &AttestationEvidenceV1,
    node_signature: &[u8; 65],
    enclave_signature: &[u8; 64],
) -> (ReplacementCandidateSubmissionV1, DcapEvidenceV1) {
    let exact_evidence = evidence.encode_canonical().unwrap();
    let submission = persist_replacement_candidate_submission(
        node_data_dir,
        evidence,
        node_signature,
        enclave_signature,
    )
    .unwrap();
    assert_eq!(submission.evidence(), exact_evidence);
    assert_eq!(
        load_replacement_candidate_submission(node_data_dir)
            .unwrap()
            .unwrap(),
        submission
    );
    let AttestationEvidenceV1::Dcap(submitted) =
        AttestationEvidenceV1::decode_canonical(submission.evidence()).unwrap()
    else {
        unreachable!();
    };
    (submission, submitted)
}

/// Registers `node_signer` as a validator with `CONSENSUS_KEY`, installs
/// `policy`, registers `initial` and then replaces it with `replacement`.
/// Returns the validator binding that the registry stores.
fn register_then_replace(
    policy: &TeePolicyV1,
    node_signer: &OutbeEvmSigner,
    initial: &SignedIntent<'_>,
    replacement: &SignedIntent<'_>,
) -> NodeEnclaveBindingV1 {
    let accepted = up_to_date_verdict_until(NOW + 12_000);
    let mut binding = None;
    run_as_validator_with_binding_of(
        policy,
        node_signer,
        initial.with_verdict(accepted.clone()),
        |_storage, mut registry| {
            assert_eq!(
                registry
                    .replace_enclave_binding_after_verifier_for_test(
                        replacement.with_verdict(accepted)
                    )
                    .unwrap(),
                V1RegistrationOutcome::Created
            );
            binding = Some(validator_binding(&registry, node_signer.address()));
        },
    );
    binding.expect("the registration test ran")
}

#[test]
fn candidate_generated_quote_intent_reaches_registry_replacement_exactly() {
    let active_policy = hardening_policy(B256::repeat_byte(0x23));
    let node_signer = OutbeEvmSigner::from_secret_bytes([0x75; 32]).unwrap();
    let identity = NodeHostIdentityV1 {
        network_binding: active_policy.network_binding(),
        reth_p2p_public: reth_p2p_public_for_evm_signer(&node_signer),
    };
    let root = tempfile::tempdir().unwrap();
    let node_data_dir = root.path().join("node-data");
    std::fs::create_dir(&node_data_dir).unwrap();
    use std::os::unix::fs::DirBuilderExt as _;
    let host_dir = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host_dir)
        .unwrap();
    let node_host = outbe_tee::node_host::NodeHostIdentityKey::create_new(
        &host_dir.join(outbe_tee::node_host::NODE_HOST_IDENTITY_KEY_V1),
    )
    .unwrap();
    let active = resident_enclave(0x76, identity, node_host.public());
    let candidate = resident_enclave(0x77, identity, node_host.public());
    stage_public_replacement_records(&host_dir, active.manifest(), candidate.manifest());
    let active_manifest = active.manifest();
    let initial = initial_intent_for_manifest(&active_policy, active_manifest, 0x66);
    active_manifest.validate_intent_binding(&initial).unwrap();
    let initial_hash = initial.intent_hash().unwrap();
    let signed_initial = SignedIntent {
        intent: &initial,
        node_signature: node_signer.sign_hash(&initial_hash).unwrap(),
        enclave_signature: active.sign_domain_attestation(initial_hash.as_slice()),
    };

    let candidate_manifest = candidate.manifest().clone();
    let replacement = replacement_intent_for_candidate(&initial, &candidate_manifest);
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    assert_eq!(
        replacement.node_host_authorization_hash,
        initial.node_host_authorization_hash
    );

    let generated = candidate.generate_dcap_quote(&replacement).unwrap();
    let replacement_hash = replacement.intent_hash().unwrap();
    let replacement_node_signature = node_signer.sign_hash(&replacement_hash).unwrap();
    let evidence = synthetic_dcap_evidence(&replacement, generated.quote_body.clone(), None);
    let (submission, submitted) = persist_exact_submission(
        &node_data_dir,
        &evidence,
        &replacement_node_signature,
        &generated.enclave_signature,
    );
    assert_eq!(
        submitted.intent.encode_canonical().unwrap(),
        replacement.encode_canonical().unwrap()
    );
    assert_eq!(submitted.quote, generated.quote_body);

    let signed_submission = SignedIntent {
        intent: &submitted.intent,
        node_signature: *submission.node_signature(),
        enclave_signature: *submission.enclave_signature(),
    };
    let binding = register_then_replace(
        &active_policy,
        &node_signer,
        &signed_initial,
        &signed_submission,
    );
    assert_eq!(binding.enclave_id, candidate_manifest.enclave_id().unwrap());
    assert_eq!(binding.binding_id, replacement.binding_id);
    assert_eq!(binding.binding_version, replacement.binding_version);
    assert_eq!(
        binding.registration_version,
        replacement.registration_version
    );
}

#[test]
fn replacement_candidate_intent_reaches_registry_unchanged_and_never_reuses_consumed_ids() {
    let validator = LifecycleValidator::new(
        hardening_policy(B256::repeat_byte(0x23)),
        0x75,
        0x76,
        EnclaveBindingSeeds::new(0x65, 0x66),
    );
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let replacement = replacement_intent(
        &validator.initial,
        &new_enclave,
        EnclaveBindingSeeds::new(0x67, 0x68),
        NOW + 3_600,
    );
    let active_manifest = initialization_manifest_for_intent(&validator.initial, [0xa6; 32]);
    let candidate_manifest = initialization_manifest_for_intent(&replacement, [0xa7; 32]);
    assert_ne!(
        active_manifest.authorization_hash().unwrap(),
        candidate_manifest.authorization_hash().unwrap()
    );
    assert_eq!(
        active_manifest.node_host_authorization_hash().unwrap(),
        candidate_manifest.node_host_authorization_hash().unwrap()
    );
    candidate_manifest
        .validate_intent_binding(&replacement)
        .unwrap();
    let candidate_intent_hash = replacement.intent_hash().unwrap();
    let signed_initial = validator.signed_initial();
    let signed_replacement = validator.sign_with_enclave(&replacement, &new_enclave);
    let accepted = up_to_date_verdict_until(NOW + 12_000);
    validator.run_as_validator_with_binding(
        signed_initial.with_verdict(accepted.clone()),
        |storage, mut registry| {
            assert_created_then_idempotent(
                &mut registry,
                |registry| {
                    registry.replace_enclave_binding_after_verifier_for_test(
                        signed_replacement.with_verdict(accepted.clone()),
                    )
                },
                |_| {},
            );
            let binding = validator.stored_binding(&registry);
            assert_eq!(binding.enclave_id, replacement.enclave_id);
            assert_eq!(binding.binding_id, replacement.binding_id);
            assert_eq!(binding.binding_version, 2);
            assert_eq!(binding.registration_version, 1);
            assert_eq!(binding.intent_hash, candidate_intent_hash);

            let old_renewal = renewal_intent(&validator.initial, NOW + 6_000);
            let signed_old_renewal = validator.sign(&old_renewal);
            storage
                .set_block_timestamp(U256::from(NOW + 2_400))
                .unwrap();
            assert_revert_bytes(
                registry.renew_enclave_after_verifier_for_test(
                    signed_old_renewal.with_verdict(accepted.clone()),
                ),
                ITeeRegistryV1::RenewalTargetsDifferentIdentity {}
                    .abi_encode()
                    .as_slice(),
            );

            let current = replacement.clone();
            let attempted_reuse = replacement_intent(
                &current,
                &validator.enclave_signer,
                EnclaveBindingSeeds::new(0x65, 0x66),
                NOW + 6_000,
            );
            let signed_attempted_reuse = validator.sign(&attempted_reuse);
            assert_revert_bytes(
                registry.replace_enclave_binding_after_verifier_for_test(
                    signed_attempted_reuse.with_verdict(accepted),
                ),
                ITeeRegistryV1::SuccessorIdsAlreadyUsed {}
                    .abi_encode()
                    .as_slice(),
            );
        },
    );
}

/// The renew and replace ABI calls that follow one initial registration.
struct RenewAndReplaceCalls<'a> {
    renewal: &'a RegistrationIntentV1,
    renewal_call: Vec<u8>,
    replacement: &'a RegistrationIntentV1,
    replacement_call: Vec<u8>,
}

impl<'a> RenewAndReplaceCalls<'a> {
    /// Encodes the calls for `renewal` and `replacement` with `evidence`.
    fn new(evidence: &[u8], renewal: &SignedIntent<'a>, replacement: &SignedIntent<'a>) -> Self {
        Self {
            renewal: renewal.intent,
            renewal_call: evidence_mutator_calldata(
                RegistryMutatorV1::RenewEnclave,
                evidence,
                renewal,
            ),
            replacement: replacement.intent,
            replacement_call: evidence_mutator_calldata(
                RegistryMutatorV1::ReplaceEnclaveBinding,
                evidence,
                replacement,
            ),
        }
    }

    /// The mutator kind and calldata of each call, in dispatch order.
    fn metered(&self) -> [MeteredCall<'_>; 2] {
        [
            MeteredCall {
                kind: RegistryMutatorV1::RenewEnclave,
                calldata: &self.renewal_call,
            },
            MeteredCall {
                kind: RegistryMutatorV1::ReplaceEnclaveBinding,
                calldata: &self.replacement_call,
            },
        ]
    }

    /// Runs a proposer, a validator and a follower with `run_on_three_replicas`.
    /// Each replica registers `validator` with `CONSENSUS_KEY`, installs its
    /// policy, registers `initial` and sets the block timestamp to
    /// `NOW + 2_400`. Then it dispatches both calls with production gas
    /// metering. Returns the providers in that order.
    fn execute_on_three_replicas(
        &self,
        validator: &LifecycleValidator,
        initial: &SignedIntent<'_>,
    ) -> [HashMapStorageProvider; 3] {
        let accepted = up_to_date_verdict_until(NOW + 12_000);
        let new_chain = || {
            validator.validator_provider_with_binding_at(
                NOW + 2_400,
                initial.with_verdict(accepted.clone()),
            )
        };
        let [(proposer, ()), (replica_validator, ()), (follower, ())] =
            run_on_three_replicas(new_chain, |storage| {
                dispatch_renew_after_verifier_for_test(
                    storage.clone(),
                    validator.node_signer.address(),
                    &self.renewal_call,
                    self.renewal,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap();
                dispatch_replace_after_verifier_for_test(
                    storage,
                    validator.node_signer.address(),
                    &self.replacement_call,
                    self.replacement,
                    PostVerifierDcapCapabilityV1::new(accepted.clone()),
                )
                .unwrap();
            });
        [proposer, replica_validator, follower]
    }
}

#[test]
fn renew_and_replace_abi_are_replica_deterministic_and_fit_normative_gas() {
    let node = LifecycleValidator::new(
        capped_lease_policy(B256::repeat_byte(0x24)),
        0x78,
        0x79,
        EnclaveBindingSeeds::new(0x69, 0x6A),
    );
    let new_enclave = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let (renewal, replacement) = node.renewal_then_replacement(
        &new_enclave,
        EnclaveBindingSeeds::new(0x6B, 0x6C),
        NOW + 6_000,
    );
    let signed_initial = node.signed_initial();
    let signed_renewal = node.sign(&renewal);
    let signed_replacement = node.sign_with_enclave(&replacement, &new_enclave);
    let evidence = [0xA7; 4_096];
    let calls = RenewAndReplaceCalls::new(&evidence, &signed_renewal, &signed_replacement);

    let [proposer, validator, follower] = calls.execute_on_three_replicas(&node, &signed_initial);
    assert_replicas_match(&proposer, &[&validator, &follower]);

    for call in calls.metered() {
        let budget = normative_budget(call.kind, call.calldata.len(), evidence.len(), &node.policy);
        assert!(budget.maximum < 30_000_000);
    }
    assert_normative_gas(&proposer, &calls.metered(), evidence.len(), &node.policy);
}
