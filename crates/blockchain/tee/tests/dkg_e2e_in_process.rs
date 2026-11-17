//! Resident four-party DKG with opaque encrypted share routing.
use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::B256;

use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{Ack, DealerBundle, FinalizedLog};
use outbe_tee::{
    in_process::InProcessTeeContext, tee_dkg::EnclaveChannel as _, CeremonyCoordinator,
};
#[path = "dkg_e2e_in_process/support.rs"]
mod support;

const N: usize = 4;

#[test]
fn dkg_rejects_a_ceremony_id_from_another_network_binding() {
    let expected_binding = outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
        chain_id: alloy_primitives::U256::from(outbe_primitives::chain::TESTNET_CHAIN_ID)
            .to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x5b),
        attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::NoAttestationDev,
    };
    let foreign_binding = outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
        genesis_hash: B256::repeat_byte(0x6c),
        ..expected_binding
    };
    let context = support::context(0x11, foreign_binding);
    let mut client = &context;
    let participant_bls = match client.request(&EnclaveRequest::GetPublicKeys).unwrap() {
        EnclaveResponse::PublicKeys { tee_bls_pub, .. } => vec![tee_bls_pub],
        other => panic!("unexpected GetPublicKeys: {other:?}"),
    };
    let participant_set_hash =
        outbe_primitives::tee_attestation_v1::dkg_participant_set_hash_v1(&participant_bls)
            .unwrap();
    let ceremony_id = outbe_primitives::tee_attestation_v1::dkg_ceremony_id_v1(
        &expected_binding,
        0,
        participant_set_hash,
    )
    .unwrap();

    let error = client
        .request(&EnclaveRequest::DkgParticipantAnnounceV1 {
            ceremony_id,
            round: 0,
            participant_bls,
        })
        .unwrap_err();
    assert!(matches!(
        error,
        outbe_tee::TransportError::InProcess(outbe_tee::InProcessDispatchError::Resident(
            outbe_tee::in_process::InProcessError::IdentityMismatch
        ))
    ));
}

#[test]
fn full_dkg_ceremony_in_process() {
    let network_binding = outbe_primitives::tee_attestation_v1::NetworkBindingV1 {
        chain_id: alloy_primitives::U256::from(outbe_primitives::chain::TESTNET_CHAIN_ID)
            .to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x5b),
        attestation_mode: outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
    };

    let contexts: Vec<_> = (0..N)
        .map(|i| support::context(i as u8 + 1, network_binding))
        .collect();
    let mut clients: Vec<_> = contexts.iter().collect();

    let participant_bls = clients
        .iter_mut()
        .map(
            |client| match client.request(&EnclaveRequest::GetPublicKeys).unwrap() {
                EnclaveResponse::PublicKeys { tee_bls_pub, .. } => tee_bls_pub,
                other => panic!("unexpected GetPublicKeys: {other:?}"),
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
    let identities = collect_participant_announcements(&mut clients, ceremony_id, &participant_bls);

    let index_of: BTreeMap<Vec<u8>, usize> = identities
        .iter()
        .enumerate()
        .map(|(i, p)| (p.bls_pub.clone(), i))
        .collect();

    let coords: Vec<CeremonyCoordinator> = identities
        .iter()
        .map(|p| CeremonyCoordinator::new(ceremony_id, 0, p.bls_pub.clone(), identities.clone()))
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

    // Seam B: every node opens+verifies its incoming bundles. Route acks back.
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

    // Seam D: every dealer finalizes its log. Broadcast (collect all).
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

    // All parties agree on the public group key. Each holds a distinct share.
    let group = &outcomes[0].group_public;
    assert!(
        outcomes.iter().all(|o| &o.group_public == group),
        "all parties must derive the same group key in resident contexts",
    );
    assert!(!group.is_empty());
    let commitments: BTreeSet<B256> = outcomes.iter().map(|o| o.share_commitment).collect();
    assert_eq!(commitments.len(), N, "share commitments must be distinct");
}

fn collect_participant_announcements(
    clients: &mut [&InProcessTeeContext],
    ceremony_id: B256,
    participant_bls: &[Vec<u8>],
) -> Vec<outbe_tee::protocol::ParticipantAnnounce> {
    let announcement = EnclaveRequest::DkgParticipantAnnounceV1 {
        ceremony_id,
        round: 0,
        participant_bls: participant_bls.to_vec(),
    };
    let mut identities = Vec::with_capacity(clients.len());
    for client in clients {
        let response = client.request(&announcement).unwrap();
        let EnclaveResponse::DkgParticipantAnnounceV1 { participant } = response else {
            panic!("unexpected DKG announcement: {response:?}");
        };
        identities.push(participant);
    }
    identities
}
