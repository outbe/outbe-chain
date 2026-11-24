//! Run the real startup coordinators with resident owners and Commonware transport traits.
use crate::tee_bootstrap::*;
use commonware_actor::{Feedback, Unreliable};
use commonware_cryptography::Signer as _;
use commonware_p2p::{CheckedSender, LimitedSender, Message, Receiver};
use commonware_runtime::{deterministic, IoBufs, Runner as _, Supervisor as _};
use outbe_tee::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
};
use std::{collections::BTreeMap, convert::Infallible, sync::Arc, time::SystemTime};

fn policy() -> TeePolicyV1 {
    // Canonical golden bytes keep this fixture independent of the submission builder.
    let bytes = hex::decode(concat!(
            "010000000000000001101010101010101010101010101010101010101010101010101010101010101011111111111111",
            "111111111111111111111111111111111111111111111111110000000000000001000000000000000000000000000000",
            "000000000000000000000000000000000002727272727272727272727272727272727272727272727272727272727272",
            "72720003000000000002939a7233f79c4ca9940a0db3957f0607000503020000000102010000000000000e1000000000",
            "00093a800000000000000e107d7e959b073b078b70bfbca6bb532d099684958f711e048649d7a8fd75fb2eca00010181",
            "818181818181818181818181818181818181818181818181818181818181818282828282828282828282828282828282",
            "82828282828282828282828282828200010002000000000000000100000000000003e8"
        )).unwrap();
    TeePolicyV1::decode_canonical(&bytes).unwrap()
}

fn context(seed: u8, policy: &TeePolicyV1) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let node = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        attestation_mode: policy.attestation_mode,
        node_id: NodeIdV1 {
            reth_p2p_public: node
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let identity = outbe_tee::NodeHostIdentityV1 {
        network_binding: manifest.network_binding(),
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let descriptor = outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: vec![],
    };
    InProcessTeeContext::new(
        keys,
        identity,
        manifest,
        descriptor,
        ProofSourceV1::NoHardware,
    )
    .unwrap()
}

type Routes = Arc<
    BTreeMap<bls12381::PublicKey, tokio::sync::mpsc::UnboundedSender<Message<bls12381::PublicKey>>>,
>;
#[derive(Clone)]
struct Sender {
    from: bls12381::PublicKey,
    routes: Routes,
}
struct Checked {
    sender: Sender,
    recipients: Vec<bls12381::PublicKey>,
}
impl CheckedSender for Checked {
    type PublicKey = bls12381::PublicKey;
    fn recipients(&self) -> Vec<Self::PublicKey> {
        self.recipients.clone()
    }
    fn send(self, message: impl Into<IoBufs> + Send, _priority: bool) -> Unreliable<Feedback> {
        let bytes = message.into().coalesce().as_ref().to_vec();
        for peer in self.recipients {
            // Completed peers may drop their startup receiver before late delivery ACKs.
            let _ =
                self.sender.routes[&peer].send((self.sender.from.clone(), bytes.clone().into()));
        }
        Unreliable::Outcome(Feedback::Ok)
    }
}
impl LimitedSender for Sender {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = Checked;
    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, SystemTime> {
        let recipients = match recipients {
            Recipients::All => self
                .routes
                .keys()
                .filter(|p| *p != &self.from)
                .cloned()
                .collect(),
            Recipients::One(p) => vec![p],
            Recipients::Some(p) => p,
        };
        Ok(Checked {
            sender: self.clone(),
            recipients,
        })
    }
}
#[derive(Debug)]
struct Inbox(tokio::sync::mpsc::UnboundedReceiver<Message<bls12381::PublicKey>>);
impl Receiver for Inbox {
    type Error = Infallible;
    type PublicKey = bls12381::PublicKey;
    fn capacity(&self) -> std::num::NonZeroUsize {
        commonware_utils::NZUsize!(64)
    }
    async fn recv(&mut self) -> Result<Message<Self::PublicKey>, Self::Error> {
        Ok(self.0.recv().await.unwrap())
    }
}
fn network(peers: &[bls12381::PublicKey]) -> Vec<(Sender, Inbox)> {
    let (routes, inboxes): (BTreeMap<_, _>, Vec<_>) = peers
        .iter()
        .map(|p| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            ((p.clone(), tx), Inbox(rx))
        })
        .unzip();
    let routes = Arc::new(routes);
    peers
        .iter()
        .zip(inboxes)
        .map(|(p, inbox)| {
            (
                Sender {
                    from: p.clone(),
                    routes: routes.clone(),
                },
                inbox,
            )
        })
        .collect()
}

#[test]
fn in_process_founding_ceremony_without_socket() {
    deterministic::Runner::default().start(|clock| async move {
        let mut policy = policy();
        policy.attestation_mode = AttestationMode::NoAttestationDev;
        let peers: Vec<_> = (1..=4)
            .map(|seed| bls12381::PrivateKey::from_seed(seed).public_key())
            .collect();
        let signers: Vec<_> = (1..=4)
            .map(|seed| OutbeEvmSigner::from_secret_bytes([seed; 32]).unwrap())
            .collect();
        let committee: BTreeSet<_> = signers.iter().map(|s| s.address()).collect();
        let futures = network(&peers)
            .into_iter()
            .zip(network(&peers))
            .zip(signers)
            .enumerate()
            .map(|(i, ((dkg, bootstrap), signer))| {
                let peers = peers.clone();
                let policy = policy.clone();
                let committee = committee.clone();
                let clock = clock.child("ceremony");
                async move {
                    let context = context((i + 1) as u8, &policy);
                    let allowed = peers.iter().filter(|p| *p != &peers[i]).cloned().collect();
                    let (public, group) = run_tee_dkg_at_startup(
                        &mut &context,
                        TeeDkgStartup {
                            participant_count: 4,
                            network_binding: policy.network_binding(),
                            network_key_epoch: 0,
                        },
                        StartupGossipTransport {
                            sender: dkg.0,
                            receiver: dkg.1,
                            clock: clock.child("ceremony"),
                            allowed_remote_peers: allowed,
                        },
                    )
                    .await
                    .unwrap();
                    assert_eq!(context.network_encryption_public().unwrap(), public);
                    let submission = build_in_process_tee_bootstrap_submission_v2(
                        &context,
                        LocalRegistrationRequest {
                            production_manifest: None,
                            node_id: context.manifest().node_id.clone(),
                            policy: &policy,
                            requested_valid_until: 7200,
                        },
                        &signer,
                        |hash| {
                            use k256::ecdsa::signature::hazmat::PrehashSigner as _;
                            let key =
                                k256::ecdsa::SigningKey::from_slice(&[(i + 1) as u8; 32]).unwrap();
                            let (sig, rec): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
                                key.sign_prehash(hash.as_slice()).map_err(
                                    crate::errors::TeeBootstrapError::node_host_sign_prehash,
                                )?;
                            let mut bytes = [0; 65];
                            bytes[..64].copy_from_slice(&sig.to_bytes());
                            bytes[64] = rec.to_byte();
                            Ok(bytes)
                        },
                    )
                    .unwrap();
                    let authority = TeeBootstrapAuthorityV2 {
                        policy,
                        committee_snapshot_hash: B256::repeat_byte(4),
                        committee_snapshot_block: 1,
                        key_epoch: 0,
                        network_encryption_epoch: 0,
                        dkg_transcript_hash: B256::ZERO,
                        network_encryption_public_key: public.into(),
                        network_encryption_group_public_key: group.into(),
                    };
                    let allowed = peers.iter().filter(|p| *p != &peers[i]).cloned().collect();
                    let payload = run_tee_bootstrap_v2_at_startup(
                        TeeBootstrapStartup {
                            local_submission: submission,
                            authority,
                            committee,
                            evm_signer: &signer,
                        },
                        StartupGossipTransport {
                            sender: bootstrap.0,
                            receiver: bootstrap.1,
                            clock,
                            allowed_remote_peers: allowed,
                        },
                    )
                    .await
                    .unwrap();
                    assert_eq!(payload.participants.len(), 4);
                    assert_eq!(payload.committee_signatures.len(), 4);
                    payload.preflight().unwrap();
                    (public, payload)
                }
            });
        let results = futures::future::join_all(futures).await;
        for result in &results[1..] {
            assert_eq!(result, &results[0]);
        }
    });
}

#[test]
fn ost3_binding_and_gossip_domains_match_frozen_golden_vectors() {
    assert_eq!(OST3_BINDING_ID_DOMAIN, b"outbe/tee/bootstrap-binding/v1");
    assert_eq!(OST3_SCOPE_DOMAIN, b"outbe/tee/bootstrap-v2-gossip/v1");
    assert_eq!(
        submission::bootstrap_binding_id(
            B256::repeat_byte(1),
            B256::repeat_byte(2),
            B256::repeat_byte(3)
        ),
        "4c2a6a9268d35ad8f2883e60d6aaabcac1c8778561b86d83e0887795af555267"
            .parse::<B256>()
            .unwrap()
    );
    let mut policy = policy();
    policy.chain_id = [1; 32];
    policy.genesis_hash = B256::repeat_byte(2);
    let authority = TeeBootstrapAuthorityV2 {
        policy,
        committee_snapshot_hash: B256::repeat_byte(4),
        committee_snapshot_block: 1,
        key_epoch: 0,
        network_encryption_epoch: 0,
        dkg_transcript_hash: B256::ZERO,
        network_encryption_public_key: B256::repeat_byte(5),
        network_encryption_group_public_key: vec![].into(),
    };
    assert_eq!(
        startup::bootstrap_gossip_scope(&authority, B256::repeat_byte(3)),
        "90a228cee734b3c48e0822d04b74fc438d55d3c7e1558f4a354f2e4cf5339dd1"
            .parse::<B256>()
            .unwrap()
    );
}
