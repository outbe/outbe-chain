//! Outbe RPC server implementation.

mod tee;
mod tribute_sources;

use alloy_primitives::{Address, Bytes, B256, U256};
use jsonrpsee::core::RpcResult;
use outbe_primitives::header::OutbeHeader;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_primitives::{
    consensus::{
        ConsensusExecutionBridge, ConsensusOperatingMode, FinalizationOutcome, FinalizedBlockBytes,
        FINALIZATION_REQUEST_TIMEOUT,
    },
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};
use reth_ethereum::primitives::AlloyBlockHeader as _;
use reth_ethereum::storage::{
    BlockNumReader, HeaderProvider, StateProvider as _, StateProviderBox, StateProviderFactory,
};
use reth_provider::BlockIdReader;
use std::sync::Arc;

use crate::errors::{RpcFinalizationLimitError, RpcInternalError};

use crate::api::{
    ConsensusStatusInfo, EmissionInfo, EpochInfo, FinalizationProof, GratisKeysSealed,
    OutbeApiServer, ParticipationInfo, RadiclePeerInfo, RadiclePhaseErrorInfo, RadiclePhaseInfo,
    RadicleRepositoryInfo, RadicleRepositoryStateInfo, RadicleStatusInfo, SlashConfig, SlashInfo,
    SyncStatusInfo, ValidatorDetailInfo, ValidatorInfo,
};
use outbe_radicle::integration::{
    RadicleRepositoryState, RadicleStatusHandle, RadicleStatusSnapshot, RadicleVotingGate,
    RadicleVotingGateError,
};

fn read_effective_slash_config(
    storage: StorageHandle<'_>,
) -> Result<SlashConfig, outbe_primitives::error::PrecompileError> {
    let si = outbe_slashindicator::contract::SlashIndicator::new(storage);
    Ok(SlashConfig {
        proposer_misdemeanor_threshold: si.proposer_misdemeanor_threshold()?,
        proposer_felony_threshold: si.proposer_felony_threshold()?,
        voter_misdemeanor_threshold: si.voter_misdemeanor_threshold()?,
        voter_felony_threshold: si.voter_felony_threshold()?,
        slash_amount_percent: si.slash_amount_percent()?,
        evidence_reward_percent: si.evidence_reward_percent()?,
    })
}

/// Bridge from Reth's `StateProvider` to outbe's `StorageReader` trait.
struct RethStateReader<'a> {
    state: &'a StateProviderBox,
}

impl StorageReader for RethStateReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|opt| opt.unwrap_or(U256::ZERO))
            .map_err(|e| crate::errors::RpcStateReadError::StateReadFailed(e.to_string()).into())
    }
}

/// RPC handler for the `outbe_*` namespace.
#[derive(Clone)]
pub struct OutbeApiHandler<P> {
    provider: Arc<P>,
    chain_identity: Option<(u64, B256)>,
    bridge: Option<ConsensusExecutionBridge>,
    tee_renewal_schedule: Option<TeeRenewalScheduleConfigV1>,
    radicle_status: RadicleStatusHandle,
    tee_enclave_health: outbe_tee::TeeEnclaveHealthChannel,
}

#[derive(Clone, Copy, Debug)]
struct TeeRenewalScheduleConfigV1 {
    dkg_prepare_window_blocks: u64,
    minimum_block_time_millis: u64,
}

impl<P> std::fmt::Debug for OutbeApiHandler<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutbeApiHandler")
            .field(
                "operating_mode",
                &self
                    .bridge
                    .as_ref()
                    .map(ConsensusExecutionBridge::operating_mode)
                    .unwrap_or(ConsensusOperatingMode::Follower),
            )
            .finish_non_exhaustive()
    }
}

impl<P> OutbeApiHandler<P> {
    /// Create a new handler backed by the given state provider factory (no
    /// bridge, plain EL full node).
    pub fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            chain_identity: None,
            bridge: None,
            tee_renewal_schedule: None,
            radicle_status: outbe_radicle::integration::RadicleStatusChannel::disabled(),
            tee_enclave_health: outbe_tee::TeeEnclaveHealthChannel::disabled(),
        }
    }

    /// Create a handler with access to the consensus bridge.
    pub fn with_bridge(provider: Arc<P>, bridge: ConsensusExecutionBridge) -> Self {
        let mut handler = Self::new(provider);
        handler.bridge = Some(bridge);
        handler
    }

    #[must_use]
    pub fn with_chain_identity(mut self, chain_id: u64, genesis_hash: B256) -> Self {
        self.chain_identity = Some((chain_id, genesis_hash));
        self
    }

    #[must_use]
    pub fn with_tee_renewal_schedule(
        mut self,
        dkg_prepare_window_blocks: u64,
        minimum_block_time_millis: u64,
    ) -> Self {
        self.tee_renewal_schedule = Some(TeeRenewalScheduleConfigV1 {
            dkg_prepare_window_blocks,
            minimum_block_time_millis,
        });
        self
    }

    #[must_use]
    pub fn with_radicle_status(mut self, status: RadicleStatusHandle) -> Self {
        self.radicle_status = status;
        self
    }

    #[must_use]
    pub fn with_tee_enclave_health(mut self, channel: outbe_tee::TeeEnclaveHealthChannel) -> Self {
        self.tee_enclave_health = channel;
        self
    }

    pub(crate) fn sync_status_info(&self) -> SyncStatusInfo {
        match &self.bridge {
            Some(b) => {
                let consensus = b.consensus_status();
                SyncStatusInfo {
                    is_syncing: !consensus.is_active(),
                    current_block: consensus.last_finalized_block,
                    highest_block: consensus.last_finalized_block,
                    consensus_active: consensus.is_active(),
                    connected_peers: consensus.connected_peers,
                }
            }
            None => {
                // Full-node mode: DevP2P handles sync (eth_syncing).
                // Report not syncing because this node has no consensus bridge.
                SyncStatusInfo {
                    is_syncing: false,
                    current_block: 0,
                    highest_block: 0,
                    consensus_active: false,
                    connected_peers: 0,
                }
            }
        }
    }

    pub(crate) async fn derive_keys_checked(
        &self,
        ledger: outbe_tee::protocol::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: alloy_primitives::Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

        // Prove that the caller controls `account` before the enclave derives its
        // (secret) modify key. Recover the EIP-191 personal_sign signer over
        // `"outbe/<ledger>/derive-keys/v1" || account || ephemeralPubkey`. Require
        // the signer to equal `account`.
        let sig65: [u8; 65] = signature.as_ref().try_into().map_err(|_| {
            invalid_params_err(
                crate::errors::RpcInvalidParamsError::InvalidSignatureLength(signature.len()),
            )
        })?;
        // Recover the signer to reject unauthorized requests before the enclave round-trip.
        // This adds defense in depth. The enclave also verifies the signature.
        // A compromised host could bypass this check and reach the enclave transport directly.
        // See the DeriveAccountKeys arm.
        let prehash = outbe_tee::protocol::eip191_hash(
            &outbe_tee::protocol::derive_account_keys_message(ledger, account, ephemeral_pubkey),
        );
        let recovered = outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65)
            .map_err(|e| {
                invalid_params_err(
                    crate::errors::RpcInvalidParamsError::signature_recovery_failed(e),
                )
            })?;
        if recovered != account {
            return Err(invalid_params_err(
                crate::errors::RpcInvalidParamsError::SignatureSignerMismatch {
                    recovered,
                    account,
                },
            ));
        }

        // Off-chain key delivery through the process-global enclave client (no state).
        // Pass `owner_sig` to the enclave so that the enclave enforces ownership in its
        // own trust domain, not ours.
        let response = outbe_tee::try_account_query(&EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey: ephemeral_pubkey.0,
            owner_sig: sig65.to_vec(),
        })
        .ok_or_else(|| internal_err(tee::PrivateExecutionUnavailable))?
        .map_err(|e| {
            internal_err(crate::errors::RpcInternalError::enclave_derive_account_keys_failed(e))
        })?;
        match response {
            EnclaveResponse::AccountKeysSealed {
                sealed,
                nonce,
                enclave_ephemeral_pubkey,
                ..
            } => Ok(GratisKeysSealed {
                sealed: sealed.into(),
                nonce: nonce.to_vec().into(),
                enclave_ephemeral_pubkey: B256::from(enclave_ephemeral_pubkey),
            }),
            EnclaveResponse::Error { message } => Err(internal_err(
                crate::errors::RpcInternalError::EnclaveError(message),
            )),
            other => Err(internal_err(
                crate::errors::RpcInternalError::unexpected_enclave_response(other),
            )),
        }
    }

    pub(crate) async fn derive_gratis_keys_checked(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: alloy_primitives::Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_keys_checked(
            outbe_tee::protocol::Ledger::Gratis,
            account,
            ephemeral_pubkey,
            signature,
        )
        .await
    }

    /// Returns the bridge when it can serve a finalization for `height`.
    ///
    /// Rejects a height above the local finalized height with invalid params.
    /// This check occurs before any request enters the bridge queue, so a
    /// request for a height that cannot exist costs no marshal lookup.
    fn finalization_bridge(
        &self,
        height: u64,
        not_serving: RpcInternalError,
    ) -> RpcResult<&ConsensusExecutionBridge> {
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| internal_err(not_serving))?;
        let finalized = bridge.consensus_status().last_finalized_block;
        if height > finalized {
            return Err(invalid_params_err(
                RpcFinalizationLimitError::HeightAboveFinalized { height, finalized },
            ));
        }
        Ok(bridge)
    }

    pub(crate) async fn finalization_for_height(
        &self,
        height: u64,
    ) -> RpcResult<FinalizationProof> {
        // Only nodes running consensus (or a follower that has itself synced the
        // height) can serve this. Both install a finalization fetcher on the
        // bridge at marshal-start. A node without a bridge (pure EL full node)
        // has no marshal and cannot answer.
        let bridge =
            self.finalization_bridge(height, RpcInternalError::NotServingConsensusFinalizations)?;
        let proof = found_or_err(bridge.request_finalization(height).await, || {
            RpcInternalError::NoFinalizationAvailable(height)
        })?;
        if proof.finalization.is_empty() {
            return Err(internal_err(
                RpcInternalError::NoDirectFinalizationAvailable(height),
            ));
        }
        Ok(FinalizationProof {
            finalization_hex: format!("0x{}", hex::encode(&proof.finalization)),
            block_hex: format!("0x{}", hex::encode(&proof.block)),
        })
    }

    pub(crate) async fn consensus_block_for_height(&self, height: u64) -> RpcResult<Bytes> {
        let bridge =
            self.finalization_bridge(height, RpcInternalError::NotServingConsensusBlocks)?;
        found_or_err(bridge.request_finalization(height).await, || {
            RpcInternalError::NoFinalizedConsensusBlockAvailable(height)
        })
        .map(|proof| proof.block)
    }

    /// Serves `outbe_getFinalityProof`. The height check applies to the
    /// starting height only. The whole walk has one deadline of
    /// [`FINALIZATION_REQUEST_TIMEOUT`].
    pub(crate) async fn finality_proof_for_height(
        &self,
        height: u64,
    ) -> RpcResult<crate::api::AncestorFinalizationProof> {
        let bridge =
            self.finalization_bridge(height, RpcInternalError::NotServingConsensusFinalizations)?;
        tokio::time::timeout(
            FINALIZATION_REQUEST_TIMEOUT,
            walk_to_certified_descendant(bridge, height),
        )
        .await
        .unwrap_or_else(|_| Err(resource_exhausted_err(RpcFinalizationLimitError::TimedOut)))
    }
}

/// Walks up to 64 blocks above `height` until it finds a block with a direct
/// certificate. A Busy or TimedOut outcome stops the walk at once.
async fn walk_to_certified_descendant(
    bridge: &ConsensusExecutionBridge,
    height: u64,
) -> RpcResult<crate::api::AncestorFinalizationProof> {
    let mut ancestors = Vec::new();
    let mut total_bytes = 0usize;
    for offset in 0..=64u64 {
        let next = height
            .checked_add(offset)
            .ok_or_else(|| internal_err(RpcInternalError::ProofHeightOverflow))?;
        let proof = found_or_err(bridge.request_finalization(next).await, || {
            RpcInternalError::NoFinalityEvidenceAvailable(next)
        })?;
        total_bytes = total_bytes.saturating_add(proof.block.len());
        // Hex JSON doubles the block payload. Stay below the default
        // upstream HTTP client's 10 MiB response bound with envelope room.
        if total_bytes > 4 * 1024 * 1024 {
            return Err(internal_err(
                RpcInternalError::AncestorFinalityProofExceedsByteLimit,
            ));
        }
        if !proof.finalization.is_empty() {
            return Ok(crate::api::AncestorFinalizationProof {
                certified: FinalizationProof {
                    finalization_hex: format!("0x{}", hex::encode(proof.finalization)),
                    block_hex: format!("0x{}", hex::encode(proof.block)),
                },
                ancestor_blocks_hex: ancestors,
            });
        }
        ancestors.push(format!("0x{}", hex::encode(proof.block)));
    }
    Err(internal_err(
        RpcInternalError::NoDescendantFinalizationWithin64Blocks,
    ))
}

/// Maps a bridge outcome to the finalized bytes or to an RPC error. Busy and
/// TimedOut map to a retryable resource error. NotFound maps to the internal
/// error that `not_found` supplies.
fn found_or_err(
    outcome: FinalizationOutcome,
    not_found: impl FnOnce() -> RpcInternalError,
) -> RpcResult<FinalizedBlockBytes> {
    match outcome {
        FinalizationOutcome::Found(proof) => Ok(proof),
        FinalizationOutcome::NotFound => Err(internal_err(not_found())),
        FinalizationOutcome::Busy => Err(resource_exhausted_err(RpcFinalizationLimitError::Busy)),
        FinalizationOutcome::TimedOut => {
            Err(resource_exhausted_err(RpcFinalizationLimitError::TimedOut))
        }
    }
}

/// Map the canary snapshot into the RPC shape. The function is pure so that unit
/// tests can test it. The caller supplies `now_unix_ms` to keep assertions
/// wall-clock-free.
pub(crate) fn enclave_health_info(
    snapshot: &outbe_tee::TeeEnclaveHealthSnapshot,
    now_unix_ms: u64,
) -> crate::api::EnclaveHealthInfo {
    use crate::api::EnclaveHealth;
    use outbe_tee::TeeEnclaveHealthState;
    let state = match snapshot.state {
        TeeEnclaveHealthState::Disabled => EnclaveHealth::Disabled,
        TeeEnclaveHealthState::Starting => EnclaveHealth::Starting,
        TeeEnclaveHealthState::Ready => EnclaveHealth::Ready,
        TeeEnclaveHealthState::Degraded => EnclaveHealth::Degraded,
        TeeEnclaveHealthState::Unavailable => EnclaveHealth::Unavailable,
    };
    crate::api::EnclaveHealthInfo {
        state,
        ready: matches!(state, EnclaveHealth::Ready),
        offer_key_ready: snapshot.offer_key_ready,
        last_ok_ago_millis: snapshot
            .last_ok_unix_ms
            .map(|ok_ms| now_unix_ms.saturating_sub(ok_ms)),
        last_canary_latency_ms: snapshot.last_canary_latency_ms,
        consecutive_failures: snapshot.consecutive_failures,
        last_failure_class: snapshot.last_failure.clone(),
        uptime_s: snapshot.enclave.as_ref().map(|status| status.uptime_s),
        heap_current_bytes: snapshot
            .enclave
            .as_ref()
            .map(|status| status.heap.current_bytes),
        heap_peak_bytes: snapshot
            .enclave
            .as_ref()
            .map(|status| status.heap.peak_bytes),
        health_probe_supported: snapshot.health_probe_supported,
    }
}

pub(crate) fn radicle_status_info(
    snapshot: &RadicleStatusSnapshot,
) -> Result<RadicleStatusInfo, crate::errors::RpcError> {
    let manager = &snapshot.manager;
    let phase = match manager.phase {
        outbe_radicle::manager::ManagerPhase::Disabled => RadiclePhaseInfo::Disabled,
        outbe_radicle::manager::ManagerPhase::JoiningUnbound => RadiclePhaseInfo::JoiningUnbound,
        outbe_radicle::manager::ManagerPhase::Ready => RadiclePhaseInfo::Ready,
        outbe_radicle::manager::ManagerPhase::RuntimeDegraded => RadiclePhaseInfo::RuntimeDegraded,
    };
    let phase_error = match snapshot.voting_gate {
        RadicleVotingGate::Fatal(error) => Some(match error {
            RadicleVotingGateError::SidecarUnavailable => RadiclePhaseErrorInfo::SidecarUnavailable,
            RadicleVotingGateError::LocalNodeIdUnavailable => {
                RadiclePhaseErrorInfo::LocalNodeIdUnavailable
            }
            RadicleVotingGateError::BindingMismatch => RadiclePhaseErrorInfo::BindingMismatch,
            RadicleVotingGateError::ActiveBindingMissing => {
                RadiclePhaseErrorInfo::ActiveBindingMissing
            }
        }),
        _ => manager.phase_error.map(|error| match error {
            outbe_radicle::manager::PhaseError::SidecarUnavailable => {
                RadiclePhaseErrorInfo::SidecarUnavailable
            }
            outbe_radicle::manager::PhaseError::LocalNodeIdUnavailable => {
                RadiclePhaseErrorInfo::LocalNodeIdUnavailable
            }
            outbe_radicle::manager::PhaseError::BindingMismatch => {
                RadiclePhaseErrorInfo::BindingMismatch
            }
        }),
    };
    let count = |value: usize| {
        u64::try_from(value).map_err(|_| crate::errors::RpcError::RadicleCounterOverflow)
    };
    Ok(RadicleStatusInfo {
        phase,
        phase_error,
        local_node_id: snapshot.local_node_id.map(B256::from),
        last_seen_finalized_number: manager.last_seen_finalized.map(|block| block.number),
        last_seen_finalized_hash: manager.last_seen_finalized.map(|block| block.hash),
        last_converged_finalized_number: manager.last_converged_finalized.map(|block| block.number),
        last_converged_finalized_hash: manager.last_converged_finalized.map(|block| block.hash),
        desired_repository_count: count(manager.desired_repository_count)?,
        available_repository_count: count(manager.available_repository_count)?,
        pending_repository_count: count(manager.pending_repository_count)?,
        resolved_peer_count: count(manager.resolved_peer_count)?,
        unresolved_peer_count: count(manager.unresolved_peer_count)?,
        connected_peer_count: count(manager.connected_peer_count)?,
        finality_regressions: manager.finality_regressions,
        finality_conflicts: manager.finality_conflicts,
        provider_failures: manager.provider_failures,
        endpoint_failures: manager.endpoint_failures,
        uds_failures: manager.uds_failures,
        tcp_status_failures: manager.tcp_status_failures,
    })
}

fn radicle_peer_info(
    proof: &outbe_radicle::integration::SignedEndpointEvidence,
) -> RadiclePeerInfo {
    let body = proof.response.body();
    RadiclePeerInfo {
        validator: body.validator,
        sender_bls_public_key: proof.peer.as_bytes().to_vec().into(),
        request_id: B256::from(body.request_id),
        chain_id: body.chain_id,
        genesis_hash: body.genesis_hash,
        node_id: B256::from(body.node_id),
        addresses: body.addresses.iter().map(canonical_endpoint).collect(),
        anchor_number: body.anchor_number,
        anchor_hash: body.anchor_hash,
        valid_until_height: body.valid_until,
        signature: proof.response.signature().to_vec().into(),
        encoded_frame: proof.encoded_frame.clone().into(),
    }
}

fn canonical_endpoint(address: &outbe_radicle::endpoint::EndpointAddress) -> String {
    let encoded = address.encode();
    match encoded[0] {
        0 => format!(
            "{}:{}",
            std::net::Ipv4Addr::new(encoded[1], encoded[2], encoded[3], encoded[4]),
            u16::from_be_bytes([encoded[5], encoded[6]])
        ),
        1 => {
            let octets: [u8; 16] = encoded[1..17].try_into().expect("canonical IPv6 endpoint");
            format!(
                "[{}]:{}",
                std::net::Ipv6Addr::from(octets),
                u16::from_be_bytes([encoded[17], encoded[18]])
            )
        }
        2 => {
            let length = usize::from(encoded[1]);
            let host = std::str::from_utf8(&encoded[2..2 + length])
                .expect("canonical DNS endpoint is ASCII");
            let offset = 2 + length;
            format!(
                "{}:{}",
                host,
                u16::from_be_bytes([encoded[offset], encoded[offset + 1]])
            )
        }
        _ => unreachable!("EndpointAddress has a canonical tag"),
    }
}

impl<P> OutbeApiHandler<P>
where
    P: StateProviderFactory + HeaderProvider<Header = OutbeHeader> + Send + Sync + 'static,
{
    /// Read precompile state at the latest block using a closure.
    fn with_latest_state<R>(
        &self,
        f: impl FnOnce(StorageHandle) -> Result<R, outbe_primitives::error::PrecompileError>,
    ) -> RpcResult<R> {
        let state = self.provider.latest().map_err(|e| {
            internal_err(crate::errors::RpcInternalError::latest_state_read_failed(e))
        })?;

        let reader = RethStateReader { state: &state };
        let mut provider = ReadOnlyStorageProvider::new(reader);
        let storage = StorageHandle::new(&mut provider);

        f(storage).map_err(|e| internal_err(crate::errors::RpcInternalError::Precompile(e)))
    }
}

#[jsonrpsee::core::async_trait]
impl<P> OutbeApiServer for OutbeApiHandler<P>
where
    P: StateProviderFactory
        + HeaderProvider<Header = OutbeHeader>
        + BlockNumReader
        + BlockIdReader
        + Send
        + Sync
        + 'static,
{
    async fn get_tribute_state_source(
        &self,
        subject: outbe_tee::tribute_source::TributeSourceSubject,
        tribute_id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> RpcResult<outbe_tee::tribute_source::TributeStateSource> {
        let provider = Arc::clone(&self.provider);
        let chain_identity = self.chain_identity;
        tokio::task::spawn_blocking(move || {
            tribute_sources::serve(&*provider, chain_identity, subject, tribute_id)
        })
        .await
        .map_err(|error| internal_err(format!("Tribute source worker failed: {error}")))?
    }

    async fn derive_keys(
        &self,
        ledger: outbe_tee::protocol::Ledger,
        account: Address,
        ephemeral_pubkey: B256,
        signature: alloy_primitives::Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_keys_checked(ledger, account, ephemeral_pubkey, signature)
            .await
    }

    async fn derive_gratis_keys(
        &self,
        account: Address,
        ephemeral_pubkey: B256,
        signature: alloy_primitives::Bytes,
    ) -> RpcResult<GratisKeysSealed> {
        self.derive_gratis_keys_checked(account, ephemeral_pubkey, signature)
            .await
    }

    async fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>> {
        self.with_latest_state(|storage| {
            let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let records = vs.get_active_validators()?;

            let staking = outbe_staking::contract::Staking::new(storage);

            let mut result = Vec::with_capacity(records.len());
            for r in &records {
                let stake = staking.get_stake(r.validator_address).unwrap_or(U256::ZERO);
                result.push(ValidatorInfo {
                    address: r.validator_address,
                    consensus_pubkey: hex::encode(r.consensus_pubkey),
                    status: r.status,
                    stake,
                });
            }
            Ok(result)
        })
    }

    async fn get_validator(&self, address: Address) -> RpcResult<Option<ValidatorDetailInfo>> {
        self.with_latest_state(|storage| {
            let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let state = vs.validator_state(address)?;
            if !state.is_registered() {
                return Ok(None);
            }
            let consensus_pubkey = state
                .consensus_pubkey()
                .ok_or(crate::errors::RpcValidatorStateError::MissingConsensusPubkey)?;
            let history = state
                .history()
                .ok_or(crate::errors::RpcValidatorStateError::MissingHistory)?;
            Ok(Some(ValidatorDetailInfo {
                address,
                consensus_pubkey: hex::encode(consensus_pubkey),
                status: state
                    .lifecycle()
                    .stored_status()
                    .ok_or(crate::errors::RpcValidatorStateError::MissingPersistedStatus)?,
                stake: state.bonded_stake(),
                slash_count: history.slash_count(),
                missed_blocks: history.missed_blocks(),
                missed_votes: history.missed_votes(),
                blocks_proposed: history.blocks_proposed(),
                joined_at_height: history.joined_at_height(),
                deactivated_at_height: history.last_deactivated_at_height().unwrap_or(0),
                unbonding_end: state.unbonding_end_hint().unwrap_or(0),
                has_bls_share: state.has_bls_share(),
            }))
        })
    }

    async fn get_epoch_info(&self) -> RpcResult<EpochInfo> {
        self.with_latest_state(|storage| {
            let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            let epoch = vs.epoch_snapshot()?;
            let active_count = vs.active_validator_count()?;

            let staking = outbe_staking::contract::Staking::new(storage);
            let total_staked = staking.get_total_staked()?;

            Ok(EpochInfo {
                epoch_number: epoch.number,
                epoch_start_timestamp: epoch.start_timestamp,
                epoch_start_block: epoch.start_block,
                epoch_length_blocks: epoch.length_blocks,
                active_validator_count: active_count,
                total_staked,
            })
        })
    }

    async fn upgrade_key_v1(
        &self,
        context: Bytes,
        proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
        legacy_direct_dev_source: bool,
    ) -> RpcResult<Bytes> {
        tee::TeeRpc::new(&*self.provider, self.chain_identity)
            .upgrade_key(context, proof, legacy_direct_dev_source)
            .await
    }

    async fn tee_renewal_schedule_v1(&self) -> RpcResult<TeeRenewalScheduleV1> {
        tee::TeeRpc::new(&*self.provider, self.chain_identity)
            .renewal_schedule(self.tee_renewal_schedule)
    }

    async fn get_stake(&self, address: Address) -> RpcResult<U256> {
        self.with_latest_state(|storage| {
            let staking = outbe_staking::contract::Staking::new(storage);
            staking.get_stake(address)
        })
    }

    async fn get_slash_info(&self, address: Address) -> RpcResult<SlashInfo> {
        self.with_latest_state(|storage| {
            let si = outbe_slashindicator::contract::SlashIndicator::new(storage);
            Ok(SlashInfo {
                proposer_miss_count: si.proposer_miss_count.read(&address)?,
                voter_miss_count: si.voter_miss_count.read(&address)?,
                felony_count: si.felony_count.read(&address)?,
            })
        })
    }

    async fn consensus_status(&self) -> RpcResult<ConsensusStatusInfo> {
        let mode = self
            .bridge
            .as_ref()
            .map(ConsensusExecutionBridge::operating_mode)
            .unwrap_or(ConsensusOperatingMode::Follower);
        let is_validator = mode.is_validator();
        let (status, has_threshold_shares) = self
            .bridge
            .as_ref()
            .map(ConsensusExecutionBridge::consensus_status_with_threshold_shares)
            .unwrap_or_default();

        Ok(ConsensusStatusInfo {
            current_view: status.current_view,
            connected_peers: status.connected_peers,
            is_active: status.is_active(),
            has_threshold_shares,
            last_finalized_block: status.last_finalized_block,
            last_vrf_seed: status.last_vrf_seed,
            randomness_status: status.randomness_status,
            vrf_material_version: status.vrf_material_version,
            last_dkg_activation_height: status.last_dkg_activation_height,
            next_planned_activation_height: status.next_planned_activation_height,
            vrf_expiry_height: status.vrf_expiry_height,
            is_validator,
            mode,
            enclave: enclave_health_info(
                &self.tee_enclave_health.snapshot(),
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
                    .unwrap_or(0),
            ),
        })
    }

    async fn get_vrf_seed(&self, block_number: Option<u64>) -> RpcResult<Option<B256>> {
        // Read the committed VRF seed from the target block header's `mixHash`
        // (prev_randao) through the provider. Honor `block_number`. This is the
        // authoritative committed value, and it is consistent across nodes. It is not
        // the process-local in-memory consensus seed. A full node never has that
        // seed, and that seed can diverge between nodes. `None` resolves to the
        // latest canonical block. Under Outbe's fast finality, that is the latest
        // finalized block.
        let target = match block_number {
            Some(n) => n,
            None => self.provider.best_block_number().map_err(|e| {
                internal_err(crate::errors::RpcInternalError::latest_block_number_read_failed(e))
            })?,
        };
        let header = self.provider.header_by_number(target).map_err(|e| {
            internal_err(crate::errors::RpcInternalError::block_header_read_failed(
                target, e,
            ))
        })?;
        // `mix_hash()` is itself `Option<B256>`. A missing block also yields None.
        Ok(header.and_then(|h| h.mix_hash()))
    }

    async fn get_emission_info(&self) -> RpcResult<EmissionInfo> {
        Ok(EmissionInfo {
            validator_reward_percent: outbe_emissionlimit::allocation::VALIDATOR_REWARD_PCT,
            fee_escrow_address: outbe_primitives::addresses::REWARDS_ADDRESS,
        })
    }

    async fn get_slash_config(&self) -> RpcResult<SlashConfig> {
        self.with_latest_state(read_effective_slash_config)
    }

    async fn get_participation(&self, address: Address) -> RpcResult<ParticipationInfo> {
        self.with_latest_state(|storage| {
            let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
            let participation = vs.participation(address)?;
            Ok(ParticipationInfo {
                address,
                blocks_proposed: participation.blocks_proposed,
                missed_blocks: participation.missed_blocks,
                missed_votes: participation.missed_votes,
            })
        })
    }

    async fn sync_status(&self) -> RpcResult<SyncStatusInfo> {
        Ok(self.sync_status_info())
    }

    async fn radicle_status(&self) -> RpcResult<RadicleStatusInfo> {
        radicle_status_info(&self.radicle_status.snapshot()).map_err(internal_err)
    }

    async fn radicle_peers(&self) -> RpcResult<Vec<RadiclePeerInfo>> {
        Ok(self
            .radicle_status
            .snapshot()
            .signed_peers
            .iter()
            .map(radicle_peer_info)
            .collect())
    }

    async fn radicle_repositories(&self) -> RpcResult<Vec<RadicleRepositoryInfo>> {
        Ok(self
            .radicle_status
            .snapshot()
            .repositories
            .iter()
            .map(|repository| RadicleRepositoryInfo {
                repo_id: hex::encode(repository.repo_id),
                state: match repository.state {
                    RadicleRepositoryState::Available => RadicleRepositoryStateInfo::Available,
                    RadicleRepositoryState::Pending => RadicleRepositoryStateInfo::Pending,
                },
            })
            .collect())
    }

    async fn get_finalization(&self, height: u64) -> RpcResult<FinalizationProof> {
        self.finalization_for_height(height).await
    }

    async fn get_consensus_block(&self, height: u64) -> RpcResult<Bytes> {
        self.consensus_block_for_height(height).await
    }

    async fn get_finality_proof(
        &self,
        height: u64,
    ) -> RpcResult<crate::api::AncestorFinalizationProof> {
        self.finality_proof_for_height(height).await
    }
}

/// Create an internal JSON-RPC error.
fn internal_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(
        jsonrpsee::types::error::INTERNAL_ERROR_CODE,
        msg.to_string(),
        None::<()>,
    )
}

/// JSON-RPC error code for "limit exceeded" (EIP-1474).
const RESOURCE_EXHAUSTED_CODE: i32 = -32005;

/// Create a retryable "limit exceeded" JSON-RPC error.
fn resource_exhausted_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(RESOURCE_EXHAUSTED_CODE, msg.to_string(), None::<()>)
}

fn invalid_params(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(
        jsonrpsee::types::error::INVALID_PARAMS_CODE,
        msg.to_string(),
        None::<()>,
    )
}

fn invalid_params_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    invalid_params(msg)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes, B256};
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_radicle::{
        endpoint::{sign_response, EndpointAddress, EndpointFrame, EndpointResponseBody, PeerId},
        integration::{RadicleStatusChannel, SignedEndpointEvidence},
    };

    use super::{radicle_peer_info, radicle_status_info, read_effective_slash_config};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_slashindicator::constants::{
        DEFAULT_EVIDENCE_REWARD_PERCENT, DEFAULT_PROPOSER_FELONY_THRESHOLD,
        DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD, DEFAULT_SLASH_AMOUNT_PERCENT,
        DEFAULT_VOTER_FELONY_THRESHOLD, DEFAULT_VOTER_MISDEMEANOR_THRESHOLD,
    };

    #[test]
    fn a_follower_bridge_does_not_make_the_handler_a_validator() {
        use outbe_primitives::consensus::{ConsensusExecutionBridge, ConsensusOperatingMode};
        let provider = std::sync::Arc::new(());
        let plain = super::OutbeApiHandler::new(provider.clone());
        let validator_bridge = ConsensusExecutionBridge::new();
        validator_bridge.set_operating_mode(ConsensusOperatingMode::Signer);
        let validator = super::OutbeApiHandler::with_bridge(provider.clone(), validator_bridge);
        let follower =
            super::OutbeApiHandler::with_bridge(provider, ConsensusExecutionBridge::new());
        assert!(plain.bridge.is_none());
        assert!(validator
            .bridge
            .as_ref()
            .unwrap()
            .operating_mode()
            .is_validator());
        assert!(!follower
            .bridge
            .as_ref()
            .unwrap()
            .operating_mode()
            .is_validator());
    }

    #[test]
    fn slash_config_rpc_reports_runtime_defaults_for_zero_storage() {
        let mut provider = HashMapStorageProvider::new(1);
        let config = StorageHandle::enter(&mut provider, |storage| {
            read_effective_slash_config(storage).expect("read effective slash config")
        });

        assert_eq!(
            config.proposer_misdemeanor_threshold,
            DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD
        );
        assert_eq!(
            config.proposer_felony_threshold,
            DEFAULT_PROPOSER_FELONY_THRESHOLD
        );
        assert_eq!(
            config.voter_misdemeanor_threshold,
            DEFAULT_VOTER_MISDEMEANOR_THRESHOLD
        );
        assert_eq!(
            config.voter_felony_threshold,
            DEFAULT_VOTER_FELONY_THRESHOLD
        );
        assert_eq!(config.slash_amount_percent, DEFAULT_SLASH_AMOUNT_PERCENT);
        assert_eq!(
            config.evidence_reward_percent,
            DEFAULT_EVIDENCE_REWARD_PERCENT
        );
    }

    #[test]
    fn radicle_rpc_reads_immutable_status() {
        let status = radicle_status_info(&RadicleStatusChannel::disabled().snapshot()).unwrap();
        assert_eq!(status.phase, crate::api::RadiclePhaseInfo::Disabled);
        assert!(status.local_node_id.is_none());
        assert_eq!(status.desired_repository_count, 0);
    }

    #[test]
    fn enclave_health_defaults_to_disabled() {
        let info = super::enclave_health_info(
            &outbe_tee::TeeEnclaveHealthChannel::disabled().snapshot(),
            1_000_000,
        );
        assert_eq!(info.state, crate::api::EnclaveHealth::Disabled);
        assert!(!info.ready);
        assert!(info.last_ok_ago_millis.is_none());
        assert!(info.health_probe_supported.is_none());
    }

    #[test]
    fn enclave_health_maps_ready_snapshot() {
        let channel = outbe_tee::TeeEnclaveHealthChannel::disabled();
        channel.publish(outbe_tee::TeeEnclaveHealthSnapshot {
            state: outbe_tee::TeeEnclaveHealthState::Ready,
            last_ok_unix_ms: Some(900_000),
            last_canary_latency_ms: Some(4),
            consecutive_failures: 0,
            last_failure: None,
            offer_key_ready: true,
            health_probe_supported: Some(true),
            enclave: Some(outbe_tee::protocol::EnclaveHealthStatusV1 {
                uptime_s: 77,
                offer_key_ready: true,
                heap: outbe_tee::protocol::EnclaveHeapUsageV1 {
                    current_bytes: 1024,
                    peak_bytes: 4096,
                },
                requests: outbe_tee::protocol::EnclaveRequestCountsV1 {
                    total: 10,
                    errored: 0,
                    denied: 0,
                },
                classes: outbe_tee::protocol::EnclaveRequestClassCountsV1 {
                    initialized: 2,
                    founding_keyless: 0,
                    keyless_onboarding: 0,
                    ready: 8,
                    dev_source_seal: 0,
                    dev_recipient_ingest: 0,
                },
            }),
        });
        let info = super::enclave_health_info(&channel.snapshot(), 1_000_000);
        assert_eq!(info.state, crate::api::EnclaveHealth::Ready);
        assert!(info.ready);
        assert_eq!(info.last_ok_ago_millis, Some(100_000));
        assert_eq!(info.last_canary_latency_ms, Some(4));
        assert_eq!(info.uptime_s, Some(77));
        assert_eq!(info.heap_peak_bytes, Some(4096));
        // Stable JSON vocabulary (camelCase, lowercase state labels).
        let json = serde_json::to_string(&info).expect("serialize");
        assert!(json.contains("\"state\":\"ready\""), "json: {json}");
        assert!(json.contains("\"offerKeyReady\":true"), "json: {json}");
        assert!(
            json.contains("\"healthProbeSupported\":true"),
            "json: {json}"
        );
    }

    #[test]
    fn radicle_rpc_evidence() {
        let signer = bls12381::PrivateKey::from_seed(7);
        let body = EndpointResponseBody {
            request_id: [8_u8; 32],
            chain_id: 54_322_345,
            genesis_hash: B256::repeat_byte(0xaa),
            validator: alloy_primitives::Address::repeat_byte(0x11),
            node_id: [9_u8; 32],
            addresses: vec![EndpointAddress::dns("peer.example.com", 8776).unwrap()],
            anchor_number: 40,
            anchor_hash: B256::repeat_byte(0x40),
            valid_until: 80,
        };
        let response = sign_response(body, &signer).unwrap();
        let evidence = SignedEndpointEvidence {
            peer: PeerId::from_public_key(&signer.public_key()),
            encoded_frame: EndpointFrame::Response(Box::new(response.clone())).encode(),
            response,
        };
        let info = radicle_peer_info(&evidence);
        assert_eq!(info.sender_bls_public_key.len(), 48);
        assert_eq!(info.signature.len(), 96);
        assert_eq!(info.encoded_frame.as_ref(), evidence.encoded_frame);
        assert_eq!(info.addresses, ["peer.example.com:8776"]);
        assert_eq!(info.valid_until_height, 80);
    }

    #[tokio::test]
    async fn derive_keys_and_gratis_alias_reject_invalid_and_mismatched_signatures() {
        let handler = super::OutbeApiHandler::new(std::sync::Arc::new(()));
        let account = Address::repeat_byte(0x11);
        let ephemeral_pubkey = B256::repeat_byte(0x22);

        let short_err = handler
            .derive_keys_checked(
                outbe_tee::protocol::Ledger::Gratis,
                account,
                ephemeral_pubkey,
                Bytes::from_static(&[1, 2, 3]),
            )
            .await
            .unwrap_err();
        assert_eq!(
            short_err.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert_eq!(
            short_err.message(),
            "signature must be 65 bytes (r||s||v), got 3"
        );

        let malformed_err = handler
            .derive_gratis_keys_checked(account, ephemeral_pubkey, Bytes::from([0xff; 65]))
            .await
            .unwrap_err();
        assert_eq!(
            malformed_err.code(),
            jsonrpsee::types::error::INVALID_PARAMS_CODE
        );
        assert!(malformed_err
            .message()
            .starts_with("signature recovery failed:"));

        let prehash =
            outbe_tee::protocol::eip191_hash(&outbe_tee::protocol::derive_account_keys_message(
                outbe_tee::protocol::Ledger::Gratis,
                account,
                ephemeral_pubkey,
            ));
        let (foreign_sig, recovered) = (1u8..=16)
            .find_map(|r_byte| {
                let mut sig = [0u8; 65];
                sig[31] = r_byte;
                sig[63] = 1;
                sig[64] = 27;
                outbe_primitives::tee_signatures::recover_signer(&prehash, &sig)
                    .ok()
                    .filter(|addr| *addr != account)
                    .map(|addr| (sig, addr))
            })
            .expect("should find a recoverable secp256k1 point");

        for err in [
            handler
                .derive_keys_checked(
                    outbe_tee::protocol::Ledger::Gratis,
                    account,
                    ephemeral_pubkey,
                    Bytes::copy_from_slice(&foreign_sig),
                )
                .await
                .unwrap_err(),
            handler
                .derive_gratis_keys_checked(
                    account,
                    ephemeral_pubkey,
                    Bytes::copy_from_slice(&foreign_sig),
                )
                .await
                .unwrap_err(),
        ] {
            assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
            assert_eq!(
                err.message(),
                format!("signature signer {recovered} does not control account {account}")
            );
        }
    }

    #[tokio::test]
    async fn get_finality_proof_enforces_bridge_overflow_window_and_byte_limits() {
        use outbe_primitives::consensus::{ConsensusExecutionBridge, FinalizedBlockBytes};

        let no_bridge = super::OutbeApiHandler::new(std::sync::Arc::new(()));
        let err = no_bridge.finality_proof_for_height(1).await.unwrap_err();
        assert_eq!(err.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
        assert_eq!(err.message(), "node is not serving consensus finalizations");

        // Overflow when walking from u64::MAX without a direct certificate at u64::MAX.
        let overflow_bridge = ConsensusExecutionBridge::new();
        overflow_bridge.set_last_finalized_block_number(u64::MAX);
        let mut overflow_rx = overflow_bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            if let Some((height, reply)) = overflow_rx.recv().await {
                assert_eq!(height, u64::MAX);
                let _ = reply.send(Some(FinalizedBlockBytes {
                    finalization: Bytes::new(),
                    block: Bytes::from_static(&[0xaa]),
                }));
            }
        });
        let overflow_handler =
            super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), overflow_bridge);
        let err = overflow_handler
            .finality_proof_for_height(u64::MAX)
            .await
            .unwrap_err();
        assert_eq!(err.message(), "proof height overflow");

        // Exceeds 4 MiB total block payload limit.
        let oversize_bridge = ConsensusExecutionBridge::new();
        oversize_bridge.set_last_finalized_block_number(10);
        let mut oversize_rx = oversize_bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            if let Some((_, reply)) = oversize_rx.recv().await {
                let _ = reply.send(Some(FinalizedBlockBytes {
                    finalization: Bytes::from_static(&[0x01]),
                    block: Bytes::from(vec![0u8; 4 * 1024 * 1024 + 1]),
                }));
            }
        });
        let oversize_handler =
            super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), oversize_bridge);
        let err = oversize_handler
            .finality_proof_for_height(10)
            .await
            .unwrap_err();
        assert_eq!(err.message(), "ancestor finality proof exceeds byte limit");

        // 65 consecutive uncertified blocks (offsets 0..=64) exhaust the 64-block descendant window.
        let window_bridge = ConsensusExecutionBridge::new();
        window_bridge.set_last_finalized_block_number(100);
        let mut window_rx = window_bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            while let Some((_, reply)) = window_rx.recv().await {
                let _ = reply.send(Some(FinalizedBlockBytes {
                    finalization: Bytes::new(),
                    block: Bytes::from_static(&[0xbb]),
                }));
            }
        });
        let window_handler =
            super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), window_bridge);
        let err = window_handler
            .finality_proof_for_height(100)
            .await
            .unwrap_err();
        assert_eq!(err.message(), "no descendant finalization within 64 blocks");
    }

    /// A bridge whose drainer answers each height from `answer`.
    fn bridge_with_drainer(
        finalized: u64,
        answer: fn(u64) -> Option<outbe_primitives::consensus::FinalizedBlockBytes>,
    ) -> outbe_primitives::consensus::ConsensusExecutionBridge {
        let bridge = outbe_primitives::consensus::ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(finalized);
        let mut rx = bridge.set_finalization_fetcher();
        tokio::spawn(async move {
            while let Some((height, reply)) = rx.recv().await {
                let _ = reply.send(answer(height));
            }
        });
        bridge
    }

    fn direct_block(height: u64) -> Option<outbe_primitives::consensus::FinalizedBlockBytes> {
        Some(outbe_primitives::consensus::FinalizedBlockBytes {
            finalization: Bytes::from_static(&[0xf1, 0x0a]),
            block: Bytes::from(height.to_be_bytes().to_vec()),
        })
    }

    #[tokio::test]
    async fn rejects_height_above_finalized() {
        use outbe_primitives::consensus::ConsensusExecutionBridge;

        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(10);
        let mut rx = bridge.set_finalization_fetcher();
        let handler = super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), bridge);
        let expected = "height 11 is above the node's finalized height 10";

        let errors = [
            handler.finalization_for_height(11).await.unwrap_err(),
            handler.consensus_block_for_height(11).await.unwrap_err(),
            handler.finality_proof_for_height(11).await.unwrap_err(),
        ];
        for err in errors {
            assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
            assert_eq!(err.message(), expected);
        }
        // The rejected requests did not enter the queue.
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn busy_and_not_found_are_distinct_errors() {
        use outbe_primitives::consensus::{ConsensusExecutionBridge, FINALIZATION_QUEUE_DEPTH};

        // NotFound: the drainer has no block for the height.
        let not_found = super::OutbeApiHandler::with_bridge(
            std::sync::Arc::new(()),
            bridge_with_drainer(50, |_| None),
        );
        let not_found_errors = [
            (
                not_found.finalization_for_height(5).await.unwrap_err(),
                "no finalization available for height 5 (not finalized locally or pruned)",
            ),
            (
                not_found.consensus_block_for_height(5).await.unwrap_err(),
                "no finalized consensus block available for height 5",
            ),
            (
                not_found.finality_proof_for_height(5).await.unwrap_err(),
                "no finality evidence available at height 5",
            ),
        ];
        for (err, message) in not_found_errors {
            assert_eq!(err.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
            assert_eq!(err.message(), message);
        }

        // Busy: the queue is full and nobody drains it.
        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(50);
        let rx = bridge.set_finalization_fetcher();
        let mut fillers = Vec::new();
        for height in 0..FINALIZATION_QUEUE_DEPTH as u64 {
            let bridge = bridge.clone();
            fillers.push(tokio::spawn(async move {
                bridge.request_finalization(height).await
            }));
        }
        while rx.len() < FINALIZATION_QUEUE_DEPTH {
            tokio::task::yield_now().await;
        }
        let busy = super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), bridge);
        let busy_errors = [
            busy.finalization_for_height(5).await.unwrap_err(),
            busy.consensus_block_for_height(5).await.unwrap_err(),
            busy.finality_proof_for_height(5).await.unwrap_err(),
        ];
        for err in busy_errors {
            assert_eq!(err.code(), -32005);
            assert_eq!(err.message(), "finalization service is busy, retry shortly");
        }
        assert_eq!(rx.len(), FINALIZATION_QUEUE_DEPTH);
        drop(rx);
        for filler in fillers {
            let _ = filler.await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_finalization_maps_to_resource_exhausted() {
        use outbe_primitives::consensus::ConsensusExecutionBridge;

        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(50);
        let mut rx = bridge.set_finalization_fetcher();
        // A drainer that holds each reply and never answers.
        let drainer = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Some(request) = rx.recv().await {
                held.push(request);
            }
        });
        let handler = super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), bridge);
        let errors = [
            handler.finalization_for_height(5).await.unwrap_err(),
            handler.consensus_block_for_height(5).await.unwrap_err(),
            handler.finality_proof_for_height(5).await.unwrap_err(),
        ];
        for err in errors {
            assert_eq!(err.code(), -32005);
            assert_eq!(
                err.message(),
                "finalization request timed out, retry shortly"
            );
        }
        drainer.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn finality_proof_walk_has_one_total_deadline() {
        use outbe_primitives::consensus::{
            ConsensusExecutionBridge, FinalizedBlockBytes, FINALIZATION_REQUEST_TIMEOUT,
        };

        let bridge = ConsensusExecutionBridge::new();
        bridge.set_last_finalized_block_number(50);
        let mut rx = bridge.set_finalization_fetcher();
        // Each answer is fast enough for the per-request timeout, but the
        // sum of 65 answers is longer than the total deadline.
        let drainer = tokio::spawn(async move {
            while let Some((_, reply)) = rx.recv().await {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let _ = reply.send(Some(FinalizedBlockBytes {
                    finalization: Bytes::new(),
                    block: Bytes::from_static(&[0xbb]),
                }));
            }
        });
        let handler = super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), bridge);
        let started = tokio::time::Instant::now();
        let err = handler.finality_proof_for_height(5).await.unwrap_err();
        assert_eq!(err.code(), -32005);
        assert_eq!(
            err.message(),
            "finalization request timed out, retry shortly"
        );
        assert_eq!(started.elapsed(), FINALIZATION_REQUEST_TIMEOUT);
        drainer.abort();
    }

    #[tokio::test]
    async fn available_height_is_unchanged() {
        let handler = super::OutbeApiHandler::with_bridge(
            std::sync::Arc::new(()),
            bridge_with_drainer(7, |height| {
                if height == 7 {
                    direct_block(height)
                } else {
                    Some(outbe_primitives::consensus::FinalizedBlockBytes {
                        finalization: Bytes::new(),
                        block: Bytes::from(height.to_be_bytes().to_vec()),
                    })
                }
            }),
        );

        let finalization = handler.finalization_for_height(7).await.unwrap();
        assert_eq!(
            serde_json::to_string(&finalization).unwrap(),
            r#"{"finalizationHex":"0xf10a","blockHex":"0x0000000000000007"}"#
        );

        let block = handler.consensus_block_for_height(7).await.unwrap();
        assert_eq!(
            serde_json::to_string(&block).unwrap(),
            r#""0x0000000000000007""#
        );

        let proof = handler.finality_proof_for_height(6).await.unwrap();
        assert_eq!(
            serde_json::to_string(&proof).unwrap(),
            r#"{"finalizationHex":"0xf10a","blockHex":"0x0000000000000007","ancestorBlocksHex":["0x0000000000000006"]}"#
        );
    }
}
