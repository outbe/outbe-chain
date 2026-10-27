use alloy_primitives::{Address, B256, U256};

// The preserved doc links name `EnclaveRequest`. Rustc does not treat those links as uses.
#[allow(unused_imports)]
use super::enclave_request::EnclaveRequest;
use super::{
    EncryptedTributeOffer, FidelityCohortRequest, FidelityCohortResult, FidelityLeagueEntry,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotRequest, GratisOpRequest,
    GratisOpResult, Ledger, ParticipantAnnounce, PromisOpRequest, PromisOpResult,
    TributeOfferResult,
};

/// Self-observed enclave health snapshot returned by [`EnclaveRequest::Health`].
///
/// `heap` comes from the enclave binary's counting global allocator. It is
/// a proxy for EPC pressure (it excludes thread stacks, allocator overhead and
/// direct mmaps). `0` means allocator accounting is not installed. Per-class
/// counters are fixed fields, not maps, so the wire shape stays stable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHealthStatusV1 {
    pub uptime_s: u64,
    pub offer_key_ready: bool,
    pub heap: EnclaveHeapUsageV1,
    pub requests: EnclaveRequestCountsV1,
    pub classes: EnclaveRequestClassCountsV1,
}

/// Enclave heap the allocator accounts for, now and at its peak.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveHeapUsageV1 {
    pub current_bytes: u64,
    pub peak_bytes: u64,
}

/// Requests the enclave dispatched, by outcome.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestCountsV1 {
    pub total: u64,
    pub errored: u64,
    pub denied: u64,
}

/// Requests the enclave dispatched, by authorization class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveRequestClassCountsV1 {
    pub initialized: u64,
    pub founding_keyless: u64,
    pub keyless_onboarding: u64,
    pub ready: u64,
    pub dev_source_seal: u64,
    pub dev_recipient_ingest: u64,
}

/// Domain-tagged message an account owner personal-signs to authorize Fidelity
/// index queries until `expiry`:
/// `"outbe/fidelity/query-auth/v1" || chain_id(32) || account(20) || expiry_be(8)`.
///
/// The enclave hashes this preimage and checks `owner_sig`.
/// The host precompile does not build or check this message.
/// A leaked signature authorizes index reads until `expiry`.
/// It is never key material and cannot decrypt state.
pub fn fidelity_query_auth_message(chain_id: B256, account: Address, expiry: u64) -> Vec<u8> {
    let tag: &[u8] = b"outbe/fidelity/query-auth/v1";
    let mut m = Vec::with_capacity(tag.len() + 32 + 20 + 8);
    m.extend_from_slice(tag);
    m.extend_from_slice(chain_id.as_slice());
    m.extend_from_slice(account.as_slice());
    m.extend_from_slice(&expiry.to_be_bytes());
    m
}

/// Deterministic hash over the canonical batch inputs: every field of every
/// offer. Length-prefixed to be unambiguous.
///
/// This hash must cover every field of `ProcessTributeOfferBatch`. An input that
/// the hash skips is silently unattested, because the host's recompute and the
/// enclave's would agree on ignoring it.
///
/// SHARED by the enclave (which returns it in `TributeOfferBatch`) and the host. The
/// host recomputes it from the request it sent and compares. A mismatch is enclave
/// non-determinism. One definition here keeps the two byte layouts from
/// drifting. Diagnostic only. Never written to chain state.
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
        buf.extend_from_slice(&offer.worldwide_day.value().to_be_bytes());
        buf.extend_from_slice(&offer.tribute_currency.to_be_bytes());
        buf.extend_from_slice(&offer.reference_currency.to_be_bytes());
        buf.extend_from_slice(&offer.issuance_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_wwd_vwap_minor.to_be_bytes::<32>());
        buf.extend_from_slice(&offer.reference_scurve_minor.to_be_bytes::<32>());
        match &offer.zk_context {
            Some(context) => {
                buf.push(1);
                buf.extend_from_slice(context.derived_owner.as_slice());
                buf.extend_from_slice(&context.chain_id.to_be_bytes());
                buf.extend_from_slice(&context.l2_chain_id.to_be_bytes());
            }
            None => buf.push(0),
        }
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-tagged message a caller personal-signs to prove control of `account`
/// before `DeriveAccountKeys` reveals its keys, bound to the target `ledger`:
/// `"outbe/<ledger>/derive-keys/v1" || account(20) || ephemeralPubkey(32)`.
///
/// SHARED by the host RPC (fast reject) and the enclave (the trust boundary) so
/// the two hash an identical preimage. A divergence would let one accept a
/// signature that the other rejects. The Gratis tag byte-matches the historical
/// [`derive_gratis_keys_message`], so existing Gratis clients are unaffected.
pub fn derive_account_keys_message(
    ledger: Ledger,
    account: Address,
    ephemeral_pubkey: B256,
) -> Vec<u8> {
    let tag: &[u8] = match ledger {
        Ledger::Gratis => b"outbe/gratis/derive-keys/v1",
        Ledger::Promis => b"outbe/promis/derive-keys/v1",
        Ledger::Fidelity => b"outbe/fidelity/derive-keys/v1",
    };
    let mut m = Vec::with_capacity(tag.len() + 20 + 32);
    m.extend_from_slice(tag);
    m.extend_from_slice(account.as_slice());
    m.extend_from_slice(ephemeral_pubkey.as_slice());
    m
}

/// Backward-compatible alias for the Gratis key-derivation message
/// (`derive_account_keys_message(Ledger::Gratis, ...)`).
pub fn derive_gratis_keys_message(account: Address, ephemeral_pubkey: B256) -> Vec<u8> {
    derive_account_keys_message(Ledger::Gratis, account, ephemeral_pubkey)
}

/// EIP-191 `personal_sign` digest of `message`. Matches ethers `signMessage`.
pub fn eip191_hash(message: &[u8]) -> B256 {
    let mut buf = Vec::with_capacity(message.len() + 40);
    buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    buf.extend_from_slice(message.len().to_string().as_bytes());
    buf.extend_from_slice(message);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage that the enclave signs (with its Ed25519 attestation
/// key) and the host verifies. It binds the canonical inputs hash to the produced
/// results. Thus the host can prove that the attested enclave computed the results
/// (and that the host did not substitute them). SHARED so the two byte layouts
/// cannot drift. `serde_json` of a fixed-field struct list is deterministic
/// (struct field order is declaration order, and there are no maps or floats).
/// Local-only. Never written to chain state.
pub fn tribute_offer_attestation_preimage(
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
) -> Result<Vec<u8>, serde_json::Error> {
    let results_json = serde_json::to_vec(results)?;
    let mut buf = Vec::with_capacity(30 + 32 + 4 + results_json.len());
    buf.extend_from_slice(b"outbe/tee/offer-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(results_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&results_json);
    Ok(buf)
}

/// Responses returned from the enclave to the node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveResponse {
    /// SGX quote bundle. Carries the enclave public keys in cleartext plus the
    /// `report_data` that binds them. The host recomputes
    /// `keccak256(noise_static_pub || recipient_x25519_pub || attestation_pub)`
    /// and checks that it equals `report_data`. This proves that the cleartext keys
    /// are the attested ones. The host then uses `noise_static_pub` as the Noise-IK
    /// remote static key. Callable before the handshake (unauthenticated).
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
    /// Public first-boot material. This is not attestation and carries no
    /// authority. The node identity must sign all fields in the canonical
    /// initialization manifest before the enclave accepts a Noise initiator.
    InitializationChallenge {
        challenge: [u8; 32],
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
    },
    /// Intent-bound real quote generated only for an authenticated NodeHost.
    /// The canonical intent is echoed byte-for-byte so callers cannot associate
    /// the returned quote with another request.
    DcapQuote {
        intent: Vec<u8>,
        quote_body: Vec<u8>,
        /// Ed25519 proof of possession by the persistent quote-bound
        /// attestation key over `RegistrationIntentV1::intent_hash()`.
        enclave_signature: Vec<u8>,
        /// Canonical `TransitionKeyReadyProofV1` for a transition intent.
        /// Empty for every other operation.
        transition_key_ready_proof: Vec<u8>,
    },
    /// Development-only proof of possession over the exact canonical intent.
    /// The echoed bytes prevent a host from associating the signature with a
    /// different registration.
    RegistrationIntentSignedDevV1 {
        intent: Vec<u8>,
        enclave_signature: Vec<u8>,
    },
    DcapVerificationStartedV1 {
        request_hash: B256,
    },
    DcapVerificationChunkAcceptedV1 {
        request_hash: B256,
        next_offset: u32,
    },
    DcapOnboardingArtifactIngestStartedV1 {
        request_hash: B256,
    },
    DcapOnboardingArtifactChunkAcceptedV1 {
        request_hash: B256,
        next_offset: u32,
    },
    DcapOnboardingArtifactRecordAcceptedV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
    },
    /// Canonical accepted verdict or stable reject code, authenticated by the
    /// persistent quote-bound Ed25519 key over the exact request commitment.
    DcapVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    /// Purpose-bound onboarding verdict plus its deterministic Registry
    /// artifact. Rejected verdicts carry an empty artifact.
    DcapOnboardingVerificationFinishedV1 {
        request_hash: B256,
        outcome: Vec<u8>,
        onboarding_artifact: Vec<u8>,
        attestation_tag: Vec<u8>,
    },
    Handshake {
        noise_msg: Vec<u8>,
    },
    PublicKeys {
        /// True only after the enclave installs the permanent tribute-offer key.
        /// Before that point `recipient_x25519_pub` is the one-time onboarding
        /// recipient key and must never be treated as permanent chain state.
        offer_key_ready: bool,
        recipient_x25519_pub: [u8; 32],
        attestation_pub: [u8; 32],
        noise_static_pub: [u8; 32],
        /// TEE threshold-BLS public key (the enclave's DKG participant identity).
        tee_bls_pub: Vec<u8>,
        /// X25519 share-encryption public key. Dealers seal DKG shares to it.
        dkg_enc_pub: [u8; 32],
        /// Retired binding field. The enclave returns this empty.
        /// Peers trust the share-recipient key through `DkgParticipantAnnounceV1`.
        /// They do not trust this field.
        dkg_enc_sig: Vec<u8>,
    },
    Initialized {
        enclave_id: B256,
        node_host_authorization_hash: B256,
        sealed_loaded: bool,
    },
    RemoteSessionAuthorizedV1 {
        ticket_id: B256,
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
    /// **sealed to each recipient enclave**. There is one opaque ciphertext per
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
        /// The committee's DKG group public key (constant term), carried into the
        /// founding bootstrap payload with the derived tribute offer key.
        group_public_key: Vec<u8>,
    },
    /// One-time onboarding result: the installed offer public key matched the
    /// on-chain commitment.
    FinalizedAdmissionIngestedV1 {
        request_hash: B256,
        tribute_offer_public: [u8; 32],
    },
    TributeOfferBatch {
        results: Vec<TributeOfferResult>,
        /// Diagnostic hash of canonical inputs (incl. price/day/currency).
        /// The host compares it to detect enclave non-determinism, then discards it.
        inputs_canonical_hash: B256,
        /// Local-only attestation tag. The host verifies it against its enclave's
        /// attestation key, then discards it. Never written to state.
        attestation_tag: Vec<u8>,
    },
    /// Result of an `ApplyGratisOp`: new ciphertexts + plaintext receipt.
    GratisOpApplied {
        result: Box<GratisOpResult>,
    },
    /// Result of an `ApplyPromisOp`: new balance ciphertext + plaintext receipt.
    PromisOpApplied {
        result: Box<PromisOpResult>,
    },
    /// Result of an `ApplyFidelityCohortOp`: the plaintext cohort outcome.
    FidelityCohortApplied {
        result: Box<FidelityCohortResult>,
    },
    /// Result of a `SnapshotFidelityLeagues`: one plaintext league per owner, in
    /// request order.
    FidelityLeaguesSnapshotted {
        leagues: Vec<FidelityLeagueEntry>,
        /// Diagnostic hash of canonical inputs. The host compares it to detect
        /// enclave non-determinism, then discards it.
        inputs_canonical_hash: B256,
        /// Local-only attestation tag. The host verifies it, then discards it.
        attestation_tag: Vec<u8>,
    },
    /// Result of a `QueryFidelityIndex`.
    FidelityIndexQueried {
        result: Box<FidelityQueryResult>,
    },
    /// Result of `DeriveAccountKeys`: `AEAD(ECDHE(enclave, requester_ephemeral),
    /// view_key || modify_key)` sealed to the requester. Opaque to the host.
    AccountKeysSealed {
        account: Address,
        sealed: Vec<u8>,
        nonce: [u8; 12],
        enclave_ephemeral_pubkey: [u8; 32],
    },
    Error {
        message: String,
    },
    /// Result of [`EnclaveRequest::Health`]. Appended at the tail per the
    /// wire-compat law on [`EnclaveRequest`].
    HealthStatus {
        status: Box<EnclaveHealthStatusV1>,
    },
    /// Ceremony-scoped DKG participant identity created by the enclave after it
    /// validates the exact participant set and derived ceremony id.
    DkgParticipantAnnounceV1 {
        participant: ParticipantAnnounce,
    },
    /// Exact purpose-bound artifact produced by the resident source enclave for
    /// a GramineDirectDev registration. The attestation tag authenticates the
    /// request commitment and artifact to the local NodeHost.
    GramineDirectDevOnboardingArtifactPreparedV1 {
        request_hash: B256,
        onboarding_artifact: Vec<u8>,
    },
    GramineDirectDevOnboardingArtifactIngestedV1 {
        tribute_offer_public: [u8; 32],
    },
    TransitionEvidenceDevV1 {
        evidence: Vec<u8>,
    },
    RemoteSessionsRetiredV1 {
        activation_height: u64,
    },
    UpgradeKeyExportedV1 {
        request_hash: B256,
        artifact: Vec<u8>,
    },
    NodCreatedForTestV1 {
        nod: outbe_primitives::nod_encryption::EncryptedNodV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodEncryptedForTributeV1 {
        nod: outbe_primitives::nod_encryption::EncryptedNodV1,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMintRejectedV1 {
        reason: String,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    LysisRejected {
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    LysisExecuted {
        result: Box<crate::lysis::LysisResult>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    EncryptedNodMinedV1 {
        result: Box<crate::nod_mine::MineEncryptedNodResultV1>,
    },
    NodAmountReadV1 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    NodTransferAckV1 {
        id: B256,
        next_offset: u32,
    },
    NodTransferReadyV1 {
        id: B256,
        total: u32,
        digest: B256,
    },
    NodTransferOutputV1 {
        id: B256,
        offset: u32,
        bytes: Vec<u8>,
    },
    NodTransferDiscardedV1 {
        id: B256,
    },
    EncryptedTributeOfferBatchV2 {
        results: Vec<crate::tribute_v2::EncryptedTributeOfferResultV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeAmountsReadV2 {
        amounts: Vec<outbe_primitives::tribute_encryption::TributeAmountsV2>,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayOpAppliedV2 {
        record: outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    TributeDayAmountReadV2 {
        amount: U256,
        inputs_canonical_hash: B256,
        attestation_tag: Vec<u8>,
    },
    /// The enclave cannot serve this request yet: not initialized, no resident key,
    /// or an expired session. Unlike `Error`, it reports this enclave's state, not the request's content.
    NotReady {
        message: String,
    },
}

/// Deterministic hash over the canonical inputs of a single Gratis op. SHARED by
/// the enclave (returned in `GratisOpResult`) and the host (recomputed from the
/// request it sent and compared). A mismatch is enclave non-determinism.
/// Length-prefixed to be unambiguous. Diagnostic only. Never written to state.
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
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    match &req.fidelity {
        Some(f) => {
            buf.push(1);
            buf.push(f.op as u8);
            buf.extend_from_slice(&f.timestamp.to_be_bytes());
            buf.extend_from_slice(&f.first_qualified_start.to_be_bytes());
            push_bytes(&mut buf, &f.current_blob);
        }
        None => buf.push(0),
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage that the enclave signs (Ed25519 attestation key) and
/// the host verifies. It binds the canonical inputs hash to the produced result.
/// Thus the host can prove that the result came from the attested enclave. SHARED
/// so the byte layouts cannot drift. Local-only. Never written to chain state.
pub fn gratis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    // Hash the ciphertext-bearing result fields deterministically. serde_json of a
    // fixed-field struct is deterministic (declaration order, no maps/floats). We
    // exclude the tag itself to avoid self-reference.
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    // v3: the result JSON carries the pledged blob.
    let mut buf = Vec::with_capacity(31 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/gratis-attestation/v3");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a single Promis op (the
/// [`promis_op_canonical_hash`] analogue of [`gratis_op_canonical_hash`]). SHARED
/// by the enclave (returned in `PromisOpResult`) and the host (recomputed and
/// compared). A mismatch is enclave non-determinism. Length-prefixed.
/// Diagnostic only. Never written to state.
pub fn promis_op_canonical_hash(req: &PromisOpRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.extend_from_slice(&(req.current_balance.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.current_balance);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    alloy_primitives::keccak256(buf)
}

/// Domain-separated preimage that the enclave signs (Ed25519 attestation key) and
/// the host verifies for a Promis op. It is the [`gratis_op_attestation_preimage`]
/// analogue, with its own domain tag. Thus nobody can replay a Gratis attestation
/// as a Promis one. Local-only. Never written to chain state.
pub fn promis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(31 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/promis-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a standalone Fidelity cohort
/// op. SHARED by the enclave (returned in `FidelityCohortApplied`) and the host
/// (recomputed and compared). Length-prefixed. Diagnostic only.
pub fn fidelity_cohort_canonical_hash(req: &FidelityCohortRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.push(req.section.op as u8);
    buf.extend_from_slice(&req.section.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.section.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.section.current_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.section.current_blob);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a standalone Fidelity cohort op. It
/// has its own tag, so nobody can replay another attestation as one. Local-only.
pub fn fidelity_cohort_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityCohortResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(39 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-cohort-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a Fidelity league snapshot
/// batch. SHARED by the enclave (returned in `FidelityLeaguesSnapshotted`) and
/// the host (recomputed and compared). A mismatch is enclave non-determinism.
/// Length-prefixed. Diagnostic only. Never written to state.
pub fn fidelity_snapshot_canonical_hash(req: &FidelitySnapshotRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&req.timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&(req.entries.len() as u32).to_be_bytes());
    for entry in &req.entries {
        buf.extend_from_slice(entry.owner.as_slice());
        buf.extend_from_slice(&(entry.cohort_blob.len() as u32).to_be_bytes());
        buf.extend_from_slice(&entry.cohort_blob);
    }
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a Fidelity snapshot batch. It is the
/// [`gratis_op_attestation_preimage`] analogue with its own tag. Local-only.
pub fn fidelity_snapshot_attestation_preimage(
    inputs_canonical_hash: B256,
    leagues: &[FidelityLeagueEntry],
) -> Result<Vec<u8>, serde_json::Error> {
    let leagues_json = serde_json::to_vec(leagues)?;
    let mut buf = Vec::with_capacity(41 + 32 + 4 + leagues_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-snapshot-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(leagues_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&leagues_json);
    Ok(buf)
}

/// Deterministic hash over the canonical inputs of a single Fidelity index
/// query. SHARED by the enclave (returned in `FidelityQueryResult`) and the host
/// (recomputed and compared). Length-prefixed. Diagnostic only.
pub fn fidelity_query_canonical_hash(req: &FidelityQueryRequest) -> B256 {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&(req.cohort_blob.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.cohort_blob);
    buf.extend_from_slice(&req.query_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.block_timestamp.to_be_bytes());
    buf.extend_from_slice(&req.first_qualified_start.to_be_bytes());
    buf.extend_from_slice(&req.expiry.to_be_bytes());
    buf.extend_from_slice(&(req.owner_sig.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.owner_sig);
    alloy_primitives::keccak256(buf)
}

/// Domain-separated attestation preimage for a Fidelity index query. It has its
/// own tag, so nobody can replay another attestation as one. Local-only.
pub fn fidelity_query_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &FidelityQueryResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag = Vec::new();
    let result_json = serde_json::to_vec(&probe)?;
    let mut buf = Vec::with_capacity(38 + 32 + 4 + result_json.len());
    buf.extend_from_slice(b"outbe/tee/fidelity-query-attestation/v1");
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(result_json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&result_json);
    Ok(buf)
}
