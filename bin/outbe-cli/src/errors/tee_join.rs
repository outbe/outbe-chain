//! Typed errors for the `tee join` workflow.

use alloy_primitives::{Address, U256};

use super::TeeIdentityError;

/// Errors during finalized admission streaming and recovery for `tee join`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeJoinAdmissionError {
    /// Enclave transport error during finalized admission.
    #[error("{0}")]
    Transport(#[from] outbe_tee::TransportError),
    /// Reconnecting to the exact enclave after a connection fault failed.
    #[error("finalized-admission connection fault ({fault}); exact reconnect failed: {reconnect}")]
    ReconnectFailed {
        fault: outbe_tee::TransportError,
        reconnect: outbe_tee::TransportError,
    },
    /// Probing the offer key state after reconnecting failed.
    #[error("finalized-admission connection fault ({fault}); authenticated recovery probe failed: {probe}")]
    RecoveryProbeFailed {
        fault: outbe_tee::TransportError,
        probe: Box<TeeJoinError>,
    },
    /// The resident offer key after reconnect does not match the registry.
    #[error("resident permanent offer key does not match finalized TeeRegistry after reconnect")]
    OfferKeyMismatchAfterReconnect,
    /// The enclave remained keyless after a second connection fault.
    #[error("finalized-admission replay exhausted after a second connection fault; authenticated enclave remains keyless")]
    ReplayExhausted,
    /// The finalized registry view no longer has the completed join binding.
    #[error("finalized Registry lost the completed join binding")]
    MissingCompletedJoinBinding,
    /// The finalized registry binding does not match the expected join anchor.
    #[error("finalized join admission anchor differs from the completed join binding")]
    AnchorBindingMismatch,
    /// Writing the admission anchor for the finalized join to disk failed.
    #[error("persist finalized join admission anchor: {0}")]
    PersistAdmissionAnchor(outbe_tee::TransportError),
    /// The CLI cannot prove onboarding admission at block height zero.
    #[error("onboarding admission cannot be proved at genesis")]
    GenesisHeightAdmission,
    /// The genesis file has no `policySchedule` field under `teeAttestationV1`.
    #[error("final genesis has no TEE policy schedule")]
    MissingGenesisPolicySchedule,
    /// Decoding the canonical schedule for the TEE policy from genesis failed.
    #[error("decode exact final genesis TEE policy schedule: {0}")]
    DecodeGenesisPolicySchedule(outbe_primitives::tee_attestation_v1::CodecError),
    /// The genesis chain identifier or hash differs from the onboarding context.
    #[error("exact final genesis does not match the onboarding network identity")]
    GenesisNetworkIdentityMismatch,
    /// Decoding the consensus block artifacts failed.
    #[error("decode block {height} artifacts: {reason}")]
    DecodeBlockArtifacts { height: u64, reason: String },
    /// No epoch-0 `BoundaryOutcome` exists in finalized block history.
    #[error("finalized history has no epoch-0 BoundaryOutcome")]
    MissingEpochZeroBoundaryOutcome,
    /// Encoding a canonical header for the committee transition failed.
    #[error("encode committee transition: {0}")]
    EncodeCommitteeTransition(outbe_tee::finalized_admission::FinalizedAdmissionCodecError),
    /// Incrementing the expected epoch for the committee transition overflowed.
    #[error("committee transition epoch overflow")]
    CommitteeEpochOverflow,
    /// The `eth_getProof` response has no `storageProof` array.
    #[error("eth_getProof has no storageProof array")]
    MissingStorageProofArray,
    /// The `eth_getProof` response omitted a required `TeeRegistry` slot.
    #[error("eth_getProof omitted a required TeeRegistry slot")]
    OmittedRequiredRegistrySlot,
    /// Encoding the finalized witness for onboarding admission failed.
    #[error("encode finalized onboarding admission witness: {0}")]
    EncodeAdmissionWitness(outbe_tee::finalized_admission::FinalizedAdmissionCodecError),
}

/// Errors during offer key ingestion and candidate promotion for `tee join`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeJoinCompletionError {
    /// Encoding the finalized onboarding artifact failed.
    #[error("encode finalized artifact: {0:#06x}")]
    EncodeFinalizedArtifact(u16),
    /// `GramineDirectDev` post-bootstrap onboarding requires a node data directory.
    #[error("GramineDirectDev post-bootstrap onboarding requires authenticated NodeHost state")]
    GramineDirectDevMissingNodeHostState,
    /// The enclave returned an unexpected response during development onboarding.
    #[error("GramineDirectDev onboarding returned an unexpected response: {0}")]
    UnexpectedGramineDirectDevResponse(String),
    /// Constructing the finalized replacement authorization failed.
    #[error("authorize finalized candidate promotion: {0}")]
    AuthorizeCandidatePromotion(outbe_tee::TransportError),
    /// Promoting the replacement candidate to the committed manifest failed.
    #[error("promote finalized rejoin candidate: {0}")]
    PromoteCandidate(outbe_tee::TransportError),
    /// Clearing the committed join checkpoint after completion failed.
    #[error("clear completed committed join checkpoint: {0}")]
    ClearCommittedCheckpoint(outbe_tee::TransportError),
    /// The finalized registry view no longer has the candidate binding.
    #[error("finalized Registry lost the candidate binding")]
    MissingCandidateBinding,
    /// The finalized registry binding differs from the durable candidate intent.
    #[error("finalized Registry binding differs from the durable candidate intent")]
    CandidateBindingIntentMismatch,
    /// Reopening the committed enclave after promotion failed.
    #[error("reopen committed enclave: {0}")]
    ReopenCommittedEnclave(outbe_tee::TransportError),
    /// The reopened committed enclave did not report the expected offer key.
    #[error("durable onboarding reopen did not expose the exact permanent offer key: {0}")]
    ReopenedOfferKeyNotReady(String),
}

/// Errors when the CLI connects or operates the join enclave client.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeJoinEnclaveError {
    /// Generating the intent-bound DCAP quote in the enclave failed.
    #[error("generate intent-bound DCAP quote: {0}")]
    GenerateDcapQuote(outbe_tee::TransportError),
    /// Signing the registration intent in the development enclave failed.
    #[error("sign development V1 intent: {0}")]
    SignDevIntent(outbe_tee::TransportError),
    /// Enclave transport request failed.
    #[error("{0}")]
    Transport(#[from] outbe_tee::TransportError),
    /// The committed NodeHost manifest does not match the chain or node identity.
    #[error("committed NodeHost manifest does not match the active chain and node identity")]
    ManifestIdentityMismatch,
    /// Deriving the enclave identifier from the committed manifest failed.
    #[error("invalid committed enclave identity: {0}")]
    InvalidCommittedEnclaveIdentity(outbe_primitives::tee_attestation_v1::CodecError),
    /// Deriving the authorization hash from the committed manifest failed.
    #[error("invalid committed NodeHost authorization: {0}")]
    InvalidCommittedAuthorization(outbe_primitives::tee_attestation_v1::CodecError),
    /// Loading the committed NodeHost manifest from disk failed.
    #[error("load committed NodeHost manifest: {0}")]
    LoadCommittedManifest(outbe_tee::TransportError),
    /// The enclave endpoint matches neither the committed manifest nor a candidate.
    #[error("endpoint matches neither the committed enclave ({committed}) nor a resumable fresh candidate ({candidate})")]
    EndpointMatchesNeither {
        committed: outbe_tee::TransportError,
        candidate: outbe_tee::TransportError,
    },
    /// Reconnecting to the unregistered committed enclave failed.
    #[error("reconnect unregistered committed NodeHost enclave: {0}")]
    ReconnectUnregisteredCommitted(outbe_tee::TransportError),
    /// Initializing a fresh production NodeHost enclave failed.
    #[error("production NodeHost initialization failed: {0}")]
    InitializeNodeHost(outbe_tee::TransportError),
    /// Connecting to the development enclave endpoint failed.
    #[error("connect development enclave at {socket}: {source}")]
    ConnectDevelopmentEnclave {
        socket: String,
        source: outbe_tee::TransportError,
    },
    /// The caller connected a hardware-attested enclave under `GramineDirectDev` policy.
    #[error("GramineDirectDev policy requires the separate non-SGX development transport")]
    GramineDirectDevHardwareAttested,
    /// The development enclave returned an unexpected quote response.
    #[error("expected development Quote, got {0}")]
    UnexpectedDevelopmentQuote(String),
}

/// Errors for `tee join` orchestration, policy checks, preflight, and relay.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeJoinError {
    /// `tee join` requires the global `--private-key` flag.
    #[error("tee join requires the global --private-key EVM signer")]
    PrivateKeyRequired,
    /// `tee join` requires the `--reth-p2p-secret-key` flag.
    #[error("tee join requires --reth-p2p-secret-key")]
    MissingRethP2pSecretKey,
    /// Hashing the V1 node identity failed.
    #[error("hash V1 node identity: {0}")]
    HashNodeIdentity(outbe_primitives::tee_attestation_v1::CodecError),
    /// Computing the hash of the active V1 policy failed.
    #[error("active V1 policy is invalid: {0}")]
    InvalidActivePolicy(outbe_primitives::tee_attestation_v1::CodecError),
    /// Computing the hash of the V1 registration intent failed.
    #[error("invalid V1 registration intent: {0}")]
    InvalidRegistrationIntent(outbe_primitives::tee_attestation_v1::CodecError),
    /// Signing the V1 registration intent with the node key failed.
    #[error("sign V1 node intent: {0}")]
    SignNodeIntent(TeeIdentityError),
    /// Calculating the maximum gas limit for `registerEnclave` failed.
    #[error("calculate V1 registration gas: {0}")]
    CalculateRegistrationGas(outbe_primitives::tee_attestation_v1::CodecError),
    /// Signing the NodeHost side of the validator address binding failed.
    #[error("sign NodeHost side of address binding: {0}")]
    SignNodeBinding(TeeIdentityError),
    /// Encoding the validator-to-NodeHost binding failed.
    #[error("encode address-to-NodeHost binding: {0}")]
    EncodeValidatorBinding(outbe_primitives::tee_attestation_v1::CodecError),
    /// The durable join checkpoint targets a different enclave lifecycle.
    #[error("durable tee join checkpoint targets another enclave lifecycle")]
    CheckpointLifecycleMismatch,
    /// The requested join parameters differ from the durable checkpoint.
    #[error("requested tee join does not match its durable checkpoint")]
    CheckpointFieldsMismatch,
    /// Decoding the durable attestation evidence for the join failed.
    #[error("decode durable join evidence: {0}")]
    DecodeDurableEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// The requested join intent conflicts with the durable registration record.
    #[error("requested tee join conflicts with the durable registration")]
    DurableRegistrationConflict,
    /// Encoding the canonical V1 attestation evidence failed.
    #[error("encode canonical V1 evidence: {0}")]
    EncodeEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// Acquiring the canonical DCAP collateral from the host failed.
    #[error("acquire canonical DCAP collateral: {0}")]
    AcquireDcapCollateral(outbe_tee::TransportError),
    /// Persisting the registration submission for the replacement candidate failed.
    #[error("persist candidate registration: {0}")]
    PersistCandidateRegistration(outbe_tee::TransportError),
    /// Persisting the registration submission for the committed join failed.
    #[error("persist committed registration: {0}")]
    PersistCommittedRegistration(outbe_tee::TransportError),
    /// Re-decoding the canonical V1 evidence bytes failed.
    #[error("re-decode canonical V1 evidence: {0}")]
    RedecodeEvidence(outbe_primitives::tee_attestation_v1::CodecError),
    /// Authenticated onboarding lost its required node data directory.
    #[error("authenticated onboarding lost its required node data directory")]
    MissingAuthenticatedNodeDataDir,
    /// A different EVM signer created the durable join checkpoint.
    #[error("durable tee join was created with a different global --private-key")]
    DurableCallerMismatch,
    /// The permanent offer key in the enclave differs from the registry.
    #[error("resident permanent offer key does not match finalized TeeRegistry")]
    OfferKeyMismatch,
    /// A finalized committed binding has no durable pre-relay record.
    #[error("finalized committed binding has no durable pre-relay transaction checkpoint")]
    MissingCommittedRelayCheckpoint,
    /// Enclave transport error during offer key classification.
    #[error("{0}")]
    Transport(#[from] outbe_tee::TransportError),
    /// The enclave returned an unexpected response to `GetPublicKeys`.
    #[error("expected enclave PublicKeys, got {0:?}")]
    UnexpectedPublicKeysResponse(Box<outbe_tee::protocol::EnclaveResponse>),
    /// `DcapRequired` mode requires `--node-data-dir`.
    #[error("DcapRequired tee join requires --node-data-dir")]
    DcapRequiresNodeDataDir,
    /// The existing finalized enclave lease has not expired yet.
    #[error("finalized enclave lease is live; use `outbe-cli tee renew`")]
    FinalizedLeaseLive,
    /// Incrementing the binding version counter overflowed.
    #[error("binding version exhausted")]
    BindingVersionExhausted,
    /// Incrementing the registration version counter overflowed.
    #[error("registration version exhausted")]
    RegistrationVersionExhausted,
    /// Hashing the node identity for the registration intent failed.
    #[error("hash registration node identity: {0}")]
    HashRegistrationNodeIdentity(outbe_primitives::tee_attestation_v1::CodecError),
    /// Hashing the registration intent failed.
    #[error("hash registration intent: {0}")]
    HashRegistrationIntent(outbe_primitives::tee_attestation_v1::CodecError),
    /// Hashing the node identity in the committed manifest failed.
    #[error("hash committed node identity: {0}")]
    HashCommittedNodeIdentity(outbe_primitives::tee_attestation_v1::CodecError),
    /// Deriving the enclave identity from the committed manifest failed.
    #[error("derive committed enclave identity: {0}")]
    DeriveCommittedEnclaveIdentity(outbe_primitives::tee_attestation_v1::CodecError),
    /// Deriving the authorization hash from the committed manifest failed.
    #[error("derive committed NodeHost authorization: {0}")]
    DeriveCommittedAuthorization(outbe_primitives::tee_attestation_v1::CodecError),
    /// No approved successor TEE policy exists on-chain.
    #[error("no approved successor TEE policy")]
    NoApprovedSuccessorPolicy,
    /// The join view and the successor policy view read different finalized blocks.
    #[error("join and successor policy use different finalized heads")]
    SuccessorPolicyHeadMismatch,
    /// The chain identifier in the finalized V1 policy differs from `eth_chainId`.
    #[error("finalized V1 policy chain id does not match eth_chainId")]
    PolicyChainIdMismatch,
    /// The finalized registry lookup returned a binding for another node.
    #[error("finalized Registry selector returned another node identity")]
    SelectorNodeIdentityMismatch,
    /// The global `--private-key` signer does not own the finalized association.
    #[error("global --private-key does not own this finalized NodeHost association")]
    SignerDoesNotOwnAssociation,
    /// Inspecting the candidate join checkpoint on disk failed.
    #[error("inspect candidate join checkpoint: {0}")]
    InspectCandidateCheckpoint(outbe_tee::TransportError),
    /// Inspecting the committed join checkpoint on disk failed.
    #[error("inspect committed join checkpoint: {0}")]
    InspectCommittedCheckpoint(outbe_tee::TransportError),
    /// Both candidate and committed join checkpoints exist simultaneously.
    #[error("candidate and committed tee join checkpoints coexist")]
    CoexistingCheckpoints,
    /// Decoding the durable evidence from the join checkpoint failed.
    #[error("decode durable join checkpoint: {0}")]
    DecodeDurableCheckpoint(outbe_primitives::tee_attestation_v1::CodecError),
    /// The requested `--valid-until` timestamp is not after finalized time.
    #[error("--valid-until is not after finalized consensus time")]
    ValidUntilBeforeFinalizedTime,
    /// The requested lease duration is outside the policy range.
    #[error("--valid-until lease {lease}s is outside finalized policy range {min}..={max}s")]
    LeaseOutsidePolicyRange { lease: u64, min: u64, max: u64 },
    /// Loading the committed manifest for replay verification failed.
    #[error("load committed replay manifest: {0}")]
    LoadReplayManifest(outbe_tee::TransportError),
    /// Reopening the completed join enclave failed.
    #[error("reopen completed tee join: {0}")]
    ReopenCompletedJoin(outbe_tee::TransportError),
    /// The committed enclave is not durably ready during exact replay.
    #[error("finalized tee join is not durably ready in the committed enclave: {0}")]
    ReplayEnclaveNotReady(String),
    /// The hash of the raw relay transaction does not match its recorded hash.
    #[error("durable join relay transaction hash mismatch")]
    RelayTxHashMismatch,
    /// The RPC server returned a different hash for the durable join transaction.
    #[error("RPC returned a different durable join transaction hash")]
    ReturnedTxHashMismatch,
    /// A competing live join binding finalized in the registry.
    #[error("finalized Registry contains a competing live join binding")]
    CompetingLiveBinding,
    /// Waiting for the exact join binding to finalize timed out.
    #[error("timed out after {0}s waiting for exact finalized tee join binding")]
    FinalizedJoinTimeout(u64),
    /// The derived address of the relay signer differs from the EVM signer address.
    #[error("global EVM signer address is inconsistent")]
    InconsistentSignerAddress,
    /// The EVM signer balance is below the required gas cost for joining.
    #[error("TEE join EVM signer {signer} has {balance} but needs at least {required}")]
    InsufficientJoinBalance {
        signer: Address,
        balance: U256,
        required: U256,
    },
    /// Loading the durable relay record for the candidate failed.
    #[error("load durable candidate relay: {0}")]
    LoadCandidateRelay(outbe_tee::TransportError),
    /// The calldata hash of the durable candidate transaction differs from current calldata.
    #[error("durable candidate transaction targets different calldata")]
    CandidateRelayCalldataMismatch,
    /// A finalized candidate binding has no durable pre-relay record.
    #[error("finalized candidate binding has no durable pre-relay transaction checkpoint")]
    MissingCandidateRelayCheckpoint,
    /// Persisting the signed relay transaction for the candidate failed.
    #[error("persist exact candidate transaction: {0}")]
    PersistCandidateRelay(outbe_tee::TransportError),
    /// Loading the durable relay record for the committed join failed.
    #[error("load durable committed relay: {0}")]
    LoadCommittedRelay(outbe_tee::TransportError),
    /// The calldata hash of the durable committed transaction differs from current calldata.
    #[error("durable committed transaction targets different calldata")]
    CommittedRelayCalldataMismatch,
    /// Persisting the signed relay transaction for the committed join failed.
    #[error("persist exact committed transaction: {0}")]
    PersistCommittedRelay(outbe_tee::TransportError),
    /// Reopening the completed committed enclave during cleanup failed.
    #[error("reopen completed committed enclave: {0}")]
    ReopenCompletedCommittedEnclave(outbe_tee::TransportError),
    /// Cleanup recovery did not find the expected permanent offer key.
    #[error("cleanup recovery did not reopen the exact permanent offer key: {0}")]
    CleanupOfferKeyNotReady(String),
    /// Clearing the recovered committed join checkpoint failed.
    #[error("clear recovered committed join checkpoint: {0}")]
    ClearRecoveredCheckpoint(outbe_tee::TransportError),
}
