//! End-to-end TEE DKG ceremony over the real transport.
//!
//! Spins up N enclave servers (each a real UDS + Noise-IK responder), connects N
//! host [`EnclaveClient`]s, and drives a full DKG ceremony with
//! [`CeremonyCoordinator`]s routing messages between peers in-process. Every
//! secret operation crosses the real Noise-IK channel to a separate enclave; the
//! host only relays opaque bytes. This is the localnet ceremony minus the
//! commonware P2P networking (substituted by in-process message routing).

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::UnixListener;
use std::thread;

use alloy_primitives::B256;

use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{Ack, DealerBundle, FinalizedLog};
use outbe_tee::{CeremonyCoordinator, EnclaveClient, QuotePolicy};
use outbe_tee_enclave::keys::EnclaveKeys;
use outbe_tee_enclave::transport::serve_connection;

const N: usize = 4;

#[test]
fn full_dkg_ceremony_over_real_noise_transport() {
    let dir = tempfile::tempdir().unwrap();

    // Spin up N enclaves: each a distinct identity, a UDS, and a server thread
    // serving one connection (the whole ceremony) to completion.
    let mut servers = Vec::new();
    let mut socks = Vec::new();
    for i in 0..N {
        let sock = dir.path().join(format!("enclave{i}.sock"));
        let keys = EnclaveKeys::new([i as u8 + 1; 32], None).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        servers.push(thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let offer_key = std::sync::Arc::new(std::sync::OnceLock::new());
            let _ = serve_connection(stream, &keys, &offer_key);
        }));
        socks.push(sock);
    }

    // Connect a host client per enclave (GetQuote -> verify -> Noise-IK).
    let mut clients: Vec<EnclaveClient> = socks
        .iter()
        .map(|s| EnclaveClient::connect(s, &QuotePolicy::dev_accept_any()).unwrap())
        .collect();

    // Announce each enclave's DKG identity (tee_bls_pub, dkg_enc_pub).
    let identities: Vec<(Vec<u8>, [u8; 32])> = clients
        .iter_mut()
        .map(
            |c| match c.request(&EnclaveRequest::GetPublicKeys).unwrap() {
                EnclaveResponse::PublicKeys {
                    tee_bls_pub,
                    dkg_enc_pub,
                    ..
                } => (tee_bls_pub, dkg_enc_pub),
                other => panic!("unexpected GetPublicKeys: {other:?}"),
            },
        )
        .collect();

    let index_of: BTreeMap<Vec<u8>, usize> = identities
        .iter()
        .enumerate()
        .map(|(i, (bls, _))| (bls.clone(), i))
        .collect();

    let ceremony_id = B256::repeat_byte(0x5c);
    let coords: Vec<CeremonyCoordinator> = identities
        .iter()
        .map(|(bls, _)| CeremonyCoordinator::new(ceremony_id, 0, bls.clone(), identities.clone()))
        .collect();

    // Open the ceremony on every enclave.
    for i in 0..N {
        coords[i].open(&mut clients[i]).expect("open");
    }

    // Seam A: every node deals; route each sealed bundle to its recipient.
    let mut bundle_inbox: Vec<Vec<DealerBundle>> = vec![Vec::new(); N];
    for i in 0..N {
        for addressed in coords[i].deal(&mut clients[i]).expect("deal") {
            let j = index_of[&addressed.to];
            bundle_inbox[j].push(addressed.msg);
        }
    }

    // Seam B: every node opens+verifies its incoming bundles; route acks back.
    let mut ack_inbox: Vec<Vec<Ack>> = vec![Vec::new(); N];
    for j in 0..N {
        let bundles = std::mem::take(&mut bundle_inbox[j]);
        for bundle in &bundles {
            if let Some(addressed) = coords[j].ingest(&mut clients[j], bundle).expect("ingest") {
                let dealer = index_of[&addressed.to];
                ack_inbox[dealer].push(addressed.msg);
            }
        }
    }

    // Seam C: every dealer records the acks it received.
    for i in 0..N {
        let acks = std::mem::take(&mut ack_inbox[i]);
        for ack in &acks {
            coords[i]
                .receive_ack(&mut clients[i], ack)
                .expect("receive_ack");
        }
    }

    // Seam D: every dealer finalizes its log; broadcast (collect all).
    let logs: Vec<FinalizedLog> = (0..N)
        .map(|i| {
            coords[i]
                .finalize_dealer(&mut clients[i])
                .expect("finalize_dealer")
        })
        .collect();

    // Seam E: every node verifies all logs and recovers its threshold share.
    let outcomes: Vec<_> = (0..N)
        .map(|i| {
            coords[i]
                .finalize_player(&mut clients[i], &logs)
                .expect("finalize_player")
        })
        .collect();

    // All parties agree on the public group key; each holds a distinct share.
    let group = &outcomes[0].group_public;
    assert!(
        outcomes.iter().all(|o| &o.group_public == group),
        "all parties must derive the same group key over the real transport",
    );
    assert!(!group.is_empty());
    let commitments: BTreeSet<B256> = outcomes.iter().map(|o| o.share_commitment).collect();
    assert_eq!(commitments.len(), N, "share commitments must be distinct");

    // Closing the clients ends the server loops.
    drop(clients);
    for s in servers {
        s.join().unwrap();
    }
}
