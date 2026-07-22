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

/// A Gratis write operation the enclave applies over encrypted per-account state.
///
/// The op determines the sign of the aggregate deltas the host applies to the
/// public `total_supply` / `pledged_total_supply` scalars, and which ciphertext
/// slots move (balance vs pledged vs pledge-lock-ticket).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOp {
    /// Mint `amount` to `account` (credit balance; `total_supply += amount`).
    Mint,
    /// Burn `amount` from `account` (debit balance; `total_supply -= amount`).
    Burn,
    /// Lock `amount` of `account`'s balance into a new `PledgeLockTicket` pending a
    /// credis request (debit balance; `pledged_total_supply += amount`). The amount
    /// is parked in the ticket, NOT yet credited to the account's pledged ledger.
    Pledge,
    /// Return a still-pending pledge (e.g. credis rejected): read the ticket, credit
    /// `amount` back to `account`'s balance, and delete the ticket
    /// (`pledged_total_supply -= amount`).
    Unpledge,
    /// Consume a `PledgeLockTicket` for a credis request: verify `spend_auth` binds
    /// it to `bundle_account`, credit the ticket `amount` into the EOA's own pledged
    /// ledger, and delete the ticket (no aggregate change — it stays pledged). Returns
    /// `gratis_amount` so credis can size the position.
    ConsumePledge,
    /// Release `amount` of collateral from the EOA's own pledged ledger back to its
    /// balance (`pledged_total_supply -= amount`). Amount-based (no ticket); the
    /// on-chain Credis position schedule is the accounting authority.
    ReleaseToEoa,
    /// Burn `amount` of collateral from the EOA's own pledged ledger at credis expiry
    /// (`total_supply -= amount`; `pledged_total_supply -= amount`). Amount-based (no
    /// ticket); the on-chain Credis position's outstanding balance is the authority.
    BurnPledged,
    /// Read-only: decrypt a state-key-sealed owner blob and return the plaintext EOA.
    /// With `pledge_handle = Some(handle)` the blob in `current_pledge_record` is a live
    /// `PledgeLockTicket` (used at credis `ConsumePledge` time, before the calldata carries
    /// no EOA); with `None` it is the self-contained `eoa_ct` stored on the Credis position
    /// (used at `payAnadosis`/expiry to recover the EOA that keys the pledged ledger).
    /// No state mutation, no authorization.
    RevealOwner,
}

/// Proof that the caller holds the account's modify key, without revealing it.
///
/// `mac = HMAC-SHA256(modify_key, "outbe/gratis/modify/v1" ‖ account ‖ op_tag ‖
/// amount ‖ op_nonce ‖ chain_id)`, recomputed inside the enclave (which
/// re-derives `modify_key` from the resident state key + account). `op_nonce` is
/// the account's monotonic on-chain replay counter, so a captured tuple cannot be
/// replayed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModifyAuth {
    pub mac: [u8; 32],
    pub op_nonce: u64,
}

/// Inputs for a single `ApplyGratisOp`. The host reads the current ciphertext
/// blobs + versions from committed storage and forwards them verbatim; the
/// enclave decrypts, enforces invariants, and re-encrypts deterministically.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpRequest {
    pub op: GratisOp,
    pub chain_id: B256,
    /// Balance/pledged-owning account (the EOA). For `ConsumePledge`/`ReleaseToEoa`/
    /// `BurnPledged` the EOA never appears in calldata or stored plaintext: the host first
    /// recovers it with a `RevealOwner` round-trip (decrypting the pledge ticket, or the
    /// `eoa_ct` stored on the Credis position) and passes the revealed address here. For
    /// `ConsumePledge` the enclave still cross-checks it against `ticket.owner`. Ignored for
    /// `RevealOwner` itself.
    pub account: Address,
    // TODO(privacy): `amount` is a plaintext write input, so per-tx amounts are
    // visible in calldata (only cumulative balances are encrypted). To also hide
    // amounts, carry a client-encrypted amount blob here (like `EncryptedTributeOffer`)
    // and decrypt it inside the enclave — heavier ABI + a client encrypt step.
    pub amount: U256,
    /// Current balance blob (`version(8 BE) ‖ ciphertext`), self-versioning so no
    /// separate version slot is needed. Empty when the account has no state yet.
    pub current_balance: Vec<u8>,
    /// Current pledged-ledger blob (same `version ‖ ct` shape). Empty if none.
    pub current_pledged: Vec<u8>,
    /// Existing pledge-lock-ticket blob (`version ‖ ct`); empty for `Pledge`. Set for
    /// `Unpledge`/`ConsumePledge`.
    pub current_pledge_record: Vec<u8>,
    /// Modify-key authorization (required for Mint/Burn/Pledge/Unpledge; ignored for
    /// the credis-driven `ConsumePledge`/`ReleaseToEoa`/`BurnPledged`).
    pub modify_auth: ModifyAuth,
    /// Pledge handle identifying the ticket (set for `Unpledge`/`ConsumePledge`).
    pub pledge_handle: Option<B256>,
    /// Destination bundle account (set for `ConsumePledge`).
    pub bundle_account: Option<Address>,
    /// Spend authorization binding the pledge to `bundle_account`
    /// (`spend_auth_mac(pledge_secret, bundle_account)`), set for `ConsumePledge`.
    pub spend_auth: Option<[u8; 32]>,
}

/// Outcome of a single Gratis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOpStatus {
    Applied,
    Rejected { reason: String },
}

/// Public result of an `ApplyGratisOp`: the new ciphertext blobs to store verbatim
/// plus the plaintext receipt the host needs (aggregate deltas, event amount,
/// pledge linkage). Per-account plaintext balances never appear here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpResult {
    pub status: GratisOpStatus,
    /// New balance blob (`version ‖ ct`) to store verbatim.
    pub new_balance: Vec<u8>,
    /// New pledged-ledger blob (`version ‖ ct`) to store verbatim.
    pub new_pledged: Vec<u8>,
    /// New pledge-lock-ticket blob (`version ‖ ct`) for `Pledge`; empty on
    /// `Unpledge`/`ConsumePledge` (which the host writes back to clear/delete the
    /// ticket slot). Empty and untouched for all other ops.
    pub new_pledge_record: Vec<u8>,
    /// Deterministic pledge handle for a `Pledge` (zero otherwise).
    pub pledge_handle: B256,
    /// Pledged amount surfaced for credis (`ConsumePledge`); zero otherwise.
    pub gratis_amount: U256,
    /// Plaintext EOA recovered by a `RevealOwner` op (zero otherwise). Lets the host key the
    /// per-account pledged/balance ledgers without the EOA ever appearing in calldata or state.
    pub revealed_owner: Address,
    /// Self-contained sealed EOA blob (`nonce(12) ‖ ChaCha20Poly1305(owner 20B)` under the
    /// state key) produced by `ConsumePledge` for the host to store on the Credis position;
    /// empty for every other op. Later decrypted via `RevealOwner` (`pledge_handle = None`).
    pub eoa_ct: Vec<u8>,
    /// Amount for the emitted event (mint/burn/pledge/unpledge magnitude).
    pub event_amount: U256,
    /// The account's next modify-auth nonce (for the host to persist).
    pub next_op_nonce: u64,
    /// Diagnostic hash of the canonical request inputs; the host recomputes it to
    /// detect enclave non-determinism, then discards.
    pub inputs_canonical_hash: B256,
    /// Local-only attestation tag over `(inputs_canonical_hash ‖ result)`; the
    /// host verifies it against the pinned enclave attestation key, then discards.
    pub attestation_tag: Vec<u8>,
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

    /// Apply a Gratis write op over encrypted per-account state. The enclave
    /// derives the resident `gratis_state_key` from the same group signature as
    /// the offer key, decrypts the supplied blobs, enforces balance invariants +
    /// modify-key authorization, and re-encrypts deterministically. This is a
    /// consensus path (called inside precompile `dispatch`, re-executed by every
    /// validator).
    ApplyGratisOp { request: Box<GratisOpRequest> },

    /// Off-chain key delivery: derive `account`'s view + modify keys from the
    /// resident state key and seal them to the requester's ephemeral X25519 key.
    /// NOT a consensus path — served only over RPC, never during block execution.
    DeriveAccountKeys {
        account: Address,
        requester_ephemeral_pubkey: [u8; 32],
    },
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
    /// Result of an `ApplyGratisOp`: new ciphertexts + plaintext receipt.
    GratisOpApplied {
        result: Box<GratisOpResult>,
    },
    /// Result of `DeriveAccountKeys`: `AEAD(ECDHE(enclave, requester_ephemeral),
    /// view_key ‖ modify_key)` sealed to the requester. Opaque to the host.
    AccountKeysSealed {
        account: Address,
        sealed: Vec<u8>,
        nonce: [u8; 12],
        enclave_ephemeral_pubkey: [u8; 32],
    },
    Error {
        message: String,
    },
}

/// Deterministic hash over the canonical inputs of a single Gratis op. SHARED by
/// the enclave (returned in `GratisOpResult`) and the host (recomputed from the
/// request it sent and compared — a mismatch is enclave non-determinism).
/// Length-prefixed to be unambiguous. Diagnostic only — never written to state.
pub fn gratis_op_canonical_hash(req: &GratisOpRequest) -> B256 {
    fn push_bytes(buf: &mut Vec<u8>, b: &[u8]) {
        buf.extend_from_slice(&(b.len() as u32).to_be_bytes());
        buf.extend_from_slice(b);
    }
    let mut buf: Vec<u8> = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    push_bytes(&mut buf, &req.current_balance);
    push_bytes(&mut buf, &req.current_pledged);
    push_bytes(&mut buf, &req.current_pledge_record);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    // Optional linkage fields: length/flag-prefixed so presence is unambiguous.
    match req.pledge_handle {
        Some(h) => {
            buf.push(1);
            buf.extend_from_slice(h.as_slice());
        }
        None => buf.push(0),
    }
    match req.bundle_account {
        Some(a) => {
            buf.push(1);
            buf.extend_from_slice(a.as_slice());
        }
        None => buf.push(0),
    }
    match req.spend_auth {
        Some(s) => {
            buf.push(1);
            buf.extend_from_slice(&s);
        }
        None => buf.push(0),
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage the enclave signs (Ed25519 attestation key) and the
/// host verifies, binding the canonical inputs hash to the produced result so the
/// host can prove the result came from the attested enclave. SHARED so the byte
/// layouts cannot drift. Local-only — never written to chain state.
pub fn gratis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
) -> Vec<u8> {
    // Hash the ciphertext-bearing result fields deterministically. serde_json of a
    // fixed-field struct is deterministic (declaration order, no maps/floats); we
    // exclude the tag itself to avoid self-reference.
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe).unwrap_or_default();
    let mut buf = Vec::with_capacity(31 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/gratis-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    buf
}
