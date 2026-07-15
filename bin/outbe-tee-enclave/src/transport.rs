//! Enclave-side transport: framed UDS server + Noise-IK responder + dispatch.
//!
//! Per connection: cleartext `GetQuote` (so the host can pin the attested Noise
//! static key) -> Noise-IK responder handshake -> encrypted request/response
//! loop until the peer closes. Fully blocking; one connection at a time (PoC).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, OnceLock};

use zeroize::Zeroizing;

use outbe_tee::codec::{decode_request, encode_response, read_frame, write_frame};
use outbe_tee::errors::TransportError;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::NOISE_PARAMS;

use crate::dkg::{build_ceremony_info, DkgSessionStore};
use crate::keys::EnclaveKeys;
use crate::process::process_tribute_offer_batch;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    KeyPolicy, SealHeader, SEAL_FORMAT,
};

/// The tribute offer key derived once from the DKG group threshold signature
/// (Seam F): the secret stays resident, clients encrypt to `public`. Written on
/// the DKG connection's `DkgRecoverTributeOffer`, read by the offer-decrypt path on a
/// different connection — hence shared across connection threads. Also carries the
/// resident **group threshold signature** `group_sig` (the Seam F output): it seals
/// on restart so the boot fast-path restores the offer key without re-running the
/// DKG, AND it is the payload a committee key-handoff seals to a newcomer so
/// the new member derives the byte-identical offer key for any epoch locally.
pub struct DerivedTributeOfferKey {
    secret: Zeroizing<[u8; 32]>,
    public: [u8; 32],
    group_sig: Zeroizing<Vec<u8>>,
}

impl DerivedTributeOfferKey {
    /// The resident offer secret (never leaves the enclave).
    fn secret(&self) -> &[u8; 32] {
        &self.secret
    }
    /// The offer public key clients encrypt to (registered on-chain at bootstrap).
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
    /// The resident group threshold signature (Seam F output). Never leaves the
    /// enclave unsealed; sealed for restart and handed off (sealed) to newcomers.
    fn group_sig(&self) -> &[u8] {
        &self.group_sig
    }
    /// Build from an explicit secret + public + group signature (the
    /// `DkgRecoverTributeOffer` / handoff-ingest arms, which already hold the public).
    fn from_parts(secret: [u8; 32], public: [u8; 32], group_sig: Vec<u8>) -> Self {
        Self {
            secret: Zeroizing::new(secret),
            public,
            group_sig: Zeroizing::new(group_sig),
        }
    }
    /// Reconstruct from a resident offer secret + group signature (recomputes the
    /// public key). Used by the seal/unseal boot path to restore the DKG-derived
    /// offer key on restart without re-running the ceremony.
    fn from_secret_and_group_sig(secret: [u8; 32], group_sig: Vec<u8>) -> Self {
        let public = crate::crypto::x25519_public(&secret);
        Self::from_parts(secret, public, group_sig)
    }
}

/// Process-wide, write-once slot for the DKG-derived offer key, shared across
/// every connection thread. `OnceLock` makes the first ceremony's key canonical;
/// a divergent re-derivation is rejected by the `DkgRecoverTributeOffer` arm. No
/// `StorageHandle` exists in this binary, so std sync primitives apply here.
pub type SharedTributeOfferKey = Arc<OnceLock<DerivedTributeOfferKey>>;

/// The TSEAL sealing key + its policy, or `None` when no confidential key is
/// available. Real `EGETKEY(MRSIGNER)` under `gramine-sgx`; a fixed mock key
/// under `mock`/test (stable across rebuilds, simulating MRSIGNER); nothing under
/// `gramine-direct` prod, where there is no confidential at-rest persistence.
fn sealing_key() -> Option<([u8; 32], KeyPolicy)> {
    if let Ok(k) = crate::gramine::sealing_key_256(true) {
        return Some((k, KeyPolicy::MrSigner));
    }
    #[cfg(any(test, feature = "mock"))]
    {
        Some((crate::seal::MOCK_SEALING_KEY, KeyPolicy::Mock))
    }
    #[cfg(not(any(test, feature = "mock")))]
    {
        None
    }
}

/// Write `data` to `path` with mode 0600, atomically (temp file + rename).
fn atomic_write_0600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Restore the offer key + group signature from the sealed blob at boot (restart
/// fast-path). Returns `None` when there is no blob, no sealing key, or unseal
/// fails — the caller then runs the DKG ceremony (or a key-handoff) to obtain the
/// key. The `chain_id` AAD and the running `isv_svn` floor (anti-rollback) come
/// from the boot config.
pub fn unseal_tribute_offer_and_group_sig_on_boot(
    cfg: &EnclaveBootConfig,
) -> Option<DerivedTributeOfferKey> {
    let path = cfg.sealed_root_path();
    let blob = std::fs::read(&path).ok()?;
    let (key, _policy) = sealing_key()?;
    match unseal_tribute_offer_and_group_sig(&blob, &key, cfg.chain_id, cfg.isv_svn) {
        Ok((secret, group_sig, _header)) => {
            eprintln!(
                "outbe-tee-enclave: unsealed offer key + group signature <- {} (restart fast-path)",
                path.display()
            );
            Some(DerivedTributeOfferKey::from_secret_and_group_sig(
                *secret,
                group_sig.to_vec(),
            ))
        }
        Err(e) => {
            // A persistent failure here usually means a STALE sealed blob written by a
            // pre-upgrade binary (incompatible seal format / wrong MRSIGNER / rolled-back
            // isv_svn), not a transient error. This node recovers by re-deriving the key
            // (fresh-chain DKG, or a key-handoff on an existing chain) — but if EVERY
            // committee node carries a stale blob, none can serve the handoff and the
            // offer key is unrecoverable. If this recurs across restarts, delete the
            // sealed blob (clear `--tee-dir`) and re-bootstrap from a clean state.
            eprintln!(
                "outbe-tee-enclave: unseal {} failed ({e}); will re-derive via DKG/handoff. \
                 If this persists after an upgrade, the blob is stale — clear --tee-dir and re-bootstrap.",
                path.display()
            );
            None
        }
    }
}

/// Persist the DKG-derived offer secret + the group threshold signature sealed
/// under the enclave's MRSIGNER key. Write-once and best-effort: no-op without a
/// boot config, before the offer key exists, without a sealing key (gramine-direct
/// prod => not confidential), or when the blob already exists. Never fatal —
/// sealing is local persistence, not consensus state.
fn seal_tribute_offer_and_group_sig_if_configured(
    boot: Option<&EnclaveBootConfig>,
    offer_key: &SharedTributeOfferKey,
) {
    let Some(cfg) = boot else { return };
    let Some(derived) = offer_key.get() else {
        return;
    };
    let path = cfg.sealed_root_path();
    if path.exists() {
        return;
    }
    let Some((key, policy)) = sealing_key() else {
        eprintln!(
            "outbe-tee-enclave: no sealing key (not gramine-sgx / not mock) — offer key \
             NOT persisted (no confidential at-rest storage on this platform)"
        );
        return;
    };
    let mut nonce = [0u8; 12];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).is_err() {
        eprintln!("outbe-tee-enclave: seal nonce RNG failed — offer key NOT persisted");
        return;
    }
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy: policy,
        isv_svn: cfg.isv_svn,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        nonce,
    };
    match seal_tribute_offer_and_group_sig(
        derived.secret(),
        derived.group_sig(),
        &key,
        cfg.chain_id,
        &header,
    ) {
        Ok(blob) => match atomic_write_0600(&path, &blob) {
            Ok(()) => eprintln!(
                "outbe-tee-enclave: sealed offer key + group signature -> {}",
                path.display()
            ),
            Err(e) => eprintln!("outbe-tee-enclave: write {} failed: {e}", path.display()),
        },
        Err(e) => eprintln!("outbe-tee-enclave: seal_tribute_offer_and_group_sig failed: {e}"),
    }
}

/// Serve a single client connection end-to-end (no boot config / sealing).
/// Thin wrapper over [`serve_connection_with`]; kept for tests and callers that
/// do not seal.
pub fn serve_connection<S: Read + Write>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
) -> Result<(), TransportError> {
    serve_connection_with(stream, keys, offer_key, None)
}

/// Serve a single client connection end-to-end. `offer_key` is the shared,
/// write-once DKG-derived offer key slot (populated by the DKG connection's
/// Seam F, read by the offer-decrypt path). `boot` carries the seal/unseal
/// configuration (chain_id / tee-dir / isv_svn); when `Some`, the sealing path
/// persists the offer secret + threshold share after Seam F.
pub fn serve_connection_with<S: Read + Write>(
    mut stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
) -> Result<(), TransportError> {
    // 1. GetQuote (cleartext, pre-handshake).
    let first = decode_request(&read_frame(&mut stream)?)?;
    let nonce = match first {
        EnclaveRequest::GetQuote { nonce } => nonce,
        _ => {
            return Err(TransportError::Handshake(
                "expected GetQuote before handshake".to_string(),
            ))
        }
    };
    write_frame(&mut stream, &encode_response(&keys.quote(nonce))?)?;

    // 2. Noise-IK responder handshake.
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
    let mut handshake = snow::Builder::new(params)
        .local_private_key(keys.noise_private())
        .build_responder()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    let mut buf = [0u8; 1024];
    let msg1 = read_frame(&mut stream)?;
    handshake
        .read_message(&msg1, &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    write_frame(&mut stream, &buf[..n])?;

    let mut noise = handshake
        .into_transport_mode()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    // Resident DKG ceremonies for this connection. A ceremony spans many
    // request/response round-trips on one connection (PoC: one connection per
    // enclave for the whole ceremony).
    let mut dkg = DkgSessionStore::new();
    // Seal the DKG-derived offer key + share once installed (Seam F). Tracked
    // per-connection so we attempt the write-once seal at most once here.
    let mut seal_attempted = false;

    // 3. Encrypted request/response loop. Exits when the peer closes (read EOF).
    while let Ok(frame) = read_frame(&mut stream) {
        let mut pt = vec![0u8; frame.len()];
        let n = noise
            .read_message(&frame, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let req = decode_request(&pt[..n])?;

        let resp = dispatch(
            req,
            keys,
            &mut dkg,
            offer_key,
            boot.map(|b| b.chain_id)
                .unwrap_or(alloy_primitives::B256::ZERO),
        );

        // Persist the offer key + share the first time it becomes available
        // (write-once).
        if !seal_attempted && offer_key.get().is_some() {
            seal_tribute_offer_and_group_sig_if_configured(boot, offer_key);
            seal_attempted = true;
        }

        let plain = encode_response(&resp)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut stream, &ct[..n])?;
    }
    Ok(())
}

/// Dispatch a post-handshake request to a response. `offer_key` is the shared
/// DKG-derived offer key slot: once Seam F populates it, the offer-decrypt path
/// and `GetPublicKeys` use it instead of the pre-DKG dev offer key.
pub fn dispatch(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
) -> EnclaveResponse {
    match req {
        EnclaveRequest::GetPublicKeys => EnclaveResponse::PublicKeys {
            // Advertise the DKG-derived offer key once available, so clients
            // encrypt to it; fall back to the dev offer key pre-DKG.
            recipient_x25519_pub: offer_key
                .get()
                .map(|k| k.public())
                .unwrap_or_else(|| keys.tribute_offer_public()),
            attestation_pub: keys.attestation_pub(),
            noise_static_pub: keys.noise_public(),
            tee_bls_pub: keys.tee_bls_public_bytes(),
            dkg_enc_pub: keys.dkg_enc_public(),
            dkg_enc_sig: keys.sign_dkg_enc_binding(chain_id),
        },
        EnclaveRequest::Initialize => EnclaveResponse::Initialized {
            sealed_loaded: false,
        },
        EnclaveRequest::ProcessTributeOfferBatch { offers } => {
            let derived = offer_key.get();
            let km = match derived {
                Some(d) => keys.tribute_offer_key_material_with(d.secret()),
                None => keys.tribute_offer_key_material(),
            };
            let (results, inputs_canonical_hash) = process_tribute_offer_batch(&km, &offers);
            // Sign (inputs_canonical_hash ‖ results) with the enclave's
            // Ed25519 attestation key. The host verifies this against the
            // attestation key it pinned from the quote, proving the results were
            // produced inside this attested enclave (not substituted by the host).
            let preimage = outbe_tee::protocol::tribute_offer_attestation_preimage(
                inputs_canonical_hash,
                &results,
            );
            let attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::TributeOfferBatch {
                results,
                inputs_canonical_hash,
                attestation_tag,
            }
        }
        EnclaveRequest::ApplyGratisOp { request } => {
            // Derive the resident Gratis state key from the same DKG group
            // signature as the offer key — identical on every enclave, so the
            // re-encrypted state is byte-identical (consensus determinism).
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "ApplyGratisOp: no resident group key (DKG not complete)".to_string(),
                };
            };
            let state_key =
                match crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0) {
                    Ok(k) => k,
                    Err(e) => {
                        return EnclaveResponse::Error {
                            message: e.to_string(),
                        }
                    }
                };
            let mut result = crate::gratis::apply_op(&state_key, &request);
            // Sign (inputs_canonical_hash ‖ result) with the attestation key so the
            // host can prove the result came from this attested enclave.
            let preimage = outbe_tee::protocol::gratis_op_attestation_preimage(
                result.inputs_canonical_hash,
                &result,
            );
            result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::GratisOpApplied {
                result: Box::new(result),
            }
        }
        EnclaveRequest::DeriveAccountKeys {
            account,
            requester_ephemeral_pubkey,
        } => {
            // OFF-CHAIN key delivery only (served over RPC, never during block
            // execution): derive the account's view + modify keys and seal them to
            // the requester's ephemeral X25519 key.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "DeriveAccountKeys: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            let sealed = (|| -> crate::errors::Result<crate::crypto::EncryptedShare> {
                let state_key =
                    crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0)?;
                let view_key = crate::gratis::derive_view_key(&state_key, account)?;
                let modify_key = crate::gratis::derive_modify_key(&state_key, account)?;
                let mut plaintext = view_key.to_vec();
                plaintext.extend_from_slice(&modify_key);
                crate::crypto::encrypt_share(&requester_ephemeral_pubkey, &plaintext)
            })();
            match sealed {
                Ok(blob) => EnclaveResponse::AccountKeysSealed {
                    account,
                    sealed: blob.ciphertext,
                    nonce: blob.nonce,
                    enclave_ephemeral_pubkey: blob.ephemeral_pub,
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::SealTributeOfferHandoff { recipient_x25519 } => {
            // Key-handoff SERVER: seal the resident group signature to the
            // newcomer's attested X25519 key. The host already verified the
            // newcomer's quote + committee membership. The plaintext group
            // signature never leaves the enclave — only the sealed blob is returned.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "SealTributeOfferHandoff: no resident offer key to hand off"
                        .to_string(),
                };
            };
            match crate::crypto::encrypt_share(&recipient_x25519, derived.group_sig()) {
                Ok(blob) => EnclaveResponse::SealedTributeOfferHandoff {
                    sealed: blob.to_bytes(),
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::SealOfferKeyForRegistry { recipient_x25519 } => {
            // On-chain key delivery SERVER: DETERMINISTICALLY
            // seal the resident group signature to `recipient_x25519` so the blob can
            // be committed on-chain. static-static ECDH from the resident offer secret
            // makes every committee enclave produce a byte-identical blob; the
            // plaintext group signature never leaves the enclave. The newcomer opens
            // it with `IngestTributeOfferHandoff`.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "SealOfferKeyForRegistry: no resident offer key to seal".to_string(),
                };
            };
            match crate::crypto::encrypt_share_deterministic(
                derived.secret(),
                &recipient_x25519,
                derived.group_sig(),
            ) {
                Ok(blob) => EnclaveResponse::SealedOfferKeyForRegistry {
                    sealed: blob.to_bytes(),
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::IngestTributeOfferHandoff {
            sealed,
            expected_tribute_offer_public,
            chain_id,
            tribute_offer_epoch,
        } => {
            // Key-handoff NEWCOMER: decrypt the handed-off group signature
            // with this enclave's X25519 share-decryption secret, derive the offer
            // key, and accept it ONLY if the derived public matches the on-chain
            // registered key — a malicious server cannot install a wrong key. On
            // success the group signature becomes resident (write-once); the serve
            // loop then seals it for restart.
            let derived = (|| -> crate::errors::Result<DerivedTributeOfferKey> {
                let blob = crate::crypto::EncryptedShare::from_bytes(&sealed)?;
                // Decrypt with the secret behind this enclave's advertised
                // `recipient_x25519` (its `tribute_offer` X25519 key) — the key the
                // server sealed the handoff to.
                let group_sig = Zeroizing::new(crate::crypto::decrypt_share(
                    &keys.tribute_offer_x25519_secret(),
                    &blob,
                )?);
                let (secret, public) = crate::crypto::derive_tribute_offer_secret_from_group_sig(
                    group_sig.as_ref(),
                    chain_id,
                    tribute_offer_epoch,
                )?;
                if public != expected_tribute_offer_public {
                    return Err(crate::errors::TeeError::Dkg(
                        "handoff rejected: derived offer key != on-chain registered key"
                            .to_string(),
                    ));
                }
                Ok(DerivedTributeOfferKey::from_parts(
                    secret,
                    public,
                    group_sig.to_vec(),
                ))
            })();
            match derived {
                Ok(d) => {
                    let public = d.public();
                    // Write-once: idempotent on the same key; a divergent key is a
                    // determinism fault (reject, keep the resident key).
                    if let Err(rejected) = offer_key.set(d) {
                        if offer_key.get().map(|k| k.public) != Some(rejected.public) {
                            return EnclaveResponse::Error {
                                message: "offer key divergence: handed-off key differs from \
                                          the resident offer key"
                                    .to_string(),
                            };
                        }
                    }
                    EnclaveResponse::TributeOfferHandoffIngested {
                        tribute_offer_public: public,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::DkgOpen {
            ceremony_id,
            round,
            participants,
        } => dispatch_dkg_open(keys, dkg, chain_id, ceremony_id, round, participants),
        EnclaveRequest::DkgStartDealer { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (pub_msg, sealed_shares) = s.start_dealer_encoded()?;
                Ok(EnclaveResponse::DkgDealt {
                    pub_msg,
                    sealed_shares,
                })
            }))
        }
        EnclaveRequest::DkgPlayerIngest {
            ceremony_id,
            dealer_bls,
            pub_msg,
            sealed_share,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            let ack = s.player_ingest_encoded(&dealer_bls, &pub_msg, &sealed_share)?;
            Ok(EnclaveResponse::DkgPlayerAck { ack })
        })),
        EnclaveRequest::DkgDealerReceiveAck {
            ceremony_id,
            player_bls,
            ack,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            s.dealer_receive_ack_encoded(&player_bls, &ack)?;
            Ok(EnclaveResponse::Ack)
        })),
        EnclaveRequest::DkgDealerFinalize { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                Ok(EnclaveResponse::DkgSignedLog {
                    signed_log: s.dealer_finalize_encoded()?,
                })
            }))
        }
        EnclaveRequest::DkgPlayerFinalize {
            ceremony_id,
            signed_logs,
        } => {
            // The session stays resident after finalize: Seam F (below) needs the
            // recovered share. It is released by `DkgRecoverTributeOffer`.
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (group_public, share_commitment) = s.player_finalize_encoded(&signed_logs)?;
                Ok(EnclaveResponse::DkgPlayerFinalized {
                    group_public,
                    share_commitment,
                })
            }))
        }
        EnclaveRequest::DkgTributeOfferPartial { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let sealed = s
                    .tribute_offer_partials_sealed()?
                    .into_iter()
                    .map(|(pk, blob)| {
                        (
                            commonware_codec::Encode::encode(&pk).to_vec(),
                            blob.to_bytes(),
                        )
                    })
                    .collect();
                Ok(EnclaveResponse::DkgTributeOfferPartial { sealed })
            }))
        }
        EnclaveRequest::DkgRecoverTributeOffer {
            ceremony_id,
            sealed_partials,
            chain_id,
            tribute_offer_epoch,
        } => {
            // Recover the offer secret AND retain the group threshold signature
            // (`group_sig`, Seam F): it seals for restart and is the key-handoff
            // payload that onboards a new committee member.
            let result = dkg.get_mut(&ceremony_id.0).and_then(|s| {
                // The group public KEY (constant term) is the public verification key
                // carried into the bootstrap payload for later reshare-endorsement
                // checks; capture it while the session is still resident.
                let group_public_key = s.group_public_key_bytes()?;
                let (secret, public, group_sig) = s.recover_tribute_offer_secret(
                    &sealed_partials,
                    chain_id,
                    tribute_offer_epoch,
                )?;
                Ok((secret, public, group_sig, group_public_key))
            });
            match result {
                Ok((secret, public, group_sig, group_public_key)) => {
                    let derived =
                        DerivedTributeOfferKey::from_parts(secret, public, group_sig.to_vec());
                    // Write-once: the first ceremony's key is canonical. A re-run
                    // that derives the SAME key is idempotent; a DIVERGENT key is a
                    // determinism fault — reject rather than keep a stale key.
                    if let Err(rejected) = offer_key.set(derived) {
                        if offer_key.get().map(|k| k.public) != Some(rejected.public) {
                            return EnclaveResponse::Error {
                                message: "offer key divergence: recovered key differs from \
                                          the resident offer key"
                                    .to_string(),
                            };
                        }
                    }
                    // Release the ceremony's resident secret state.
                    dkg.remove(&ceremony_id.0);
                    EnclaveResponse::DkgTributeOfferKey {
                        tribute_offer_public: public,
                        group_public_key,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::GetQuote { .. } => EnclaveResponse::Error {
            message: "GetQuote is only valid before the handshake".to_string(),
        },
        EnclaveRequest::SessionHandshake { .. } => EnclaveResponse::Error {
            message: "SessionHandshake is only valid during the handshake".to_string(),
        },
    }
}

fn dispatch_dkg_open(
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    chain_id: alloy_primitives::B256,
    ceremony_id: alloy_primitives::B256,
    round: u64,
    participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
) -> EnclaveResponse {
    let result = (|| {
        // The host relays each `(bls, enc, sig)` it gathered from peers' GetPublicKeys.
        // Before trusting any pairing: verify every enc key is signed by the BLS
        // identity it is paired with, and reject duplicate enc keys / identities — so
        // an untrusted host cannot mis-pair an enc key onto a foreign identity or
        // collapse two participants onto one enc key (cross-decryption of shares).
        let mut enc_by_bls = std::collections::BTreeMap::new();
        let mut seen_enc = std::collections::BTreeSet::new();
        let participant_bls: Vec<Vec<u8>> =
            participants.iter().map(|p| p.bls_pub.clone()).collect();
        for p in &participants {
            if !crate::keys::verify_dkg_enc_binding(&p.bls_pub, chain_id, &p.enc_pub, &p.enc_sig) {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: enc-key identity binding failed verification".to_string(),
                ));
            }
            if !seen_enc.insert(p.enc_pub) {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate enc key across participants".to_string(),
                ));
            }
            if enc_by_bls.insert(p.bls_pub.clone(), p.enc_pub).is_some() {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate BLS identity across participants".to_string(),
                ));
            }
        }
        let (info, pubkeys) = build_ceremony_info(round, &participant_bls)?;
        let mut recipient_enc_keys = std::collections::BTreeMap::new();
        for pk in &pubkeys {
            let bls_bytes = commonware_codec::Encode::encode(pk).to_vec();
            let enc = enc_by_bls.get(&bls_bytes).ok_or_else(|| {
                crate::errors::TeeError::Dkg("DkgOpen: enc key missing for participant".to_string())
            })?;
            recipient_enc_keys.insert(pk.clone(), *enc);
        }
        dkg.open(
            ceremony_id.0,
            info,
            keys.tee_bls_key().clone(),
            keys.dkg_enc_secret(),
            recipient_enc_keys,
        )?;
        Ok(EnclaveResponse::Ack)
    })();
    into_response(result)
}

/// Map a seam `Result` into an `EnclaveResponse`, turning errors into the typed
/// `Error` response the host surfaces (never a panic).
fn into_response(result: crate::errors::Result<EnclaveResponse>) -> EnclaveResponse {
    result.unwrap_or_else(|e| EnclaveResponse::Error {
        message: e.to_string(),
    })
}

/// Accept loop (used by the enclave binary). Each connection is served on its
/// own thread so multiple long-lived clients are handled concurrently — the node
/// keeps one connection open for offer decryption for its whole lifetime *and*
/// opens a second one for the startup TEE-bootstrap registration fetch; a
/// sequential loop would deadlock the second behind the first. `keys` is
/// read-only and shared via `Arc`; each connection still keeps its own
/// `DkgSessionStore`. A per-connection error is logged and never stops the
/// server.
pub fn serve(
    listener: &UnixListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
) -> Result<(), TransportError> {
    // The DKG-derived offer key is shared across all connection threads: the DKG
    // ceremony connection writes it (Seam F), the offer-decrypt connection reads
    // it. `main` may pre-seed it from a sealed blob (restart fast-path).
    for conn in listener.incoming() {
        let stream = conn?;
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        std::thread::spawn(move || {
            // PoC: surface to stderr; one bad client must not kill the enclave.
            if let Err(err) = serve_connection_with(stream, &keys, &offer_key, boot.as_deref()) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}

/// TCP accept loop — same thread-per-connection model as [`serve`], but over
/// TCP. Used when the enclave runs under Gramine, where pathname Unix domain
/// sockets are process-internal and a host process (the node) cannot reach them;
/// Gramine passes TCP through to the host network. The Noise-IK handshake still
/// authenticates + encrypts every byte, so TCP only changes the carrier, not the
/// confidentiality of the channel.
pub fn serve_tcp(
    listener: &TcpListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
) -> Result<(), TransportError> {
    for conn in listener.incoming() {
        let stream = conn?;
        // Low-latency request/response (the protocol is many small round-trips).
        let _ = stream.set_nodelay(true);
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        std::thread::spawn(move || {
            if let Err(err) = serve_connection_with(stream, &keys, &offer_key, boot.as_deref()) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    /// One in-process enclave: its key material, resident DKG store, and the
    /// shared DKG-derived offer key slot.
    struct Enclave {
        keys: EnclaveKeys,
        dkg: DkgSessionStore,
        offer_key: SharedTributeOfferKey,
        chain_id: B256,
    }

    impl Enclave {
        fn new(seed: u8) -> Self {
            Self {
                keys: EnclaveKeys::new([seed; 32], None).expect("keys"),
                dkg: DkgSessionStore::new(),
                offer_key: Arc::new(OnceLock::new()),
                chain_id: B256::repeat_byte(0xC1),
            }
        }

        fn call(&mut self, req: EnclaveRequest) -> EnclaveResponse {
            dispatch(
                req,
                &self.keys,
                &mut self.dkg,
                &self.offer_key,
                self.chain_id,
            )
        }

        fn identity(&mut self) -> outbe_tee::protocol::ParticipantAnnounce {
            match self.call(EnclaveRequest::GetPublicKeys) {
                EnclaveResponse::PublicKeys {
                    tee_bls_pub,
                    dkg_enc_pub,
                    dkg_enc_sig,
                    ..
                } => outbe_tee::protocol::ParticipantAnnounce {
                    bls_pub: tee_bls_pub,
                    enc_pub: dkg_enc_pub,
                    enc_sig: dkg_enc_sig,
                },
                other => panic!("unexpected GetPublicKeys response: {other:?}"),
            }
        }
    }

    // --- Fix A (C1) adversarial: DkgOpen must reject host tampering of the
    // (bls, enc, sig) bundle before trusting any pairing ----------------------

    fn honest_announces(n: usize) -> (Vec<Enclave>, Vec<outbe_tee::protocol::ParticipantAnnounce>) {
        let mut enclaves: Vec<Enclave> = (0..n).map(|i| Enclave::new(i as u8 + 1)).collect();
        let participants = enclaves.iter_mut().map(|e| e.identity()).collect();
        (enclaves, participants)
    }

    fn open_on(
        enclave: &mut Enclave,
        participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
    ) -> EnclaveResponse {
        enclave.call(EnclaveRequest::DkgOpen {
            ceremony_id: B256::repeat_byte(0x11),
            round: 0,
            participants,
        })
    }

    #[test]
    fn dkg_open_rejects_forged_enc_signature() {
        let (mut enclaves, mut participants) = honest_announces(4);
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
        let (mut enclaves, mut participants) = honest_announces(4);
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
        let (mut enclaves, mut participants) = honest_announces(4);
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
        let (mut enclaves, mut participants) = honest_announces(4);
        // A participant whose enc binding was signed under a DIFFERENT chain_id
        // than the verifier's must be rejected (cross-chain replay defense).
        let mut foreign = Enclave::new(9);
        foreign.chain_id = B256::repeat_byte(0xEE);
        participants[0] = foreign.identity();
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
        let ceremony_id = B256::repeat_byte(0x7c);

        let mut enclaves: Vec<Enclave> = (0..n).map(|i| Enclave::new(i as u8 + 1)).collect();

        // Announce identities, build the participant list (bls + enc + binding sig).
        let participants: Vec<outbe_tee::protocol::ParticipantAnnounce> =
            enclaves.iter_mut().map(|e| e.identity()).collect();
        let participant_bls: Vec<Vec<u8>> =
            participants.iter().map(|p| p.bls_pub.clone()).collect();

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
        // its partial to every recipient (n² sealed blobs). The host only ever
        // relays ciphertexts: `(recipient_bls, sealed_blob)`. Each enclave then
        // decrypts in-SGX the blobs addressed to it and recovers the group
        // signature → shared offer key.
        let mut all_sealed: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for e in enclaves.iter_mut() {
            match e.call(EnclaveRequest::DkgTributeOfferPartial { ceremony_id }) {
                EnclaveResponse::DkgTributeOfferPartial { sealed } => all_sealed.extend(sealed),
                other => panic!("DkgTributeOfferPartial: {other:?}"),
            }
        }

        let chain_id = B256::repeat_byte(0xc1);
        let mut tribute_offer_keys: Vec<[u8; 32]> = Vec::new();
        for (i, e) in enclaves.iter_mut().enumerate() {
            let sealed_partials: Vec<Vec<u8>> = all_sealed
                .iter()
                .filter(|(recipient_bls, _)| *recipient_bls == participant_bls[i])
                .map(|(_, blob)| blob.clone())
                .collect();
            match e.call(EnclaveRequest::DkgRecoverTributeOffer {
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
                        "group public key must be emitted at offer recovery"
                    );
                    tribute_offer_keys.push(tribute_offer_public);
                }
                other => panic!("DkgRecoverTributeOffer: {other:?}"),
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

        // The ceremony session is released after offer-key recovery.
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

    // ---- Seal / unseal offer secret + share (cfg(test) mock sealing key) ----

    fn install_tribute_offer_key(
        secret: [u8; 32],
        share: Vec<u8>,
    ) -> (SharedTributeOfferKey, [u8; 32]) {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let derived = DerivedTributeOfferKey::from_secret_and_group_sig(secret, share);
        let public = derived.public();
        offer_key.set(derived).ok().expect("set offer key");
        (offer_key, public)
    }

    /// Seal the DKG-derived offer key + group signature, then a fresh boot unseals
    /// and restores the byte-identical offer public key AND the group signature —
    /// the restart fast-path that skips the ceremony.
    #[test]
    fn ws_c_seal_then_unseal_restores_tribute_offer_key_and_group_sig() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0xCD; 32], dir.path().to_path_buf(), 2);
        let group_sig = vec![0x9b_u8; 96];
        let (offer_key, public) = install_tribute_offer_key([0x5a; 32], group_sig.clone());

        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);
        assert!(cfg.sealed_root_path().exists(), "sealed blob written");

        let restored = unseal_tribute_offer_and_group_sig_on_boot(&cfg).expect("unseal on boot");
        assert_eq!(restored.public(), public);
        assert_eq!(
            restored.group_sig(),
            group_sig.as_slice(),
            "group signature restored"
        );
    }

    /// vA seals at SVN 1; a vB enclave of the SAME signer (same mock MRSIGNER key,
    /// different build) boots at SVN 2 and unseals vA's blob — cross-version
    /// unseal with the anti-rollback floor satisfied.
    #[test]
    fn ws_c_cross_version_unseal_same_signer_key() {
        let dir = tempfile::tempdir().unwrap();
        let chain = [0xAB; 32];
        let cfg_a = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 1);
        let (offer_key, public) = install_tribute_offer_key([0x77; 32], vec![0x11; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg_a), &offer_key);

        let cfg_b = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 2);
        let restored =
            unseal_tribute_offer_and_group_sig_on_boot(&cfg_b).expect("vB unseals vA blob");
        assert_eq!(restored.public(), public);
    }

    /// A blob sealed for one chain does not unseal under a different `chain_id`
    /// (it is bound into the AEAD AAD).
    #[test]
    fn ws_c_unseal_rejects_wrong_chain_id() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x01; 32], dir.path().to_path_buf(), 1);
        let (offer_key, _public) = install_tribute_offer_key([0x33; 32], vec![0x22; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);

        let cfg_wrong = EnclaveBootConfig::new([0x02; 32], dir.path().to_path_buf(), 1);
        assert!(unseal_tribute_offer_and_group_sig_on_boot(&cfg_wrong).is_none());
    }

    /// Sealing is write-once: a second call does not overwrite the blob (so a
    /// later re-derived key cannot silently replace the persisted one).
    #[test]
    fn ws_c_seal_is_write_once() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x09; 32], dir.path().to_path_buf(), 1);
        let (offer_key, public) = install_tribute_offer_key([0x44; 32], vec![0x33; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);
        let first = std::fs::read(cfg.sealed_root_path()).unwrap();

        // A different resident key must not overwrite the existing blob.
        let (offer_key2, _other) = install_tribute_offer_key([0x55; 32], vec![0x44; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key2);
        let second = std::fs::read(cfg.sealed_root_path()).unwrap();
        assert_eq!(first, second, "seal is write-once");
        // The persisted key is still the first one.
        assert_eq!(
            unseal_tribute_offer_and_group_sig_on_boot(&cfg)
                .unwrap()
                .public(),
            public
        );
    }

    /// The encoded share is only present inside the AEAD ciphertext — it never
    /// appears as plaintext bytes in the on-disk blob (secret-at-rest invariant).
    #[test]
    fn ws_c_share_never_in_host_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x07; 32], dir.path().to_path_buf(), 1);
        let share = vec![0xC3_u8; 40];
        let (offer_key, _public) = install_tribute_offer_key([0x66; 32], share.clone());
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);

        let blob = std::fs::read(cfg.sealed_root_path()).unwrap();
        assert!(
            !blob.windows(share.len()).any(|w| w == share.as_slice()),
            "raw share bytes must not appear in the sealed blob"
        );
    }

    // ---- Tribute offer key-handoff ----

    /// A server enclave with a resident group signature seals it to a newcomer's
    /// X25519 key; the newcomer ingests it, verifies it against the on-chain
    /// expected offer public, and ends up with the byte-identical resident key.
    #[test]
    fn handoff_seals_and_ingests_offer_key() {
        let chain_id = B256::repeat_byte(0xC1);
        let epoch = 0u64;
        // Any bytes act as the group signature for the HKDF derivation.
        let group_sig = vec![0x9b_u8; 96];
        let (secret, public) =
            crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, chain_id, epoch)
                .expect("derive offer public");

        // Server: install the resident group signature into its offer-key slot.
        let mut server = Enclave::new(1);
        server
            .offer_key
            .set(DerivedTributeOfferKey::from_parts(
                secret, public, group_sig,
            ))
            .ok()
            .expect("install server offer key");

        // Newcomer: fresh enclave, no offer key; its X25519 recipient key is the
        // handoff target.
        let mut newcomer = Enclave::new(2);
        let newcomer_enc = newcomer.keys.tribute_offer_public();

        let sealed = match server.call(EnclaveRequest::SealTributeOfferHandoff {
            recipient_x25519: newcomer_enc,
        }) {
            EnclaveResponse::SealedTributeOfferHandoff { sealed } => sealed,
            other => panic!("expected SealedTributeOfferHandoff, got {other:?}"),
        };

        match newcomer.call(EnclaveRequest::IngestTributeOfferHandoff {
            sealed,
            expected_tribute_offer_public: public,
            chain_id,
            tribute_offer_epoch: epoch,
        }) {
            EnclaveResponse::TributeOfferHandoffIngested {
                tribute_offer_public,
            } => assert_eq!(tribute_offer_public, public),
            other => panic!("expected TributeOfferHandoffIngested, got {other:?}"),
        }
        assert_eq!(newcomer.offer_key.get().map(|k| k.public()), Some(public));
    }

    /// The newcomer rejects a handoff whose derived offer key does not match the
    /// on-chain expected public — a malicious server cannot install a wrong key.
    #[test]
    fn handoff_ingest_rejects_wrong_expected_public() {
        let chain_id = B256::repeat_byte(0xC2);
        let epoch = 0u64;
        let group_sig = vec![0x44_u8; 96];
        let (secret, public) =
            crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, chain_id, epoch)
                .expect("derive");

        let mut server = Enclave::new(3);
        server
            .offer_key
            .set(DerivedTributeOfferKey::from_parts(
                secret, public, group_sig,
            ))
            .ok()
            .expect("install");

        let mut newcomer = Enclave::new(4);
        let newcomer_enc = newcomer.keys.tribute_offer_public();
        let sealed = match server.call(EnclaveRequest::SealTributeOfferHandoff {
            recipient_x25519: newcomer_enc,
        }) {
            EnclaveResponse::SealedTributeOfferHandoff { sealed } => sealed,
            other => panic!("got {other:?}"),
        };

        // Claim a different expected public than the one the group signature yields.
        match newcomer.call(EnclaveRequest::IngestTributeOfferHandoff {
            sealed,
            expected_tribute_offer_public: [0xEE_u8; 32],
            chain_id,
            tribute_offer_epoch: epoch,
        }) {
            EnclaveResponse::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
        // The newcomer must NOT have installed any key.
        assert!(newcomer.offer_key.get().is_none());
    }
}
