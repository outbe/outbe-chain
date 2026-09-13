use crate::transport::tests::*;

#[test]
fn dkg_open_rejects_forged_enc_signature() {
    let (mut enclaves, mut participants, _) = honest_announces(4);
    // Honest list opens fine.
    assert!(matches!(
        open_on(&mut enclaves[0], participants.clone()),
        EnclaveResponse::Ack
    ));
    // Corrupt one binding signature: the enclave must reject the whole open.
    participants[1].enc_sig[0] ^= 0xff;
    assert!(
        matches!(
            open_on(&mut enclaves[0], participants),
            EnclaveResponse::Error { .. }
        ),
        "forged enc_sig must be rejected"
    );
}

#[test]
fn dkg_open_rejects_mispaired_enc_signature() {
    let (mut enclaves, mut participants, _) = honest_announces(4);
    // Swap two participants' signatures: each now pairs a (bls, enc) with the
    // OTHER party's signature, so neither binding verifies.
    participants.swap(0, 1);
    let s0 = participants[0].enc_sig.clone();
    participants[0].enc_sig = participants[1].enc_sig.clone();
    participants[1].enc_sig = s0;
    assert!(
        matches!(
            open_on(&mut enclaves[2], participants),
            EnclaveResponse::Error { .. }
        ),
        "mispaired enc signature must be rejected"
    );
}

#[test]
fn dkg_open_rejects_duplicate_enc_key() {
    let (mut enclaves, mut participants, _) = honest_announces(4);
    // A host replays one party's full announce into another slot: the enc key
    // (and identity) now collide. The dedup guard must reject before open.
    participants[1] = participants[0].clone();
    assert!(
        matches!(
            open_on(&mut enclaves[3], participants),
            EnclaveResponse::Error { .. }
        ),
        "duplicate enc key must be rejected"
    );
}

#[test]
fn dkg_open_rejects_wrong_chain_binding() {
    let (mut enclaves, mut participants, _) = honest_announces(4);
    // A host cannot replay an otherwise valid signature under another
    // network/ceremony context.
    participants[0].ceremony_id = B256::repeat_byte(0xEE);
    assert!(
        matches!(
            open_on(&mut enclaves[1], participants),
            EnclaveResponse::Error { .. }
        ),
        "enc binding signed under a foreign chain_id must be rejected"
    );
}

/// Drive a full n-party TEE DKG ceremony through `dispatch` (the real protocol
/// request/response path), exercising the byte serialization of every seam.
/// Validates that the protocol-level ceremony converges to one group key with
/// distinct per-party share commitments.
#[test]
fn dispatch_drives_full_dkg_ceremony_over_protocol() {
    let n = 4usize;
    let (mut enclaves, participants, ceremony_id) = honest_announces(n);
    let participant_bls: Vec<Vec<u8>> = participants.iter().map(|p| p.bls_pub.clone()).collect();

    // Open the ceremony on every enclave.
    for e in enclaves.iter_mut() {
        let resp = e.call(EnclaveRequest::DkgOpen {
            ceremony_id,
            round: 0,
            participants: participants.clone(),
        });
        assert!(matches!(resp, EnclaveResponse::Ack), "DkgOpen: {resp:?}");
    }

    // Seam A: every enclave deals; collect (pub_msg, sealed shares per recipient).
    // `Deal` = (encoded pub_msg, recipient BLS -> sealed share bytes).
    type Deal = (Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>);
    let mut deals: Vec<Deal> = Vec::new();
    for e in enclaves.iter_mut() {
        match e.call(EnclaveRequest::DkgStartDealer { ceremony_id }) {
            EnclaveResponse::DkgDealt {
                pub_msg,
                sealed_shares,
            } => deals.push((pub_msg, sealed_shares.into_iter().collect())),
            other => panic!("DkgStartDealer: {other:?}"),
        }
    }

    // Seams B + C: deliver dealer i's sealed share to player j, then the ack
    // back to dealer i.
    for i in 0..n {
        let dealer_bls = participant_bls[i].clone();
        let pub_msg = deals[i].0.clone();
        for j in 0..n {
            let player_bls = participant_bls[j].clone();
            let sealed_share = deals[i]
                .1
                .get(&player_bls)
                .expect("sealed share for player")
                .clone();
            let ack = match enclaves[j].call(EnclaveRequest::DkgPlayerIngest {
                ceremony_id,
                dealer_bls: dealer_bls.clone(),
                pub_msg: pub_msg.clone(),
                sealed_share,
            }) {
                EnclaveResponse::DkgPlayerAck { ack } => ack.expect("valid dealing acks"),
                other => panic!("DkgPlayerIngest: {other:?}"),
            };
            let resp = enclaves[i].call(EnclaveRequest::DkgDealerReceiveAck {
                ceremony_id,
                player_bls,
                ack,
            });
            assert!(
                matches!(resp, EnclaveResponse::Ack),
                "DkgDealerReceiveAck: {resp:?}"
            );
        }
    }

    // Seam D: every dealer finalizes its signed log.
    let signed_logs: Vec<Vec<u8>> = enclaves
        .iter_mut()
        .map(
            |e| match e.call(EnclaveRequest::DkgDealerFinalize { ceremony_id }) {
                EnclaveResponse::DkgSignedLog { signed_log } => signed_log,
                other => panic!("DkgDealerFinalize: {other:?}"),
            },
        )
        .collect();

    // Seam E: every player verifies all logs and recovers its threshold share.
    let mut groups: Vec<Vec<u8>> = Vec::new();
    let mut commitments: Vec<B256> = Vec::new();
    for e in enclaves.iter_mut() {
        match e.call(EnclaveRequest::DkgPlayerFinalize {
            ceremony_id,
            signed_logs: signed_logs.clone(),
        }) {
            EnclaveResponse::DkgPlayerFinalized {
                group_public,
                share_commitment,
            } => {
                groups.push(group_public);
                commitments.push(share_commitment);
            }
            other => panic!("DkgPlayerFinalize: {other:?}"),
        }
    }

    // All parties agree on the group key; each holds a distinct share.
    assert!(
        groups.iter().all(|g| *g == groups[0]),
        "group key must agree"
    );
    assert!(!groups[0].is_empty());
    let mut sorted = commitments.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), n, "share commitments must be distinct");

    // Seam F: every enclave threshold-signs the fixed offer message and SEALS
    // its partial to every recipient (n^2 sealed blobs). The host only ever
    // relays ciphertexts: `(recipient_bls, sealed_blob)`. Each enclave then
    // decrypts in-SGX the blobs addressed to it and recovers the group
    // signature -> shared offer key.
    let mut all_sealed: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for e in enclaves.iter_mut() {
        match e.call(EnclaveRequest::DkgTributeOfferPartial { ceremony_id }) {
            EnclaveResponse::DkgTributeOfferPartial { sealed } => all_sealed.extend(sealed),
            other => panic!("DkgTributeOfferPartial: {other:?}"),
        }
    }

    let chain_id = B256::from(testnet_chain_word());
    let mut tribute_offer_keys: Vec<[u8; 32]> = Vec::new();
    for (i, e) in enclaves.iter_mut().enumerate() {
        let sealed_partials: Vec<Vec<u8>> = all_sealed
            .iter()
            .filter(|(recipient_bls, _)| *recipient_bls == participant_bls[i])
            .map(|(_, blob)| blob.clone())
            .collect();
        match e.call(EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id,
            sealed_partials,
            chain_id,
            tribute_offer_epoch: 0,
        }) {
            EnclaveResponse::DkgTributeOfferKey {
                tribute_offer_public,
                group_public_key,
            } => {
                assert!(
                    !group_public_key.is_empty(),
                    "group public key must be emitted at founding offer finalization"
                );
                tribute_offer_keys.push(tribute_offer_public);
            }
            other => panic!("DkgFinalizeTributeOffer: {other:?}"),
        }
    }
    // Every enclave derives the byte-identical shared offer public key.
    assert!(
        tribute_offer_keys
            .iter()
            .all(|k| *k == tribute_offer_keys[0]),
        "offer public key must agree across enclaves"
    );
    assert_ne!(tribute_offer_keys[0], [0u8; 32]);

    // The ceremony session is released after founding offer-key finalization.
    assert!(enclaves.iter().all(|e| e.dkg.is_empty()));
}

#[test]
fn dispatch_unknown_ceremony_is_typed_error_not_panic() {
    let mut e = Enclave::new(1);
    let resp = e.call(EnclaveRequest::DkgStartDealer {
        ceremony_id: B256::repeat_byte(0xEE),
    });
    assert!(matches!(resp, EnclaveResponse::Error { .. }), "{resp:?}");
}
