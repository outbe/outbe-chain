use alloy_primitives::{Address, B256, U256};

use super::{
    EncryptedTributeOffer, FidelityCohortRequest, FidelityQueryRequest, FidelitySnapshotRequest,
    GratisOpRequest, Ledger, ParticipantAnnounce, PromisOpRequest,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveRequest {
    /// Development-only pre-handshake quote. The production server
    /// rejects this variant and never routes it after initialization.
    GetQuote {
        nonce: [u8; 32],
    },
    /// Production pre-handshake discovery for an uninitialized enclave. Returns
    /// one challenge plus the persistent enclave public keys that the node
    /// identity signs. The enclave rejects it after initialization is committed.
    GetInitializationChallenge,
    /// Production pre-handshake initialization authorization. `manifest` is the
    /// canonical `EnclaveInitializationManifestV1`. `node_signature` is a
    /// recoverable secp256k1 signature (`r || s || v`). The following Noise IK
    /// message 1 must authenticate the exact NodeHost key in the manifest.
    Initialize {
        manifest: Vec<u8>,
        node_signature: Vec<u8>,
    },
    /// Production pre-handshake marker for an already initialized enclave. The
    /// responder returns nothing until the following Noise IK message 1 proves
    /// possession of the sealed NodeHost static key.
    OpenSession,
    /// Production pre-handshake marker for one previously authorized remote
    /// source NodeHost. The enclave consumes the ticket before Noise message 1.
    /// That message must prove the exact initiator static stored under this id.
    OpenRemoteSessionV1 {
        ticket_id: B256,
    },
    /// Noise-IK handshake message.
    SessionHandshake {
        noise_msg: Vec<u8>,
    },
    /// Return the enclave's public keys (recipient X25519, attestation, Noise
    /// static, tribute-BLS).
    GetPublicKeys,
    /// Local-owner command that installs one bounded, one-use remote Noise
    /// admission previously derived from exact finalized Registry state.
    AuthorizeRemoteSessionV1 {
        ticket_id: B256,
        initiator_static_x25519: [u8; 32],
        responder_static_x25519: [u8; 32],
        deadline: u64,
        finalized_block_hash: B256,
    },
    /// Generate a fresh DCAP quote for the exact canonical registration,
    /// renewal or transition intent. Production accepts this only inside an
    /// authenticated NodeHost session and only when the intent matches the
    /// sealed identity. A transition additionally returns a purpose-bound
    /// proof that this enclave has the permanent offer key resident.
    GenerateDcapQuote {
        intent: Vec<u8>,
    },
    /// Sign one exact GramineDirectDev registration intent inside the enclave.
    /// The development transport accepts this command. An authenticated
    /// production NodeHost session also accepts it when the enclave itself
    /// detects SGX with remote attestation disabled. It never returns an SGX
    /// quote or hardware-attestation claim.
    SignRegistrationIntentDevV1 {
        intent: Vec<u8>,
    },

    /// Start one bounded, request-committed DCAP verification upload. Evidence
    /// and policy bytes follow in strictly sequential chunks on this same
    /// authenticated Noise session.
    BeginDcapVerificationV1 {
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
    },
    /// Start the dedicated `RegisterEnclave` verify-and-seal flow. Unlike the
    /// generic verifier, this request commits both authorization signatures and
    /// exact Registry offer-key epochs before the enclave accepts any evidence
    /// bytes.
    BeginDcapOnboardingVerificationV1 {
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
        node_signature: Vec<u8>,
        enclave_signature: Vec<u8>,
        expected_tribute_offer_public: [u8; 32],
        key_epoch: u64,
        tribute_offer_epoch: u64,
    },
    /// Append the next exact byte range to the active verification upload.
    DcapVerificationChunkV1 {
        request_hash: B256,
        offset: u32,
        bytes: Vec<u8>,
    },
    /// Finish the exact upload and run the full enclave-resident verifier.
    FinishDcapVerificationV1 {
        request_hash: B256,
    },

    /// Open a TEE DKG ceremony session inside the enclave. Each `participants[i]`
    /// bundles a BLS identity, its announced X25519 share-encryption key, and the
    /// owner's signature binding the two. Thus the untrusted host cannot mis-pair
    /// or duplicate enc keys. The enclave:
    /// 1. verifies every binding.
    /// 2. rejects duplicate enc keys.
    /// 3. builds the ceremony `Info` from the BLS set.
    /// 4. captures the enc keys so that it can seal dealings to recipients.
    ///
    /// The host relays `ParticipantAnnounce` values from
    /// `DkgParticipantAnnounceV1`. It does not take them from `PublicKeys`.
    DkgOpen {
        ceremony_id: B256,
        round: u64,
        participants: Vec<ParticipantAnnounce>,
    },
    /// Seam A: deal + seal per-player shares. Returns the public commitment and
    /// one opaque sealed share per participant.
    DkgStartDealer {
        ceremony_id: B256,
    },
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
    DkgDealerFinalize {
        ceremony_id: B256,
    },
    /// Seam E: verify the collected signed dealer logs and recover this enclave's
    /// local threshold share (committed inside the enclave). Returns the public
    /// group key and the share commitment.
    DkgPlayerFinalize {
        ceremony_id: B256,
        signed_logs: Vec<Vec<u8>>,
    },
    /// Seam F (offer key): threshold-sign the fixed offer message with this
    /// enclave's recovered share. Then **seal the partial to every recipient
    /// enclave's X25519 key** (one ciphertext per participant). The host relays
    /// only the opaque ciphertexts. It never sees a plaintext partial, so it
    /// cannot recover the group signature (and hence the offer key) itself.
    /// Requires `DkgPlayerFinalize` first.
    DkgTributeOfferPartial {
        ceremony_id: B256,
    },
    /// Founding Seam F: finalize the initial group threshold signature from the
    /// sealed partials addressed to THIS enclave (decrypted in-SGX). Then install
    /// the one permanent offer X25519 keypair. The capability matrix permits
    /// this request only to a keyless Validator. It is not a lost-key recovery
    /// or post-genesis replacement surface. Releases the ceremony session.
    DkgFinalizeTributeOffer {
        ceremony_id: B256,
        /// Sealed partials addressed to this enclave (one `EncryptedShare` blob per
        /// signer). The enclave decrypts them with its X25519 share-decryption
        /// secret.
        sealed_partials: Vec<Vec<u8>>,
        chain_id: B256,
        tribute_offer_epoch: u64,
    },

    /// Decrypt a batch of offers, apply each one's node-resolved price, and
    /// return the canonical Tribute results. Each `EncryptedTributeOffer` is
    /// self-contained (its own owner, day, currencies and price), so the batch is
    /// simply a list. A single transaction carries one offer today. The list
    /// prepares for multi-offer txs. This is the sole offer-processing
    /// entrypoint. The enclave decrypts, applies the price, computes economics +
    /// Poseidon `token_id`, and returns `TributeOfferResult`.
    ProcessTributeOfferBatch {
        offers: Vec<EncryptedTributeOffer>,
    },

    /// Start one streaming finalized-admission verification rooted in the
    /// measured genesis committee. The enclave retains only the current
    /// committee. Transition records follow on this authenticated session.
    BeginDcapOnboardingArtifactIngestV1 {
        request_hash: B256,
        artifact: Vec<u8>,
        anchor_outcome: Vec<u8>,
        expected_intent_hash: B256,
        expected_tribute_offer_public: [u8; 32],
        expected_key_epoch: u64,
        expected_tribute_offer_epoch: u64,
    },
    /// Append bytes to the current transition or admission record.
    DcapOnboardingArtifactChunkV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
        offset: u32,
        bytes: Vec<u8>,
    },
    /// Verify the current complete record. Transition records advance and prune
    /// the committee cursor. The admission record authenticates Registry state.
    CommitDcapOnboardingArtifactRecordV1 {
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
    },
    /// After a verified admission record, decrypt,
    /// durably seal, and only finally activate the resident offer key.
    FinishDcapOnboardingArtifactIngestV1 {
        request_hash: B256,
    },

    /// Apply a Gratis write op over encrypted per-account state. The enclave:
    /// 1. derives the resident `gratis_state_key` from the same group signature
    ///    as the offer key.
    /// 2. decrypts the supplied blobs.
    /// 3. enforces balance invariants + modify-key authorization.
    /// 4. re-encrypts deterministically.
    ///
    /// This is a consensus path (called inside precompile `dispatch`, re-executed
    /// by every validator).
    ApplyGratisOp {
        request: Box<GratisOpRequest>,
    },

    /// Like [`EnclaveRequest::ApplyGratisOp`] but for the confidential Promis
    /// ledger (mint/burn over an encrypted balance). Consensus path, re-executed
    /// by every validator.
    ApplyPromisOp {
        request: Box<PromisOpRequest>,
    },

    /// Off-chain key delivery: derive `account`'s view + modify keys for `ledger`
    /// from the matching resident state key. Then seal them to the requester's
    /// ephemeral X25519 key. NOT a consensus path. Served only over RPC, never
    /// during block execution.
    ///
    /// `owner_sig` is the 65-byte (`r||s||v`) EIP-191 `personal_sign` signature by
    /// `account` over `derive_account_keys_message(ledger, account,
    /// requester_ephemeral_pubkey)`. The enclave recovers it and rejects unless the
    /// signer equals `account`, so the enclave releases the keys only to the account
    /// owner. The trust boundary is the enclave. It is not the (untrusted) host RPC,
    /// which also checks the signature as a fast reject. Carried as `Vec<u8>` because
    /// serde does not derive for `[u8; 65]`. The enclave validates the length.
    DeriveAccountKeys {
        ledger: Ledger,
        account: Address,
        requester_ephemeral_pubkey: [u8; 32],
        owner_sig: Vec<u8>,
    },

    /// Apply a standalone Fidelity cohort mutation (`In`/`Out`) over encrypted
    /// per-account state, on its own round-trip. Consensus path, re-executed by
    /// every validator. See [`FidelityCohortRequest`].
    ApplyFidelityCohortOp {
        request: Box<FidelityCohortRequest>,
    },

    /// Batch-decrypt cohort blobs and return one plaintext league per owner. This
    /// is metadosis's once-per-WWD Fidelity snapshot. Consensus path (Metadosis
    /// preparation step in begin-block, re-executed by every validator).
    SnapshotFidelityLeagues {
        request: Box<FidelitySnapshotRequest>,
    },

    /// Owner-authorized read of one account's RCFI/league over its encrypted
    /// cohorts (signed, expiring authorization, see [`FidelityQueryRequest`]).
    /// NOT a consensus path. Served via `eth_call`.
    QueryFidelityIndex {
        request: Box<FidelityQueryRequest>,
    },

    /// Read-only health/telemetry probe: uptime, request counters, offer-key
    /// readiness and self-observed heap usage. Never touches keys or sealed
    /// state. NOT a consensus path. Served to the local NodeHost only.
    ///
    /// WIRE-COMPAT LAW: the codec encodes enums by variant declaration index
    /// (postcard). Thus:
    /// - append new variants ONLY at the tail of `EnclaveRequest` /
    ///   `EnclaveResponse`.
    /// - never add, remove or reorder fields of existing wire structs.
    ///
    /// Deployment order for a new variant: enclave binary first, node second.
    Health,
    /// Ask this initialized enclave to produce its exact ceremony-scoped DKG
    /// participant announcement. `participant_bls` is canonicalized and bound
    /// into both the ceremony id and signature inside the enclave.
    DkgParticipantAnnounceV1 {
        ceremony_id: B256,
        round: u64,
        participant_bls: Vec<Vec<u8>>,
    },
    /// Produce a deterministic offer-key onboarding artifact for one exact
    /// GramineDirectDev registration. Only an initialized local NodeHost can use
    /// this request, and only when both conditions are true:
    /// - its sealed network binding selected GramineDirectDev.
    /// - its permanent offer key is already resident.
    ///
    /// The recipient still must prove finalized TeeRegistry admission before it
    /// can install the key.
    PrepareGramineDirectDevOnboardingArtifactV1 {
        request_hash: B256,
        context: Vec<u8>,
    },
    /// Install an exact finalized DirectDev onboarding artifact into this
    /// initialized keyless enclave. The CLI exposes this only after the exact
    /// registration transaction and TeeRegistry binding are finalized.
    IngestGramineDirectDevOnboardingArtifactV1 {
        artifact: Vec<u8>,
        expected_intent_hash: B256,
        expected_tribute_offer_public: [u8; 32],
        expected_key_epoch: u64,
        expected_tribute_offer_epoch: u64,
    },
    /// Appended wire variant. Proves resident-key readiness without a DCAP quote.
    GenerateTransitionEvidenceDevV1 {
        intent: Vec<u8>,
    },
    /// Revoke pre-boundary remote tickets and open sessions. Owner-only and monotonic.
    RetireRemoteSessionsV1 {
        activation_height: u64,
    },
    AuthorizeRemoteSessionV2 {
        ticket_id: B256,
        initiator_static_x25519: [u8; 32],
        responder_static_x25519: [u8; 32],
        deadline: u64,
        finalized_block_hash: B256,
        retirement_height: u64,
    },
    /// Upgrade transfer uses finalized pending-candidate proofs in both directions.
    /// Export carries a canonical placeholder artifact with the recipient context.
    BeginUpgradeKeyTransferV1 {
        request_hash: B256,
        artifact: Vec<u8>,
        anchor_outcome: Vec<u8>,
        export: bool,
    },
    CreateNodForTestV2 {
        terms: outbe_primitives::nod_encryption::NodTermsV2,
        creator_public: [u8; 32],
        amount: U256,
    },
    MineEncryptedNodV2 {
        request: Box<crate::nod_mine::MineEncryptedNodRequestV2>,
    },
    ReadNodAmountV2 {
        nod: outbe_primitives::nod_encryption::EncryptedNodV2,
    },
    NodTransferChunkV2 {
        id: B256,
        total: u32,
        offset: u32,
        bytes: Vec<u8>,
    },
    ExecuteNodTransferV2 {
        id: B256,
    },
    ReadNodTransferV2 {
        id: B256,
        offset: u32,
    },
    DiscardNodTransferV2 {
        id: B256,
    },
    /// Creator-owned encrypted Tribute records; requires the installed network key.
    ProcessEncryptedTributeOfferBatchV2 {
        offers: Vec<EncryptedTributeOffer>,
    },
    /// Private calculation bridge for authenticated NodeHost consumers.
    ReadTributeAmountsV2 {
        tributes: Vec<outbe_primitives::tribute_encryption::EncryptedTributeV2>,
    },
    ApplyTributeDayOpV2 {
        request: Box<crate::tribute_day::TributeDayOpRequestV2>,
    },
    ReadTributeDayAmountV2 {
        record: outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2,
    },
    ExecuteLysis {
        request: Box<crate::lysis::LysisRequest>,
    },
}

impl EnclaveRequest {
    /// Stable snake_case label for this request, used as a metric/log key on
    /// both the node client and the enclave server. Never wire data.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::CreateNodForTestV2 { .. } => "create_nod_for_test_v2",
            Self::ExecuteLysis { .. } => "execute_lysis",
            Self::MineEncryptedNodV2 { .. } => "mine_encrypted_nod_v2",
            Self::ReadNodAmountV2 { .. } => "read_nod_amount_v2",
            Self::NodTransferChunkV2 { .. } => "nod_transfer_chunk_v2",
            Self::ExecuteNodTransferV2 { .. } => "execute_nod_transfer_v2",
            Self::ReadNodTransferV2 { .. } => "read_nod_transfer_v2",
            Self::DiscardNodTransferV2 { .. } => "discard_nod_transfer_v2",
            Self::ApplyTributeDayOpV2 { .. } => "apply_tribute_day_op_v2",
            Self::ReadTributeDayAmountV2 { .. } => "read_tribute_day_amount_v2",
            Self::ProcessEncryptedTributeOfferBatchV2 { .. } => {
                "process_encrypted_tribute_offer_batch_v2"
            }
            Self::ReadTributeAmountsV2 { .. } => "read_tribute_amounts_v2",
            Self::BeginUpgradeKeyTransferV1 { .. } => "begin_upgrade_key_transfer_v1",
            Self::GetQuote { .. } => "get_quote",
            Self::GetInitializationChallenge => "get_initialization_challenge",
            Self::Initialize { .. } => "initialize",
            Self::OpenSession => "open_session",
            Self::OpenRemoteSessionV1 { .. } => "open_remote_session_v1",
            Self::SessionHandshake { .. } => "session_handshake",
            Self::GetPublicKeys => "get_public_keys",
            Self::AuthorizeRemoteSessionV1 { .. } => "authorize_remote_session_v1",
            Self::GenerateDcapQuote { .. } => "generate_dcap_quote",
            Self::SignRegistrationIntentDevV1 { .. } => "sign_registration_intent_dev_v1",
            Self::GenerateTransitionEvidenceDevV1 { .. } => "generate_transition_evidence_dev_v1",
            Self::RetireRemoteSessionsV1 { .. } => "retire_remote_sessions_v1",
            Self::AuthorizeRemoteSessionV2 { .. } => "authorize_remote_session_v2",
            Self::BeginDcapVerificationV1 { .. } => "begin_dcap_verification_v1",
            Self::BeginDcapOnboardingVerificationV1 { .. } => {
                "begin_dcap_onboarding_verification_v1"
            }
            Self::DcapVerificationChunkV1 { .. } => "dcap_verification_chunk_v1",
            Self::FinishDcapVerificationV1 { .. } => "finish_dcap_verification_v1",
            Self::DkgOpen { .. } => "dkg_open",
            Self::DkgParticipantAnnounceV1 { .. } => "dkg_participant_announce_v1",
            Self::DkgStartDealer { .. } => "dkg_start_dealer",
            Self::DkgPlayerIngest { .. } => "dkg_player_ingest",
            Self::DkgDealerReceiveAck { .. } => "dkg_dealer_receive_ack",
            Self::DkgDealerFinalize { .. } => "dkg_dealer_finalize",
            Self::DkgPlayerFinalize { .. } => "dkg_player_finalize",
            Self::DkgTributeOfferPartial { .. } => "dkg_tribute_offer_partial",
            Self::DkgFinalizeTributeOffer { .. } => "dkg_finalize_tribute_offer",
            Self::ProcessTributeOfferBatch { .. } => "process_tribute_offer_batch",
            Self::BeginDcapOnboardingArtifactIngestV1 { .. } => {
                "begin_dcap_onboarding_artifact_ingest_v1"
            }
            Self::DcapOnboardingArtifactChunkV1 { .. } => "dcap_onboarding_artifact_chunk_v1",
            Self::CommitDcapOnboardingArtifactRecordV1 { .. } => {
                "commit_dcap_onboarding_artifact_record_v1"
            }
            Self::FinishDcapOnboardingArtifactIngestV1 { .. } => {
                "finish_dcap_onboarding_artifact_ingest_v1"
            }
            Self::ApplyGratisOp { .. } => "apply_gratis_op",
            Self::ApplyPromisOp { .. } => "apply_promis_op",
            Self::DeriveAccountKeys { .. } => "derive_account_keys",
            Self::ApplyFidelityCohortOp { .. } => "apply_fidelity_cohort_op",
            Self::SnapshotFidelityLeagues { .. } => "snapshot_fidelity_leagues",
            Self::QueryFidelityIndex { .. } => "query_fidelity_index",
            Self::Health => "health",
            Self::PrepareGramineDirectDevOnboardingArtifactV1 { .. } => {
                "prepare_gramine_direct_dev_onboarding_artifact_v1"
            }
            Self::IngestGramineDirectDevOnboardingArtifactV1 { .. } => {
                "ingest_gramine_direct_dev_onboarding_artifact_v1"
            }
        }
    }

    /// True when re-sending this request after a lost response cannot
    /// double-apply enclave state. The request is pure or deterministic, so the
    /// reconnect-and-retry path may re-send it once. State-mutating requests must
    /// never be re-sent implicitly. These include initialization, DKG
    /// seams, artifact ingestion, session admission and the multi-frame DCAP
    /// upload. The exhaustive match forces every future variant to make this
    /// choice.
    pub const fn is_idempotent(&self) -> bool {
        match self {
            Self::CreateNodForTestV2 { .. } => true,
            Self::GetQuote { .. }
            | Self::ExecuteLysis { .. }
            | Self::MineEncryptedNodV2 { .. }
            | Self::ReadNodAmountV2 { .. }
            | Self::NodTransferChunkV2 { .. }
            | Self::ExecuteNodTransferV2 { .. }
            | Self::ReadNodTransferV2 { .. }
            | Self::DiscardNodTransferV2 { .. }
            | Self::ProcessEncryptedTributeOfferBatchV2 { .. }
            | Self::ReadTributeAmountsV2 { .. }
            | Self::ApplyTributeDayOpV2 { .. }
            | Self::ReadTributeDayAmountV2 { .. }
            | Self::GetPublicKeys
            | Self::GenerateDcapQuote { .. }
            | Self::SignRegistrationIntentDevV1 { .. }
            | Self::GenerateTransitionEvidenceDevV1 { .. }
            | Self::RetireRemoteSessionsV1 { .. }
            | Self::ProcessTributeOfferBatch { .. }
            | Self::ApplyGratisOp { .. }
            | Self::ApplyPromisOp { .. }
            | Self::DeriveAccountKeys { .. }
            | Self::ApplyFidelityCohortOp { .. }
            | Self::SnapshotFidelityLeagues { .. }
            | Self::QueryFidelityIndex { .. }
            | Self::Health
            | Self::PrepareGramineDirectDevOnboardingArtifactV1 { .. } => true,
            Self::BeginUpgradeKeyTransferV1 { .. }
            | Self::GetInitializationChallenge
            | Self::Initialize { .. }
            | Self::OpenSession
            | Self::OpenRemoteSessionV1 { .. }
            | Self::SessionHandshake { .. }
            | Self::AuthorizeRemoteSessionV1 { .. }
            | Self::AuthorizeRemoteSessionV2 { .. }
            | Self::BeginDcapVerificationV1 { .. }
            | Self::BeginDcapOnboardingVerificationV1 { .. }
            | Self::DcapVerificationChunkV1 { .. }
            | Self::FinishDcapVerificationV1 { .. }
            | Self::DkgParticipantAnnounceV1 { .. }
            | Self::DkgOpen { .. }
            | Self::DkgStartDealer { .. }
            | Self::DkgPlayerIngest { .. }
            | Self::DkgDealerReceiveAck { .. }
            | Self::DkgDealerFinalize { .. }
            | Self::DkgPlayerFinalize { .. }
            | Self::DkgTributeOfferPartial { .. }
            | Self::DkgFinalizeTributeOffer { .. }
            | Self::BeginDcapOnboardingArtifactIngestV1 { .. }
            | Self::DcapOnboardingArtifactChunkV1 { .. }
            | Self::CommitDcapOnboardingArtifactRecordV1 { .. }
            | Self::FinishDcapOnboardingArtifactIngestV1 { .. }
            | Self::IngestGramineDirectDevOnboardingArtifactV1 { .. } => false,
        }
    }
}
