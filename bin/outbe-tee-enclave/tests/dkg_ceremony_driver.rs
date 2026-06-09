//! End-to-end TEE DKG ceremony driven by the async [`run_tee_dkg_ceremony`]
//! event loop over an in-memory gossip bus, with N separate real enclaves over
//! UDS + Noise-IK. This exercises the host-side ceremony driver the consensus
//! stack will run (one task per node, messages routed by BLS pubkey) — the same
//! loop that, in the node, runs over the commonware P2P channel.

use std::collections::BTreeMap;
use std::os::unix::net::UnixListener;
use std::thread;

use alloy_primitives::B256;
use tokio::sync::mpsc;

use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{run_tee_dkg_ceremony, CeremonyError, DkgGossip, DkgWireMessage};
use outbe_tee::{CeremonyCoordinator, EnclaveClient, QuotePolicy};
use outbe_tee_enclave::keys::EnclaveKeys;
use outbe_tee_enclave::transport::serve_connection;

const N: usize = 4;

type Envelope = (Vec<u8>, DkgWireMessage);

/// In-memory gossip bus: routes messages between the in-process node tasks by
/// BLS pubkey. Stands in for the consensus P2P channel in the test.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn ceremony_driver_completes_over_in_memory_gossip() {
    let dir = tempfile::tempdir().unwrap();

    // N enclaves: each a distinct identity + a UDS server thread.
    let mut servers = Vec::new();
    let mut clients = Vec::new();
    for i in 0..N {
        let sock = dir.path().join(format!("enclave{i}.sock"));
        let keys = EnclaveKeys::new([i as u8 + 1; 32], None).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        servers.push(thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let offer_key = std::sync::Arc::new(std::sync::OnceLock::new());
                let _ = serve_connection(stream, &keys, &offer_key);
            }
        }));
        clients.push(EnclaveClient::connect(&sock, &QuotePolicy::dev_accept_any()).unwrap());
    }

    // Announce identities (tee_bls_pub, dkg_enc_pub).
    let identities: Vec<(Vec<u8>, [u8; 32])> = clients
        .iter_mut()
        .map(
            |c| match c.request(&EnclaveRequest::GetPublicKeys).unwrap() {
                EnclaveResponse::PublicKeys {
                    tee_bls_pub,
                    dkg_enc_pub,
                    ..
                } => (tee_bls_pub, dkg_enc_pub),
                other => panic!("GetPublicKeys: {other:?}"),
            },
        )
        .collect();

    // One mpsc inbox per node + a shared bls -> sender map.
    let mut receivers = Vec::new();
    let mut senders: BTreeMap<Vec<u8>, mpsc::UnboundedSender<Envelope>> = BTreeMap::new();
    for (bls, _) in &identities {
        let (tx, rx) = mpsc::unbounded_channel();
        senders.insert(bls.clone(), tx);
        receivers.push(rx);
    }

    let ceremony_id = B256::repeat_byte(0x9d);
    let chain_id = B256::repeat_byte(0xc1);

    // Spawn one driver task per node. `receivers.remove(0)` consumes the inboxes
    // in order, so node i gets receiver i.
    let mut tasks = Vec::new();
    for ((bls, _), mut client) in identities.iter().cloned().zip(clients.into_iter()) {
        let coord = CeremonyCoordinator::new(ceremony_id, 0, bls.clone(), identities.clone());
        let mut gossip = InMemoryGossip {
            my_bls: bls,
            senders: senders.clone(),
            receiver: receivers.remove(0),
        };
        tasks.push(tokio::spawn(async move {
            run_tee_dkg_ceremony(&coord, &mut client, &mut gossip, N, chain_id, 0).await
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

    // Seam F over the real Noise-IK transport: every node derives the
    // byte-identical shared offer public key (and a non-zero one).
    let tribute_offer_pub = outcomes[0].tribute_offer_public;
    assert_ne!(
        tribute_offer_pub, [0u8; 32],
        "offer public key must be set by Seam F"
    );
    assert!(
        outcomes
            .iter()
            .all(|o| o.tribute_offer_public == tribute_offer_pub),
        "all driver tasks must derive the same offer public key",
    );

    for s in servers {
        s.join().unwrap();
    }
}
