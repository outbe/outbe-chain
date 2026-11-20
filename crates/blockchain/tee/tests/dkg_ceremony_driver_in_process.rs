//! Real resident DKG through the consensus ceremony event loop and in-memory gossip.
use std::collections::BTreeMap;

use alloy_primitives::B256;
use tokio::sync::mpsc;

use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{
    run_tee_dkg_ceremony, CeremonyError, DkgGossip, DkgWireMessage, FoundingCeremonyParameters,
};
use outbe_tee::test_utils::resident_context;
use outbe_tee::{tee_dkg::EnclaveChannel as _, CeremonyCoordinator};

const N: usize = 4;

type Envelope = (Vec<u8>, DkgWireMessage);

/// In-memory gossip bus: routes messages between the in-process node tasks by
/// BLS pubkey. It replaces the consensus P2P channel in the test.
struct InMemoryGossip {
    my_bls: Vec<u8>,
    senders: BTreeMap<Vec<u8>, mpsc::UnboundedSender<Envelope>>,
    receiver: mpsc::UnboundedReceiver<Envelope>,
}

impl DkgGossip for InMemoryGossip {
    async fn send(&mut self, to: &[u8], msg: DkgWireMessage) -> Result<(), CeremonyError> {
        if let Some(tx) = self.senders.get(to) {
            let _ = tx.send((self.my_bls.clone(), msg));
        }
        Ok(())
    }

    async fn broadcast(&mut self, msg: DkgWireMessage) -> Result<(), CeremonyError> {
        for (bls, tx) in &self.senders {
            if bls != &self.my_bls {
                let _ = tx.send((self.my_bls.clone(), msg.clone()));
            }
        }
        Ok(())
    }

    async fn recv(&mut self) -> Option<Envelope> {
        self.receiver.recv().await
    }
}

#[test]
fn ceremony_driver_completes_over_in_memory_gossip() {
    use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
    commonware_runtime::deterministic::Runner::default().start(|context| async move {
        let network_binding = outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
            chain_id: alloy_primitives::U256::from(outbe_primitives::chain::TESTNET_CHAIN_ID)
                .to_be_bytes(),
            genesis_hash: B256::repeat_byte(0x9c),
            attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
        };

        let contexts: Vec<_> = (0..N)
            .map(|i| std::sync::Arc::new(resident_context(i as u8 + 1, network_binding)))
            .collect();
        let mut clients: Vec<_> = contexts.iter().map(|c| c.as_ref()).collect();

        let participant_bls = clients
            .iter_mut()
            .map(
                |client| match client.request(&EnclaveRequest::GetPublicKeys).unwrap() {
                    EnclaveResponse::PublicKeys { tee_bls_pub, .. } => tee_bls_pub,
                    other => panic!("GetPublicKeys: {other:?}"),
                },
            )
            .collect::<Vec<_>>();
        let participant_set_hash =
            outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
                .unwrap();
        let ceremony_id = outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(
            &network_binding,
            0,
            participant_set_hash,
        )
        .unwrap();
        let identities = clients
            .iter_mut()
            .map(|client| {
                match client
                    .request(&EnclaveRequest::DkgParticipantAnnounceV1 {
                        ceremony_id,
                        round: 0,
                        participant_bls: participant_bls.clone(),
                    })
                    .unwrap()
                {
                    EnclaveResponse::DkgParticipantAnnounceV1 { participant } => participant,
                    other => panic!("unexpected DKG announcement: {other:?}"),
                }
            })
            .collect::<Vec<_>>();

        // One mpsc inbox per node + a shared bls -> sender map.
        let mut receivers = Vec::new();
        let mut senders: BTreeMap<Vec<u8>, mpsc::UnboundedSender<Envelope>> = BTreeMap::new();
        for p in &identities {
            let (tx, rx) = mpsc::unbounded_channel();
            senders.insert(p.bls_pub.clone(), tx);
            receivers.push(rx);
        }

        let chain_id = B256::from(network_binding.chain_id);

        // Spawn one driver task per node. `receivers.remove(0)` consumes the inboxes
        // in order, so node i gets receiver i.
        let mut tasks = Vec::new();
        for (p, context_owner) in identities.iter().cloned().zip(contexts) {
            let bls = p.bls_pub.clone();
            let coord = CeremonyCoordinator::new(ceremony_id, 0, bls.clone(), identities.clone());
            let mut gossip = InMemoryGossip {
                my_bls: bls,
                senders: senders.clone(),
                receiver: receivers.remove(0),
            };
            tasks.push(context.child("ceremony").spawn(move |_ctx| async move {
                let mut client = context_owner.as_ref();
                run_tee_dkg_ceremony(
                    &coord,
                    &mut client,
                    &mut gossip,
                    FoundingCeremonyParameters {
                        participant_count: N,
                        chain_id,
                        network_key_epoch: 0,
                    },
                )
                .await
            }));
        }

        // Drop the original sender map so channels close once all tasks finish.
        drop(senders);

        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.unwrap().expect("ceremony completes"));
        }

        let group = &outcomes[0].group_public;
        assert!(
            outcomes.iter().all(|o| &o.group_public == group),
            "all driver tasks must derive the same group key",
        );
        assert!(!group.is_empty());
        let commitments: std::collections::BTreeSet<B256> =
            outcomes.iter().map(|o| o.share_commitment).collect();
        assert_eq!(commitments.len(), N, "share commitments must be distinct");

        // Seam F in resident contexts: every node derives the
        // byte-identical shared network encryption public key (and a non-zero one).
        let network_key_pub = outcomes[0].network_encryption_public_key;
        assert_ne!(
            network_key_pub, [0u8; 32],
            "network encryption public key must be set by Seam F"
        );
        assert!(
            outcomes
                .iter()
                .all(|o| o.network_encryption_public_key == network_key_pub),
            "all driver tasks must derive the same network encryption public key",
        );
    });
}
