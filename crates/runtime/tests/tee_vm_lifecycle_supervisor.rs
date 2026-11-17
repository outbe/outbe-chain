//! Supervisor observations use the production reducers and real BLS/MPT import.
// Existing shared fixtures use these crate names; reuse dependencies via consensus.
extern crate outbe_consensus as outbe_simplex;
extern crate self as alloy_rlp;
pub use alloy_consensus::private::alloy_rlp::{encode, Decodable};
extern crate self as zeroize;
use alloy_primitives::{Address, B256, U256};
pub use k256::elliptic_curve::zeroize::Zeroizing;
use outbe_consensus::finalized_admission_test_utils as finality;
use outbe_primitives::tee_attestation_v1::*;
use outbe_primitives::OutbeHeader;
use outbe_tee::{
    in_process::InProcessTeeContext,
    lifecycle::{enrollment::*, renewal::*, *},
};
use std::{path::PathBuf, sync::Arc};
#[path = "tee_vm_lifecycle_supervisor/authority.rs"]
mod authority;
#[path = "../src/admission_provider.rs"]
mod authority_provider;

#[path = "../../blockchain/tee/src/lifecycle/enrollment/tests/chain.rs"]
mod chain;
#[path = "../src/tee_supervisor/driver.rs"]
mod driver;
#[path = "../src/tee_supervisor/engine.rs"]
mod engine;
#[path = "tee_vm_lifecycle_supervisor/proof.rs"]
mod proof;
#[path = "tee_vm_lifecycle_supervisor/running.rs"]
mod running;
#[allow(dead_code)]
#[path = "../../blockchain/tee/tests/in_process_dkg_and_admission/fixtures.rs"]
mod shared;
use chain::{Chain, Signer};

struct Harness {
    dir: PathBuf,
    live: Arc<InProcessTeeContext>,
    journal: ServiceTxJournalV1,
    chain: Chain,
}
impl Harness {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let genesis = finality::FinalityCommitteeFixture::new(10);
        let descriptor = proof::descriptor(&genesis);
        let live = Arc::new(proof::context(11, &descriptor));
        let dir = std::env::temp_dir().join(format!("tee-vm-lifecycle-{}", B256::random()));
        std::fs::create_dir(&dir).unwrap();
        let host = dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
        std::fs::create_dir(&host).unwrap();
        let key = host.join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1);
        std::fs::write(&key, [4; 32]).unwrap();
        std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let journal = ServiceTxJournalV1::open(&dir).unwrap();
        let at = LifecycleCheckpointV1 {
            height: 10,
            hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
            timestamp: 1000,
            base_fee_per_gas: U256::from(1),
            chain_id: 676,
            genesis_hash: descriptor.network_binding.genesis_hash,
        };
        let mut policy =
            outbe_primitives::tee_test_utils::no_attestation_dev_policy_v1(676, at.genesis_hash)
                .unwrap();
        policy.maximum_lease = 7200;
        policy.minimum_lease = 3600;
        let chain = Chain {
            at,
            binding: None,
            policy,
            jailed: false,
            strict: false,
            upgrade_height: 0,
            balance: U256::MAX,
            nonces: [0; 3],
            sent: Default::default(),
            proof: None,
            public: B256::repeat_byte(9),
            fail_nonce: false,
            journal_dir: dir.clone(),
        };
        Self {
            dir,
            live,
            journal,
            chain,
        }
    }
    async fn step(&mut self) -> engine::Step {
        engine::step(
            &self.chain,
            &Signer,
            engine::ResidentSession {
                live: &self.live,
                journal: &mut self.journal,
                node_data_dir: &self.dir,
                phase: RenewalUpgradePhaseV1::Idle,
            },
        )
        .await
        .unwrap()
    }
    fn sent(&self) -> usize {
        self.chain.sent.lock().unwrap().len()
    }
    fn pending_intent(&self) -> RegistrationIntentV1 {
        use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
        use alloy_sol_types::SolCall;
        let record = self.journal.load().unwrap().unwrap();
        let tx = TxEip1559::rlp_decode_signed(&mut &record.raw_signed_tx[1..]).unwrap();
        use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
        let evidence = match record.kind {
            ServiceTxKindV1::Register => {
                ITeeRegistryV1::registerEnclaveCall::abi_decode(&tx.tx().input)
                    .unwrap()
                    .evidence
            }
            ServiceTxKindV1::Renew => {
                ITeeRegistryV1::renewEnclaveCall::abi_decode(&tx.tx().input)
                    .unwrap()
                    .evidence
            }
            _ => panic!("expected registration or renewal"),
        };
        AttestationEvidenceV1::decode_canonical(&evidence)
            .unwrap()
            .intent()
            .clone()
    }
    fn finalize(&mut self) {
        outbe_consensus::config::init_consensus_chain_id(676).unwrap();
        let genesis = finality::FinalityCommitteeFixture::new(10);
        let donor = proof::context(1, &proof::descriptor(&genesis));
        // Only the donor is seeded; the recipient must pass the production verifier.
        donor
            .activate_for_test(
                outbe_tee::resident::ResidentOfferMaterial::for_test(
                    vec![5; 96],
                    B256::from(self.live.manifest().chain_id),
                    0,
                    0,
                )
                .unwrap(),
            )
            .unwrap();
        let intent = self.pending_intent();
        let ctx = proof::onboarding_context(&intent, donor.network_encryption_public().unwrap());
        let artifact = donor
            .prepare_no_attestation_dev_onboarding_artifact_v2(ctx)
            .unwrap()
            .onboarding_artifact;
        let height = self.chain.at.height + 1;
        let timestamp = self.chain.at.timestamp + 1;
        let fixture = proof::finalized_proof(
            &genesis,
            &ctx,
            intent.requested_valid_until,
            (height, timestamp),
        );
        self.chain.public = ctx.network_encryption_public.into();
        let mut b = outbe_tee::test_utils::RenewalBindingFixtureV1::new(1).build();
        b.node_id_hash = intent.node_id.node_id_hash().unwrap();
        b.enclave_id = intent.enclave_id;
        b.binding_id = intent.binding_id;
        b.intent_hash = intent.intent_hash().unwrap();
        b.policy_hash = intent.policy_hash;
        b.binding_version = intent.binding_version;
        b.registration_version = intent.registration_version;
        b.renewal_nonce = intent.renewal_nonce;
        b.transition_nonce = intent.transition_nonce;
        b.valid_until = intent.requested_valid_until;
        b.recipient_x25519 = intent.recipient_x25519.into();
        b.attestation_ed25519 = intent.attestation_ed25519.into();
        b.noise_responder_x25519 = intent.noise_responder_x25519.into();
        b.node_host_authorization_hash = intent.node_host_authorization_hash;
        self.chain.binding = Some(b);
        self.chain.proof = Some(LifecycleOnboardingProofV1 {
            artifact: artifact.into(),
            proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1 {
                anchor_outcome: fixture.anchor.into(),
                committee_transitions: fixture.transitions.into_iter().map(Into::into).collect(),
                admission: fixture.proof.encode_canonical().unwrap().into(),
            },
        });
        self.chain.at.height = height + 2;
        self.chain.at.timestamp = timestamp + 2;
        self.chain.nonces = [self.chain.nonces[0] + 1; 3];
    }
    async fn admit(&mut self) {
        assert!(matches!(
            self.step().await,
            engine::Step::Enrollment(EnrollmentOutcome::Submitted { .. })
        ));
        self.finalize();
        assert!(matches!(
            self.step().await,
            engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
        ));
        assert!(self.live.offer_key_ready().unwrap());
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

impl LifecycleSignerV1 for Signer {
    type Error = std::convert::Infallible;
    fn address(&self) -> Address {
        Self::address()
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        EnrollmentSignerV1::sign_node_hash(self, hash)
    }
    fn sign_transaction_hash(
        &self,
        hash: B256,
    ) -> Result<alloy_primitives::Signature, Self::Error> {
        self.sign_operator_hash(hash)
    }
}
