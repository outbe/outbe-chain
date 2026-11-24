use super::*;

fn setup(n: u32) -> (CeremonyInfo, Vec<PrivKey>, Vec<PubKey>) {
    let keys: Vec<PrivKey> = (0..n)
        .map(|i| PrivKey::from_seed(u64::from(i) + 1))
        .collect();
    let participant_bls: Vec<Vec<u8>> = keys
        .iter()
        .map(|k| k.public_key().encode().to_vec())
        .collect();
    let (info, pubkeys) = build_ceremony_info(0, &participant_bls).unwrap();
    // Return private keys ordered to match the canonical pubkey order.
    let mut ordered_keys = keys;
    ordered_keys.sort_by_key(|k| k.public_key().encode());
    (info, ordered_keys, pubkeys)
}

/// Deterministic, distinct X25519 secret for party `i`. Varies byte 1, which
/// X25519 clamping leaves untouched (clamping clears the low 3 bits of byte 0,
/// so varying byte 0 in 1..8 would collapse to one scalar).
fn enc_secret_for(i: usize) -> [u8; 32] {
    let mut s = [0x11u8; 32];
    s[1] = i as u8 + 1;
    s
}

/// Build each party's announced X25519 share-encryption pubkey, keyed by BLS
/// pubkey: `(enc_secrets, recipient_enc_keys)`.
fn enc_material(pubkeys: &[PubKey]) -> (Vec<[u8; 32]>, BTreeMap<PubKey, [u8; 32]>) {
    let secrets: Vec<[u8; 32]> = (0..pubkeys.len()).map(enc_secret_for).collect();
    let map = pubkeys
        .iter()
        .zip(&secrets)
        .map(|(pk, sk)| (pk.clone(), x25519_public(sk)))
        .collect();
    (secrets, map)
}

#[test]
fn player_finalize_encoded_rejects_incomplete_dealer_set() {
    // Fix A (C2): a host that feeds fewer than all-n dealer logs would diverge
    // the group key across enclaves. Thus the byte finalize must reject an
    // incomplete set. Drive a 4-party ceremony to per-dealer encoded logs, then
    // finalize.
    let n = 4usize;
    let (info, keys, pubkeys) = setup(n as u32);
    let (enc_secrets, enc_keys) = enc_material(&pubkeys);
    let mut sessions: Vec<DkgSession> = keys
        .iter()
        .zip(&enc_secrets)
        .map(|(k, sk)| {
            DkgSession::new(info.clone(), k.clone(), sk, enc_keys.clone()).expect("session")
        })
        .collect();

    let mut pub_msgs: Vec<CeremonyPubMsg> = Vec::with_capacity(n);
    let mut sealed_shares: Vec<BTreeMap<PubKey, EncryptedShare>> = Vec::with_capacity(n);
    for s in sessions.iter_mut() {
        let (pm, blobs) = s.start_dealer(None).expect("start_dealer");
        pub_msgs.push(pm);
        sealed_shares.push(blobs.into_iter().collect());
    }
    for i in 0..n {
        let dealer_pk = pubkeys[i].clone();
        let pm = pub_msgs[i].clone();
        let mut acks: Vec<(PubKey, CeremonyAck)> = Vec::new();
        for (j, s) in sessions.iter_mut().enumerate() {
            let blob = sealed_shares[i].remove(&pubkeys[j]).expect("sealed share");
            if let Some(ack) = s
                .player_ingest(dealer_pk.clone(), pm.clone(), &blob)
                .expect("player_ingest")
            {
                acks.push((pubkeys[j].clone(), ack));
            }
        }
        for (pk, ack) in acks {
            sessions[i]
                .dealer_receive_ack(pk, ack)
                .expect("dealer_receive_ack");
        }
    }

    let logs: Vec<Vec<u8>> = sessions
        .iter_mut()
        .map(|s| {
            s.dealer_finalize_encoded()
                .expect("dealer_finalize_encoded")
        })
        .collect();

    // Complete set finalizes; the incomplete subset (n-1 dealers) is rejected.
    assert!(
        sessions[1].player_finalize_encoded(&logs).is_ok(),
        "complete dealer set must finalize"
    );
    let err = sessions[0]
        .player_finalize_encoded(&logs[..n - 1])
        .unwrap_err();
    assert!(
        matches!(err, TeeError::Dkg(_)),
        "incomplete dealer set must be rejected: {err:?}"
    );
}

#[test]
fn player_ingest_rejects_share_sealed_to_another_enclave() {
    // A share sealed to a DIFFERENT recipient must not open in this session.
    let (info, keys, pubkeys) = setup(4);

    // The dealer's recipient map points EVERY participant at the dealer's own
    // enc key. Thus the dealer seals the player's share to the wrong key.
    let dealer_enc = enc_secret_for(0);
    let wrong_keys: BTreeMap<PubKey, [u8; 32]> = pubkeys
        .iter()
        .map(|pk| (pk.clone(), x25519_public(&dealer_enc)))
        .collect();
    let mut dealer =
        DkgSession::new(info.clone(), keys[0].clone(), &dealer_enc, wrong_keys).expect("dealer");

    let player_enc = enc_secret_for(1);
    let mut player =
        DkgSession::new(info, keys[1].clone(), &player_enc, BTreeMap::new()).expect("player");

    let (pub_msg, blobs) = dealer.start_dealer(None).expect("start_dealer");
    let player_pk = player.participant_pubkey();
    let blob = blobs
        .into_iter()
        .find(|(pk, _)| *pk == player_pk)
        .expect("share for player")
        .1;

    let err = player
        .player_ingest(dealer.participant_pubkey(), pub_msg, &blob)
        .unwrap_err();
    assert!(matches!(err, TeeError::Crypto(_) | TeeError::Dkg(_)));
}

#[test]
fn seam_order_violations_are_typed_errors_not_panics() {
    let (info, keys, pubkeys) = setup(4);
    let (enc_secrets, enc_keys) = enc_material(&pubkeys);
    let mut session =
        DkgSession::new(info, keys[0].clone(), &enc_secrets[0], enc_keys).expect("session");

    // dealer_finalize before start_dealer -> typed error.
    assert!(matches!(session.dealer_finalize(), Err(TeeError::Seam(_))));

    session.start_dealer(None).expect("start_dealer");
    // Second start_dealer -> typed error.
    assert!(matches!(session.start_dealer(None), Err(TeeError::Seam(_))));
}

/// Drive all resident sessions through seams A-E, retaining shares for Seam F.
fn drive_ceremony_to_shares(n: u32) -> Vec<DkgSession> {
    let (info, keys, pubkeys) = setup(n);
    let count = n as usize;
    let (enc_secrets, enc_keys) = enc_material(&pubkeys);
    let mut sessions: Vec<DkgSession> = keys
        .iter()
        .zip(&enc_secrets)
        .map(|(k, sk)| {
            DkgSession::new(info.clone(), k.clone(), sk, enc_keys.clone()).expect("session")
        })
        .collect();

    let mut pub_msgs: Vec<CeremonyPubMsg> = Vec::with_capacity(count);
    let mut sealed_shares: Vec<BTreeMap<PubKey, EncryptedShare>> = Vec::with_capacity(count);
    for session in sessions.iter_mut() {
        let (pub_msg, blobs) = session.start_dealer(None).expect("start_dealer");
        pub_msgs.push(pub_msg);
        sealed_shares.push(blobs.into_iter().collect());
    }
    for i in 0..count {
        let dealer_pk = pubkeys[i].clone();
        let pub_msg = pub_msgs[i].clone();
        let mut acks: Vec<(PubKey, CeremonyAck)> = Vec::with_capacity(count);
        for (j, session) in sessions.iter_mut().enumerate() {
            let player_pk = pubkeys[j].clone();
            let blob = sealed_shares[i].remove(&player_pk).expect("sealed share");
            if let Some(ack) = session
                .player_ingest(dealer_pk.clone(), pub_msg.clone(), &blob)
                .expect("player_ingest")
            {
                acks.push((player_pk, ack));
            }
        }
        for (player_pk, ack) in acks {
            sessions[i]
                .dealer_receive_ack(player_pk, ack)
                .expect("dealer_receive_ack");
        }
    }
    let mut dealer_logs: Vec<(PubKey, CeremonyLog)> = Vec::with_capacity(count);
    for (i, session) in sessions.iter_mut().enumerate() {
        let signed = session.dealer_finalize().expect("dealer_finalize");
        let (dealer_pk, log) = verify_dealer_log(&info, signed).expect("dealer log verifies");
        assert_eq!(dealer_pk, pubkeys[i]);
        dealer_logs.push((dealer_pk, log));
    }
    for session in sessions.iter_mut() {
        session
            .player_finalize(dealer_logs.clone())
            .expect("player_finalize");
    }
    sessions
}

#[test]
fn seam_f_below_quorum_partials_errors() {
    use commonware_cryptography::Signer as _;
    let sessions = drive_ceremony_to_shares(4);
    let my_pk = sessions[0].signing_key.public_key();
    // Only 2 ciphertexts addressed to session 0 (< quorum 3 for n=4).
    let for_me: Vec<Vec<u8>> = sessions
        .iter()
        .take(2)
        .map(|s| {
            let sealed = s.offer_partials_sealed().expect("seal");
            sealed
                .into_iter()
                .find(|(r, _)| *r == my_pk)
                .map(|(_, b)| b.to_wire_bytes())
                .expect("blob for me")
        })
        .collect();
    assert!(sessions[0]
        .recover_offer_material(&for_me, B256::ZERO, 0)
        .is_err());
}

/// Seam F before player finalization returns typed errors without resident share/output.
#[test]
fn seam_f_before_finalize_is_typed_error() {
    let (info, keys, pubkeys) = setup(4);
    let (enc_secrets, enc_keys) = enc_material(&pubkeys);
    let session =
        DkgSession::new(info, keys[0].clone(), &enc_secrets[0], enc_keys).expect("session");
    assert!(matches!(
        session.offer_partials_sealed(),
        Err(TeeError::Seam(_))
    ));
    assert!(matches!(
        session.recover_offer_material(&[], B256::ZERO, 0),
        Err(TeeError::Seam(_))
    ));
}

/// Public gossip ciphertexts cannot supply a quorum of plaintext offer partials.
#[test]
fn offer_key_not_recoverable_by_host_from_sealed_partials() {
    let sessions = drive_ceremony_to_shares(4);
    let chain_id = B256::repeat_byte(0xc1);
    let epoch = 7;
    let sealed: Vec<_> = sessions
        .iter()
        .map(|s| s.offer_partials_sealed().unwrap())
        .collect();
    let materials: Vec<_> = sessions
        .iter()
        .map(|session| {
            let recipient = session.participant_pubkey();
            let addressed: Vec<_> = sealed
                .iter()
                .flat_map(|signer| {
                    signer
                        .iter()
                        .filter(|(pk, _)| *pk == recipient)
                        .map(|(_, blob)| blob.to_wire_bytes())
                })
                .collect();
            session
                .recover_offer_material(&addressed, chain_id, epoch)
                .unwrap()
        })
        .collect();
    let public = materials[0].public();
    assert_ne!(public, [0; 32]);
    let nonce = [7; 12];
    let plaintext = b"resident offer-key agreement";
    let state = materials[0]
        .derive_state_key_handle(b"outbe/state/v1/", chain_id, epoch)
        .unwrap();
    let ciphertext = state.aead_encrypt(&nonce, plaintext).unwrap();
    for material in &materials {
        assert_eq!(material.public(), public);
        // Opaque-handle interoperability preserves the old secret-equality check without exporting it.
        let state = material
            .derive_state_key_handle(b"outbe/state/v1/", chain_id, epoch)
            .unwrap();
        assert_eq!(
            state.aead_decrypt(&nonce, &ciphertext).unwrap().as_slice(),
            plaintext
        );
    }
    let decodable = sealed
        .iter()
        .flatten()
        .filter(|(_, blob)| {
            let bytes = blob.to_wire_bytes();
            PartialSignature::<Variant>::read(&mut Copying(bytes.as_slice())).is_ok()
        })
        .count();
    assert!(
        decodable < 3,
        "host cannot assemble a quorum of plaintext partials from ciphertext; decoded {decodable}"
    );
}
