use super::finality::{CertifiedBlockFixture, FinalityCommitteeFixture};
use alloy_primitives::{B256, U256};
use commonware_codec::{Encode as _, ReadExt as _};
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381;
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, tee_attestation_v1::*};
use outbe_tee::{
    dcap_protocol::DcapOnboardingContextV1,
    finalized_admission::*,
    in_process::{admission::FinalizedOnboardingV1, InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};
use std::collections::BTreeMap;

pub fn node_id() -> NodeIdV1 {
    let signing = k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap();
    NodeIdV1 {
        reth_p2p_public: signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap(),
    }
}
pub fn context(seed: u8, descriptor: &TrustedNetworkDescriptorV1) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: descriptor.network_binding.chain_id,
        genesis_hash: descriptor.network_binding.genesis_hash,
        attestation_mode: descriptor.network_binding.attestation_mode,
        node_id: node_id(),
        initialization_challenge: [seed; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let identity = NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        descriptor.clone(),
        ProofSourceV1::NoHardware,
    )
    .unwrap()
}
pub fn intent(
    context: &InProcessTeeContext,
    operation: AttestationOperationV1,
) -> RegistrationIntentV1 {
    let manifest = context.manifest();
    RegistrationIntentV1 {
        chain_id: manifest.chain_id,
        genesis_hash: manifest.genesis_hash,
        operation,
        attestation_mode: manifest.attestation_mode,
        policy_hash: B256::repeat_byte(0x41),
        node_id: manifest.node_id.clone(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: if operation == AttestationOperationV1::TransitionEnclaveMeasurement {
            1
        } else {
            0
        },
        requested_valid_until: 2000,
        recipient_x25519: manifest.recipient_x25519,
        attestation_ed25519: manifest.attestation_ed25519,
        noise_responder_x25519: manifest.noise_responder_x25519,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    }
}
pub fn onboarding_context(
    intent: &RegistrationIntentV1,
    public: [u8; 32],
) -> DcapOnboardingContextV1 {
    DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: intent.intent_hash().unwrap(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: public,
        key_epoch: 0,
        network_encryption_epoch: 0,
    }
}
pub fn run_dkg(contexts: &[InProcessTeeContext]) -> [u8; 32] {
    let id = [42; 32];
    let participants = contexts
        .iter()
        .map(|c| c.tee_bls_public_bytes())
        .collect::<Vec<_>>();
    let recipient_keys = contexts
        .iter()
        .map(|c| {
            let key = bls12381::PublicKey::read(&mut c.tee_bls_public_bytes().as_slice()).unwrap();
            (key, c.dkg_enc_public())
        })
        .collect::<BTreeMap<_, _>>();
    let mut missing = recipient_keys.clone();
    missing.pop_first();
    assert!(matches!(
        contexts[0].open_dkg_session(id, 0, &participants, missing),
        Err(outbe_tee::in_process::InProcessError::IdentityMismatch)
    ));
    let mut wrong_local = recipient_keys.clone();
    let local =
        bls12381::PublicKey::read(&mut contexts[0].tee_bls_public_bytes().as_slice()).unwrap();
    wrong_local.insert(local, contexts[1].dkg_enc_public());
    assert!(matches!(
        contexts[0].open_dkg_session(id, 0, &participants, wrong_local),
        Err(outbe_tee::in_process::InProcessError::IdentityMismatch)
    ));
    for context in contexts {
        context
            .open_dkg_session(id, 0, &participants, recipient_keys.clone())
            .unwrap();
    }
    let mut logs = Vec::new();
    for dealer in contexts {
        let (pub_msg, sealed) = dealer
            .with_dkg_session(&id, |s| s.start_dealer_encoded())
            .unwrap();
        for player in contexts {
            let pk = player.tee_bls_public_bytes();
            let blob = &sealed
                .iter()
                .find(|(recipient, _)| recipient == &pk)
                .unwrap()
                .1;
            let ack = player
                .with_dkg_session(&id, |s| {
                    s.player_ingest_encoded(&dealer.tee_bls_public_bytes(), &pub_msg, blob)
                })
                .unwrap()
                .unwrap();
            dealer
                .with_dkg_session(&id, |s| s.dealer_receive_ack_encoded(&pk, &ack))
                .unwrap();
        }
        logs.push(
            dealer
                .with_dkg_session(&id, |s| s.dealer_finalize_encoded())
                .unwrap(),
        );
    }
    assert!(matches!(
        contexts[0].with_dkg_session(&id, |s| s.player_finalize_encoded(&logs[..3])),
        Err(outbe_tee::in_process::InProcessError::Dkg(
            outbe_tee::in_process::dkg::DkgError::IncompleteDealerSet {
                actual: 3,
                expected: 4
            }
        ))
    ));
    let mut output = None;
    for context in contexts {
        let (public, _) = context
            .with_dkg_session(&id, |s| s.player_finalize_encoded(&logs))
            .unwrap();
        if let Some(previous) = output.as_ref() {
            assert_eq!(&public, previous);
        }
        output = Some(public);
    }
    let partials = contexts
        .iter()
        .map(|c| {
            c.with_dkg_session(&id, |s| s.tribute_offer_partials_sealed())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut offer_public = None;
    for context in contexts {
        let pk = context.tee_bls_public_bytes();
        let sealed = partials
            .iter()
            .map(|partials| {
                partials
                    .iter()
                    .find(|(recipient, _)| recipient.encode().as_ref() == pk.as_slice())
                    .unwrap()
                    .1
                    .to_wire_bytes()
            })
            .collect::<Vec<_>>();
        verify_bad_partial_rejected(context, &id, &sealed, &recipient_keys);
        let public = context.complete_founding_dkg(&id, &sealed, 0).unwrap();
        if let Some(previous) = offer_public {
            assert_eq!(public, previous);
        }
        offer_public = Some(public);
    }
    offer_public.unwrap()
}
fn compact(block: &CertifiedBlockFixture) -> CertifiedHeaderV1 {
    let decoded = outbe_simplex::committee_chain::decode_public_finalized_block(
        &block.finalization,
        &block.block,
        outbe_simplex::bls::MAX_COMMITTEE_MEMBERS,
    )
    .unwrap();
    CertifiedHeaderV1 {
        finalization: block.finalization.clone(),
        header: alloy_rlp::encode(decoded.block.header()),
        descendants: Vec::new(),
    }
}
pub struct AdmissionFixture {
    pub anchor: Vec<u8>,
    pub transitions: Vec<Vec<u8>>,
    pub proof: FinalizedAdmissionWitnessV1,
}
impl AdmissionFixture {
    pub fn descriptor(genesis: &FinalityCommitteeFixture) -> TrustedNetworkDescriptorV1 {
        TrustedNetworkDescriptorV1 {
            network_binding: NetworkBindingV1 {
                chain_id: U256::from(676_u64).to_be_bytes(),
                genesis_hash: B256::repeat_byte(0x31),
                attestation_mode: AttestationMode::NoAttestationDev,
            },
            genesis_consensus_keys: genesis.public_keys_min_pk(),
        }
    }
    pub fn new(
        genesis: &FinalityCommitteeFixture,
        context: &DcapOnboardingContextV1,
        upgrade: bool,
    ) -> Self {
        use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
        let successor = FinalityCommitteeFixture::new(50);
        let slots = if upgrade {
            upgrade_registry_slots_v1(context)
        } else {
            onboarding_registry_slots_v1(context)
        };
        let mut values = vec![
            U256::from_be_bytes(context.network_encryption_public),
            U256::from(context.key_epoch),
            U256::from(context.network_encryption_epoch),
        ];
        values.extend(if upgrade {
            [
                U256::from_be_bytes(context.context_hash().0),
                U256::from(2000),
                U256::from(123),
                U256::from(123),
                U256::from_be_bytes(context.policy_hash.0),
                U256::from(1),
            ]
        } else {
            [
                U256::from_be_bytes(context.enclave_id.0),
                U256::from_be_bytes(context.binding_id.0),
                U256::from_be_bytes(context.intent_hash.0),
                U256::from_be_bytes(context.policy_hash.0),
                U256::from(2000),
                U256::from_be_bytes(context.recipient_x25519),
            ]
        });
        let leaves = slots
            .iter()
            .zip(values.iter().copied())
            .map(|(slot, value)| (U256::from_be_bytes(slot.0), value))
            .collect::<Vec<_>>();
        let (storage_root, storage_proofs) = storage_trie(&leaves);
        let account = reth_trie::TrieAccount {
            nonce: 1,
            balance: U256::from(2),
            storage_root,
            code_hash: B256::repeat_byte(0x91),
        };
        let (state_root, accounts) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
        let transition = genesis.certify_block(
            Epoch::new(0),
            1,
            900,
            B256::repeat_byte(0x81),
            successor.preannounce_extra_data(Epoch::new(1)),
        );
        let admission = successor.certify_block(Epoch::new(1), 2, 1000, state_root, Vec::new());
        // Exercise real header ancestry, not only direct finalization.
        let descendant = successor.certify_child_block(super::finality::CertifiedChildBlockInput {
            epoch: Epoch::new(1),
            height: 3,
            timestamp: 1001,
            state_root,
            extra_data: Vec::new(),
            parent_hash: admission.block_hash,
            base_fee_per_gas: None,
        });
        let mut certified = compact(&admission);
        certified.finalization = descendant.finalization.clone();
        certified.descendants.push(compact(&descendant).header);
        let proof = FinalizedAdmissionWitnessV1 {
            admission: certified,
            registry_account: MptAccountProofV1 {
                nonce: account.nonce,
                balance: account.balance,
                storage_root,
                code_hash: account.code_hash,
                nodes: accounts[&TEE_REGISTRY_ADDRESS]
                    .iter()
                    .map(|n| n.to_vec())
                    .collect(),
            },
            registry_storage: slots
                .into_iter()
                .zip(values.iter().copied())
                .zip(storage_proofs)
                .map(|((key, value), nodes)| MptStorageProofV1 {
                    key,
                    value,
                    nodes: nodes.into_iter().map(|n| n.to_vec()).collect(),
                })
                .collect(),
        };
        Self {
            anchor: genesis.outcome(Epoch::new(0)),
            transitions: vec![compact(&transition).encode_canonical().unwrap()],
            proof,
        }
    }
    pub fn request<'a>(
        &'a self,
        (artifact, witness): (&'a [u8], &'a [u8]),
        intent: &'a RegistrationIntentV1,
        authorization: &'a NodeHostAuthorizationWitnessV1,
        context: &DcapOnboardingContextV1,
    ) -> FinalizedOnboardingV1<'a> {
        FinalizedOnboardingV1 {
            artifact,
            intent,
            authorization,
            anchor_outcome: &self.anchor,
            committee_transitions: &self.transitions,
            admission_witness: witness,
            expected_network_encryption_public: context.network_encryption_public,
            expected_key_epoch: context.key_epoch,
            expected_network_encryption_epoch: context.network_encryption_epoch,
        }
    }
}

fn verify_bad_partial_rejected(
    context: &InProcessTeeContext,
    id: &[u8; 32],
    sealed: &[Vec<u8>],
    recipients: &BTreeMap<bls12381::PublicKey, [u8; 32]>,
) {
    use commonware_codec::{EncodeSize as _, Write as _};
    use commonware_cryptography::bls12381::primitives::{ops::threshold, variant::MinSig};
    use outbe_tee::in_process::{
        dkg::{DkgError, DkgOperation, TEE_OFFER_MESSAGE, TEE_OFFER_NAMESPACE},
        InProcessError,
    };
    let counterfeit = outbe_simplex::bls::bootstrap_dkg_for_participants(
        commonware_utils::ordered::Set::from_iter_dedup(recipients.keys().cloned()),
    )
    .unwrap();
    let partial = threshold::sign_message::<MinSig>(
        &counterfeit.shares[0],
        TEE_OFFER_NAMESPACE,
        TEE_OFFER_MESSAGE,
    );
    let mut plaintext = zeroize::Zeroizing::new(Vec::with_capacity(partial.encode_size()));
    partial.write(&mut *plaintext);
    let recipient = context.dkg_enc_public();
    let blob = outbe_tee::tee_encrypt::tee_encrypt(
        &recipient,
        &recipient,
        b"outbe/tee/dkg-share/v1",
        &plaintext,
    )
    .unwrap();
    let mut forged = sealed.to_vec();
    forged[0] = blob.to_wire_bytes();
    assert!(matches!(
        context.complete_founding_dkg(id, &forged, 0),
        Err(InProcessError::Dkg(DkgError::Operation {
            operation: DkgOperation::VerifyOfferPartial
        }))
    ));
    assert!(!context.offer_key_ready().unwrap());
}

pub fn wrong_committee_certificate(proof: &FinalizedAdmissionWitnessV1) -> Vec<u8> {
    use alloy_consensus::BlockHeader as _;
    use alloy_rlp::Decodable as _;
    let header =
        outbe_primitives::OutbeHeader::decode(&mut proof.admission.descendants[0].as_slice())
            .unwrap();
    super::finality::FinalityCommitteeFixture::new(70)
        .certify_child_block(super::finality::CertifiedChildBlockInput {
            epoch: Epoch::new(1),
            height: header.number(),
            timestamp: header.timestamp(),
            state_root: header.state_root(),
            extra_data: header.extra_data().to_vec(),
            parent_hash: header.parent_hash(),
            base_fee_per_gas: None,
        })
        .finalization
}
