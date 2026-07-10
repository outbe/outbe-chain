//! Neutral wire-protocol types for the node <-> enclave channel.
//!
//! These types are the message contract shared by the host (`outbe-tee`) and
//! the enclave (`outbe-tee-enclave`). They carry **no secret material** and no
//! cryptographic logic — only the shape of requests and responses.
//!
//! Transport (later slice): length-prefixed framing over UDS, wrapped in a
//! Noise-IK transport (payload layer). `GetQuote` is callable before the Noise
//! handshake; every other command is only valid inside an established session.
//!
//! Opaque byte fields (`Vec<u8>`) intentionally hide DKG wire internals: the
//! host parses only the public envelope and forwards the encrypted
//! secret-bearing parts to the enclave without decrypting them.

use alloy_primitives::{Address, B256, U256};

/// A single offer handed to the enclave.
///
/// Fields mirror the part of `ITributeFactory.offerTribute` the enclave needs,
/// plus the oracle price and the sender:
///   - `cipherText`, `nonce`, `ephemeralPubkey`, `referenceCurrency`,
///     `excludeFromIntexIssuance` (ABI);
///   - `owner` — the L1 `msg.sender`; the enclave binds it into the result and
///     into the `token_id` (computed in-enclave, see `TributeOfferResult`);
///   - `tribute_price_minor` — the coen/usdt oracle price, resolved by the node
///     from committed Oracle state and passed in (not an ABI field).
///
/// The ZK fields (`zkProof`/`zkVerificationKey`/`zkPublicKey`/`zkMerkleRoot`)
/// are verified BEFORE the enclave call and are NOT forwarded. `worldwide_day`
/// and `currency` are NOT wire inputs — they live in the encrypted payload and
/// the enclave reads them from there. The node reads the current USDC/COEN oracle
/// rate at this block and passes only the resolved `tribute_price_minor`.
///
/// Price integrity: the enclave applies the rate but does not verify it against
/// chain state; integrity is enforced by deterministic re-execution (a forged
/// rate yields a state-root mismatch). See plan §"Oracle Price Determinism".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EncryptedTributeOffer {
    /// L1 `msg.sender` that owns the resulting Tribute (public, on-chain).
    pub owner: Address,
    /// ABI `cipherText`: AEAD ciphertext of the offer payload.
    pub cipher_text: Vec<u8>,
    /// ABI `nonce`: 12-byte ChaCha20Poly1305 nonce.
    pub nonce: Vec<u8>,
    /// ABI `ephemeralPubkey` (uint256): client ephemeral X25519 public key for
    /// ECDHE, big-endian.
    pub ephemeral_pubkey: U256,
    /// ABI `referenceCurrency`.
    pub reference_currency: u16,
    /// ABI `excludeFromIntexIssuance`: when true, the resulting Tribute is
    /// excluded from Intex issuance. Unencrypted (public), like
    /// `reference_currency` — the enclave echoes it back in the result.
    pub exclude_from_intex_issuance: bool,
    /// Current USDC/COEN oracle rate (at this block) the enclave applies.
    pub tribute_price_minor: U256,
}

/// Status of a single offer after enclave processing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TributeOfferStatus {
    Created,
    Rejected { reason: String },
}

/// Public result for a single offer (Enclave Return Rule: no L2 draft owner, no
/// L2 pubkey, no raw proof witness).
///
/// `token_id` is computed **inside the enclave** via Poseidon over sensitive
/// decrypted data (it cannot be derived on the host, which never sees that
/// data). `owner` is the L1 `msg.sender`, bound by the enclave. The remaining
/// fields are the economics derived from the decrypted payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeOfferResult {
    /// Poseidon(token_id preimage) — computed in-enclave from sensitive data.
    pub token_id: B256,
    /// L1 `msg.sender` (public, on-chain).
    pub owner: Address,
    pub worldwide_day: u32,
    pub issuance_amount_minor: U256,
    pub issuance_currency: u16,
    pub nominal_amount_minor: U256,
    pub reference_currency: u16,
    /// Echoed from the offer's unencrypted `excludeFromIntexIssuance` ABI flag
    /// (see `EncryptedTributeOffer`); the host stores it on the Tribute.
    pub exclude_from_intex_issuance: bool,
    pub tribute_price_minor: U256,
    /// SU hashes (hex) — the host marks them used (replay prevention). Public
    /// on-chain as used-markers. The privacy-preserving markers-only form (rather
    /// than raw hashes) is a later slice (see `process.rs`).
    pub su_hashes: Vec<String>,
    /// WAA wallet addresses — host routes agent rewards. Public on-chain.
    pub wallet_addresses: Vec<String>,
    /// SRA addresses — host routes agent rewards. Public on-chain.
    pub sra_addresses: Vec<String>,
    pub status: TributeOfferStatus,
}

/// Requests sent from the node to the enclave.
///
/// DKG secret-seam variants carry opaque bytes: the host never sees plaintext
/// shares.
/// One DKG participant's announced identity, structurally bound so the untrusted
/// host cannot mis-pair a BLS identity with a different X25519 enc key or collapse
/// two participants onto one enc key. `enc_sig` is the owner's TEE-BLS signature
/// over the `(chain_id, enc_pub)` binding, verified at `DkgOpen` before the enc key
/// is trusted as that identity's share recipient.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParticipantAnnounce {
    /// Encoded TEE-BLS public key (the participant's DKG identity).
    pub bls_pub: Vec<u8>,
    /// Announced X25519 share-encryption public key.
    pub enc_pub: [u8; 32],
    /// TEE-BLS signature over the `(chain_id, enc_pub)` binding.
    pub enc_sig: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveRequest {
    /// Callable BEFORE the Noise handshake (unauthenticated). `nonce` provides
    /// freshness against quote replay.
    GetQuote { nonce: [u8; 32] },
    /// Noise-IK handshake message.
    SessionHandshake { noise_msg: Vec<u8> },
    /// Return the enclave's public keys (recipient X25519, attestation, Noise
    /// static, tribute-BLS).
    GetPublicKeys,
    /// Load the sealed root seed from disk, or start fresh.
    Initialize,

    /// Open a TEE DKG ceremony session inside the enclave. Each `participants[i]`
    /// bundles a BLS identity, its announced X25519 share-encryption key, and the
    /// owner's signature binding the two — so the untrusted host cannot mis-pair or
    /// duplicate enc keys. The enclave verifies every binding, rejects duplicate
    /// enc keys, then builds the ceremony `Info` from the BLS set and captures the
    /// enc keys so dealings can be sealed to recipients. The host only relays values
    /// it obtained from each participant's `PublicKeys`.
    DkgOpen {
        ceremony_id: B256,
        round: u64,
        participants: Vec<ParticipantAnnounce>,
    },
    /// Seam A: deal + seal per-player shares. Returns the public commitment and
    /// one opaque sealed share per participant.
    DkgStartDealer { ceremony_id: B256 },
    /// Seam B: open + verify an incoming sealed dealing inside the enclave. The
    /// host relays the opaque `sealed_share` without decrypting it.
    DkgPlayerIngest {
        ceremony_id: B256,
        dealer_bls: Vec<u8>,
        pub_msg: Vec<u8>,
        sealed_share: Vec<u8>,
    },
    /// Seam C: record a player's acknowledgement at this enclave's dealer.
    DkgDealerReceiveAck {
        ceremony_id: B256,
        player_bls: Vec<u8>,
        ack: Vec<u8>,
    },
    /// Seam D: finalize this enclave's dealing into a signed dealer log.
    DkgDealerFinalize { ceremony_id: B256 },
    /// Seam E: verify the collected signed dealer logs and recover this enclave's
    /// local threshold share (committed inside the enclave). Returns the public
    /// group key and the share commitment.
    DkgPlayerFinalize {
        ceremony_id: B256,
        signed_logs: Vec<Vec<u8>>,
    },
    /// Seam F (offer key): threshold-sign the fixed offer message with this
    /// enclave's recovered share, then **seal the partial to every recipient
    /// enclave's X25519 key** (one ciphertext per participant). The host relays
    /// only the opaque ciphertexts — it never sees a plaintext partial, so it
    /// cannot recover the group signature (and hence the offer key) itself.
    /// Requires `DkgPlayerFinalize` first.
    DkgTributeOfferPartial { ceremony_id: B256 },
    /// Seam F (offer key): recover the group threshold signature from the sealed
    /// partials addressed to THIS enclave (decrypted in-SGX) and derive the shared
    /// offer X25519 keypair from it (`HKDF` bound to `chain_id` + `tribute_offer_epoch`).
    /// The offer secret is stored resident in the enclave; only the public key is
    /// returned. Releases the ceremony session.
    DkgRecoverTributeOffer {
        ceremony_id: B256,
        /// Sealed partials addressed to this enclave (one `EncryptedShare` blob per
        /// signer); decrypted with the enclave's X25519 share-decryption secret.
        sealed_partials: Vec<Vec<u8>>,
        chain_id: B256,
        tribute_offer_epoch: u64,
    },

    /// Decrypt a batch of offers, apply the oracle price, and return the
    /// canonical Tribute results. Each `EncryptedTributeOffer` is self-contained (its
    /// own `owner`, `reference_currency`, cleartext `worldwide_day`/currency, and
    /// oracle price), so the batch is simply a list. A single transaction carries
    /// one offer today; the list future-proofs multi-offer txs. This is the sole
    /// offer-processing entrypoint (the enclave decrypts, applies the price,
    /// computes economics + Poseidon `token_id`, and returns `TributeOfferResult`).
    ProcessTributeOfferBatch { offers: Vec<EncryptedTributeOffer> },

    /// Key-handoff, SERVER side: seal this enclave's resident group threshold
    /// signature to a newcomer's attested X25519 key. The host has already verified
    /// the newcomer's quote (so `recipient_x25519` is attested) and that the
    /// requester is in the active committee. Returns `SealedTributeOfferHandoff`
    /// (an opaque `EncryptedShare` blob the host relays); the enclave only seals to
    /// the supplied key — it never exports the group signature in plaintext.
    SealTributeOfferHandoff { recipient_x25519: [u8; 32] },

    /// Key-handoff, NEWCOMER side: ingest a sealed group signature received
    /// from a current committee member. The enclave decrypts it with its X25519
    /// share-decryption secret, derives the offer keypair, and accepts it ONLY if the
    /// derived public matches `expected_tribute_offer_public` (the on-chain registered
    /// key) — so a malicious server cannot install a wrong key. On success the group
    /// signature becomes resident (and seals for restart); returns
    /// `TributeOfferHandoffIngested`.
    IngestTributeOfferHandoff {
        /// Opaque `EncryptedShare` blob (the sealed group signature).
        sealed: Vec<u8>,
        /// The on-chain registered offer public key to verify the handoff against.
        expected_tribute_offer_public: [u8; 32],
        chain_id: B256,
        tribute_offer_epoch: u64,
    },

    /// On-chain key delivery, SERVER side: DETERMINISTICALLY
    /// seal this enclave's resident group signature to `recipient_x25519` so the
    /// sealed blob can be COMMITTED ON-CHAIN. Unlike `SealTributeOfferHandoff` (a
    /// per-reply P2P seal with a random ephemeral key + nonce), every committee
    /// enclave returns a BYTE-IDENTICAL blob for the same recipient — the prerequisite
    /// for storing it in `TeeRegistry` as a consensus-validated artifact. Returns
    /// `SealedOfferKeyForRegistry`. The newcomer opens it with `IngestTributeOfferHandoff`.
    SealOfferKeyForRegistry { recipient_x25519: [u8; 32] },
}

/// Deterministic hash over the canonical batch inputs — each offer's
/// owner/cipher_text/nonce/ephemeral/reference-currency/exclude-from-intex/price.
/// Length-prefixed to be unambiguous.
///
/// SHARED by the enclave (which returns it in `TributeOfferBatch`) and the host (which
/// recomputes it from the request it sent and compares — a mismatch is enclave
/// non-determinism). Defining it once here keeps the two byte layouts from
/// drifting. Diagnostic only — never written to chain state.
pub fn inputs_canonical_hash(offers: &[EncryptedTributeOffer]) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&(offers.len() as u32).to_be_bytes());
    for offer in offers {
        buf.extend_from_slice(offer.owner.as_slice());
        buf.extend_from_slice(&(offer.cipher_text.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.cipher_text);
        buf.extend_from_slice(&(offer.nonce.len() as u32).to_be_bytes());
        buf.extend_from_slice(&offer.nonce);
        buf.extend_from_slice(&offer.ephemeral_pubkey.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_currency.to_be_bytes());
        buf.push(u8::from(offer.exclude_from_intex_issuance));
        buf.extend_from_slice(&offer.tribute_price_minor.to_be_bytes::<32>());
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage the enclave signs (with its Ed25519 attestation key)
/// and the host verifies — it binds the canonical inputs hash to the produced
/// results, so the host can prove the results were computed inside the attested
/// enclave (not substituted by the host). SHARED so the two byte layouts cannot
/// drift: `serde_json` of a fixed-field struct list is deterministic (struct
/// field order is declaration order; there are no maps or floats). Local-only —
/// never written to chain state.
pub fn tribute_offer_attestation_preimage(
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
) -> Vec<u8> {
    let results_json = serde_json::to_vec(results).unwrap_or_default();
    let mut buf = Vec::with_capacity(30 + 32 + 4 + results_json.len());
    buf.extend_from_slice(b"outbe/tee/offer-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(results_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&results_json);
    buf
}

/// Responses returned from the enclave to the node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveResponse {
    /// SGX quote bundle. Carries the enclave public keys in cleartext plus the
    /// `report_data` that binds them: the host recomputes
    /// `keccak256(noise_static_pub || recipient_x25519_pub || attestation_pub)`
    /// and checks it equals `report_data`, proving the cleartext keys are the
    /// attested ones. `noise_static_pub` is then used as the Noise-IK remote
    /// static key. Callable before the handshake (unauthenticated).
    Quote {
        mrenclave: B256,
        mrsigner: B256,
        isv_svn: u16,
        report_data: B256,
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        quote_body: Vec<u8>,
        /// Human-readable attestation environment the enclave detected (e.g.
        /// `dcap (gramine-sgx)` or `none (gramine-direct / no SGX)`), so the host
        /// can log the exact mode instead of guessing direct-vs-bare.
        attestation: String,
    },
    Handshake {
        noise_msg: Vec<u8>,
    },
    PublicKeys {
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        /// TEE threshold-BLS public key (the enclave's DKG participant identity).
        tee_bls_pub: Vec<u8>,
        /// X25519 share-encryption public key; dealers seal DKG shares to it.
        dkg_enc_pub: [u8; 32],
        /// TEE-BLS signature over the `(chain_id, dkg_enc_pub)` binding, proving
        /// this enc key belongs to `tee_bls_pub`. Relayed by the host into peers'
        /// `DkgOpen` and verified there before the enc key is trusted.
        dkg_enc_sig: Vec<u8>,
    },
    Initialized {
        sealed_loaded: bool,
    },
    /// Generic acknowledgement (e.g. `DkgOpen` / `DkgDealerReceiveAck`).
    Ack,
    /// Seam A result: public commitment + one opaque sealed share per recipient
    /// `(recipient_bls, sealed_share)`.
    DkgDealt {
        pub_msg: Vec<u8>,
        sealed_shares: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam B result: the player's acknowledgement bytes, or `None` if the dealing
    /// did not verify.
    DkgPlayerAck {
        ack: Option<Vec<u8>>,
    },
    /// Seam D result: this enclave's signed dealer log.
    DkgSignedLog {
        signed_log: Vec<u8>,
    },
    /// Seam E result: the public group key and this enclave's share commitment.
    DkgPlayerFinalized {
        group_public: Vec<u8>,
        share_commitment: B256,
    },
    /// Seam F result: this enclave's partial signature over the offer message,
    /// **sealed to each recipient enclave** — one opaque ciphertext per
    /// participant `(recipient_bls, sealed_partial)`. The host relays the
    /// ciphertexts but cannot decrypt them, so it cannot recover the group
    /// signature / offer key.
    DkgTributeOfferPartial {
        sealed: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// Seam F result: the shared offer public key derived from the recovered
    /// group signature (the secret stays resident in the enclave).
    DkgTributeOfferKey {
        tribute_offer_public: [u8; 32],
        /// The committee's DKG group public KEY (constant term) — the public
        /// verification key for this committee's threshold group signatures.
        /// Carried into the bootstrap payload so a later reshare endorsement can be
        /// verified on-chain against this committee's key.
        group_public_key: Vec<u8>,
    },
    /// Key-handoff SERVER result: the resident group signature sealed to the
    /// newcomer's X25519 key (an opaque `EncryptedShare` blob the host relays).
    SealedTributeOfferHandoff {
        sealed: Vec<u8>,
    },
    /// On-chain key delivery SERVER result: the resident group signature
    /// DETERMINISTICALLY sealed to `recipient_x25519` — byte-identical across all
    /// committee enclaves, for committing to `TeeRegistry`.
    SealedOfferKeyForRegistry {
        sealed: Vec<u8>,
    },
    /// Key-handoff NEWCOMER result: the offer public derived from the
    /// ingested group signature (it matched the on-chain expected key).
    TributeOfferHandoffIngested {
        tribute_offer_public: [u8; 32],
    },
    TributeOfferBatch {
        results: Vec<TributeOfferResult>,
        /// Diagnostic hash of canonical inputs (incl. price/day/currency);
        /// host compares it to detect enclave non-determinism, then discards.
        inputs_canonical_hash: B256,
        /// Local-only attestation tag; host verifies against its enclave's
        /// attestation key, then discards. Never written to state.
        attestation_tag: Vec<u8>,
    },
    Error {
        message: String,
    },
}
