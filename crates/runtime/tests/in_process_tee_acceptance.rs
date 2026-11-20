//! Resident admission at the supervisor's finalized chain-port boundary.
//!
//! The chain view is a fixture; the scheduler, BLS and MPT verification,
//! recipient binding, and resident activation are production code.
extern crate outbe_consensus as outbe_simplex;
extern crate self as alloy_rlp;
extern crate self as zeroize;

pub use alloy_consensus::private::alloy_rlp::{encode, Decodable};
pub use k256::elliptic_curve::zeroize::Zeroizing;

use alloy_primitives::{Address, Signature, B256, U256};
use outbe_consensus::finalized_admission_test_utils as finality;
use outbe_primitives::{
    tee_attestation_v1::{AttestationMode, AttestationOperationV1, TeePolicyV1},
    tee_test_utils::no_attestation_dev_policy_v1,
};
use outbe_tee::{
    in_process::{admission::AdmissionError, InProcessError, InProcessTeeContext, ProofSourceV1},
    lifecycle::{enrollment::*, renewal::*, *},
    node_host::{NodeHostIdentityKey, NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1},
    resident::ResidentTeeKeys,
    test_utils::RenewalBindingFixtureV1,
    upgrade_transfer::UpgradeKeyProofV1,
    NodeHostIdentityV1,
};

#[path = "../../blockchain/tee/src/lifecycle/enrollment/tests/chain.rs"]
mod chain;
#[allow(dead_code)]
// The supervisor also uses this module for paths not exercised by admission.
#[path = "../src/tee_supervisor/engine.rs"]
mod engine;
#[allow(dead_code)]
#[path = "../../blockchain/tee/tests/in_process_dkg_and_admission/fixtures.rs"]
mod shared;

use chain::{Chain, Signer};

impl LifecycleSignerV1 for Signer {
    type Error = std::convert::Infallible;

    fn address(&self) -> Address {
        Self::address()
    }

    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        EnrollmentSignerV1::sign_node_hash(self, hash)
    }

    fn sign_transaction_hash(&self, hash: B256) -> Result<Signature, Self::Error> {
        self.sign_operator_hash(hash)
    }
}

#[derive(Clone, Copy, Debug)]
enum Denial {
    AccountMpt,
    StorageMpt,
    BlsFinality,
    WrongRecipient,
}

#[tokio::test]
async fn supervisor_rejects_invalid_proofs_and_foreign_recipient_before_key_install() {
    const CHAIN_ID: u64 = 676;
    outbe_consensus::config::init_consensus_chain_id(CHAIN_ID).unwrap();
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = shared::AdmissionFixture::descriptor(&genesis);
    let founders = (1..=4)
        .map(|seed| shared::context(seed, &descriptor))
        .collect::<Vec<_>>();
    let expected_public = shared::run_dkg(&founders);

    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::create_dir(&host).unwrap();
    let host_identity =
        NodeHostIdentityKey::create_new(&host.join(NODE_HOST_IDENTITY_KEY_V1)).unwrap();
    let template = shared::context(5, &descriptor);
    let mut manifest = template.manifest().clone();
    manifest.node_host_noise_x25519 = host_identity.public();
    let live = InProcessTeeContext::new(
        ResidentTeeKeys::from_seed([5; 32]).unwrap(),
        *template.identity(),
        manifest,
        descriptor.clone(),
        ProofSourceV1::NoHardware,
    )
    .unwrap();

    let policy =
        no_attestation_dev_policy_v1(CHAIN_ID, descriptor.network_binding.genesis_hash).unwrap();
    let mut intent = shared::intent(&live, AttestationOperationV1::RegisterEnclave);
    intent.policy_hash = policy.policy_hash().unwrap();
    let onboarding = shared::onboarding_context(&intent, expected_public);
    let artifact = founders[0]
        .prepare_no_attestation_dev_onboarding_artifact_v2(onboarding)
        .unwrap();
    let fixture = shared::AdmissionFixture::new(&genesis, &onboarding, false);
    let header =
        alloy_consensus::Header::decode(&mut fixture.proof.admission.header.as_slice()).unwrap();

    let mut binding = RenewalBindingFixtureV1::new(1).build();
    binding.node_id_hash = onboarding.node_id_hash;
    binding.enclave_id = intent.enclave_id;
    binding.binding_id = intent.binding_id;
    binding.intent_hash = onboarding.intent_hash;
    binding.policy_hash = intent.policy_hash;
    binding.binding_version = intent.binding_version;
    binding.registration_version = intent.registration_version;
    binding.renewal_nonce = intent.renewal_nonce;
    binding.transition_nonce = intent.transition_nonce;
    binding.valid_until = intent.requested_valid_until;
    binding.recipient_x25519 = intent.recipient_x25519.into();
    binding.attestation_ed25519 = intent.attestation_ed25519.into();
    binding.noise_responder_x25519 = intent.noise_responder_x25519.into();
    binding.node_host_authorization_hash = intent.node_host_authorization_hash;
    let mut chain = Chain {
        at: LifecycleCheckpointV1 {
            height: header.number,
            hash: header.hash_slow(),
            state_root: header.state_root,
            timestamp: header.timestamp,
            base_fee_per_gas: U256::from(1),
            chain_id: CHAIN_ID,
            genesis_hash: descriptor.network_binding.genesis_hash,
        },
        binding: Some(binding),
        policy,
        jailed: false,
        strict: false,
        upgrade_height: 0,
        balance: U256::MAX,
        nonces: [0; 3],
        sent: Default::default(),
        proof: None,
        public: B256::from(expected_public),
        fail_nonce: false,
        journal_dir: dir.path().to_owned(),
    };
    let mut journal = ServiceTxJournalV1::open(dir.path()).unwrap();

    let valid = LifecycleOnboardingProofV1 {
        artifact: artifact.onboarding_artifact.into(),
        proof: UpgradeKeyProofV1 {
            anchor_outcome: fixture.anchor.into(),
            committee_transitions: fixture.transitions.into_iter().map(Into::into).collect(),
            admission: fixture.proof.encode_canonical().unwrap().into(),
        },
    };
    for denial in [
        Denial::AccountMpt,
        Denial::StorageMpt,
        Denial::BlsFinality,
        Denial::WrongRecipient,
    ] {
        let mut input = valid.clone();
        let mut witness = fixture.proof.clone();
        match denial {
            // Keep the trie nodes and certified header intact; the openings
            // must fail against an otherwise valid finalized state root.
            Denial::AccountMpt => witness.registry_account.balance += U256::from(1),
            Denial::StorageMpt => {
                // A different but still live expiry passes claim checks and
                // must fail the storage opening against the signed root.
                let expiry =
                    outbe_tee::finalized_admission::onboarding_registry_slots_v1(&onboarding)[7];
                witness
                    .registry_storage
                    .iter_mut()
                    .find(|opening| opening.key == expiry)
                    .unwrap()
                    .value += U256::from(1);
            }
            Denial::BlsFinality => {
                witness.admission.finalization =
                    shared::wrong_committee_certificate(&fixture.proof);
            }
            Denial::WrongRecipient => {
                let foreign = shared::context(6, &descriptor);
                let for_foreign = outbe_tee::dcap_protocol::DcapOnboardingContextV1 {
                    recipient_x25519: foreign.recipient_public(),
                    ..onboarding
                };
                input.artifact = founders[0]
                    .prepare_no_attestation_dev_onboarding_artifact_v2(for_foreign)
                    .unwrap()
                    .onboarding_artifact
                    .into();
            }
        }
        input.proof.admission = witness.encode_canonical().unwrap().into();
        chain.proof = Some(input);
        let error = engine::step(
            &chain,
            &Signer,
            engine::ResidentSession {
                live: &live,
                journal: &mut journal,
                node_data_dir: dir.path(),
                phase: RenewalUpgradePhaseV1::Idle,
            },
        )
        .await
        .unwrap_err();

        let engine::SupervisorError::Enrollment(EnrollmentError::Resident(error)) = error else {
            panic!("{denial:?} did not reach resident validation: {error:?}");
        };
        assert!(
            matches!(
                (denial, &error),
                (
                    Denial::AccountMpt,
                    InProcessError::Admission(AdmissionError::RegistryAccountProof(_))
                ) | (
                    Denial::StorageMpt,
                    InProcessError::Admission(AdmissionError::RegistryStorageProof(_))
                ) | (
                    Denial::BlsFinality,
                    InProcessError::Admission(AdmissionError::AdmissionFinalization(_))
                ) | (Denial::WrongRecipient, InProcessError::IdentityMismatch)
            ),
            "{denial:?}: {error:?}"
        );
        assert!(
            !live.offer_key_ready().unwrap(),
            "{denial:?} installed a key"
        );
        assert!(
            live.admission_anchor().unwrap().is_none(),
            "{denial:?} installed an anchor"
        );
    }

    // The unmodified fixture must work through the same scheduler and admit
    // the public key independently established by the founding DKG.
    chain.proof = Some(valid);
    let result = engine::step(
        &chain,
        &Signer,
        engine::ResidentSession {
            live: &live,
            journal: &mut journal,
            node_data_dir: dir.path(),
            phase: RenewalUpgradePhaseV1::Idle,
        },
    )
    .await
    .unwrap();
    let engine::Step::Enrollment(EnrollmentOutcome::Admitted { anchor }) = result else {
        panic!("valid fixture was not admitted: {result:?}");
    };
    assert_eq!(anchor.finalized_hash, chain.at.hash);
    assert_eq!(live.network_encryption_public().unwrap(), expected_public);
}

#[test]
fn dcap_required_without_source_refuses_resident_context() {
    let genesis = finality::FinalityCommitteeFixture::new(10);
    let descriptor = shared::AdmissionFixture::descriptor(&genesis);
    let template = shared::context(7, &descriptor);
    let initialize = |mode| {
        let mut manifest = template.manifest().clone();
        manifest.attestation_mode = mode;
        let mut descriptor = descriptor.clone();
        descriptor.network_binding = manifest.network_binding();
        let identity = NodeHostIdentityV1 {
            network_binding: descriptor.network_binding,
            reth_p2p_public: manifest.node_id.reth_p2p_public,
        };
        InProcessTeeContext::new(
            ResidentTeeKeys::from_seed([7; 32]).unwrap(),
            identity,
            manifest,
            descriptor,
            ProofSourceV1::NoHardware,
        )
    };

    let dev = initialize(AttestationMode::NoAttestationDev).unwrap();
    assert!(!dev.offer_key_ready().unwrap());
    assert!(matches!(
        initialize(AttestationMode::DcapRequired),
        Err(InProcessError::HardwareProofUnavailable)
    ));
}
