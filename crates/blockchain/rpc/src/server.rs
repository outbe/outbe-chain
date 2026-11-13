//! Outbe RPC server implementation.

mod finalization;
mod tee;
#[cfg(test)]
mod tests;

use alloy_primitives::{Address, Bytes, B256, U256};
use jsonrpsee::core::RpcResult;
use outbe_primitives::header::OutbeHeader;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_primitives::{
    consensus::{ConsensusExecutionBridge, ConsensusOperatingMode},
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

use crate::api::{
    ConsensusStatusInfo, EmissionInfo, EpochInfo, FinalizationProof, OutbeApiServer,
    ParticipationInfo, RadiclePeerInfo, RadiclePhaseErrorInfo, RadiclePhaseInfo,
    RadicleRepositoryInfo, RadicleRepositoryStateInfo, RadicleStatusInfo, SlashConfig, SlashInfo,
    SyncStatusInfo, ValidatorDetailInfo, ValidatorInfo,
};
use outbe_radicle::integration::{
    RadicleRepositoryState, RadicleStatusHandle, RadicleStatusSnapshot, RadicleVotingGate,
    RadicleVotingGateError,
};

fn read_effective_slash_config(
    reader: &dyn crate::state_reader::OutbeRpcStateReader,
    storage: StorageHandle<'_>,
) -> Result<SlashConfig, outbe_primitives::error::PrecompileError> {
    reader.slash_config(storage)
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
    reader: std::sync::Arc<dyn crate::state_reader::OutbeRpcStateReader>,
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
    pub fn new(
        provider: Arc<P>,
        reader: std::sync::Arc<dyn crate::state_reader::OutbeRpcStateReader>,
    ) -> Self {
        Self {
            provider,
            chain_identity: None,
            bridge: None,
            tee_renewal_schedule: None,
            radicle_status: outbe_radicle::integration::RadicleStatusChannel::disabled(),
            tee_enclave_health: outbe_tee::TeeEnclaveHealthChannel::disabled(),
            reader,
        }
    }

    /// Create a handler with access to the consensus bridge.
    pub fn with_bridge(
        provider: Arc<P>,
        bridge: ConsensusExecutionBridge,
        reader: std::sync::Arc<dyn crate::state_reader::OutbeRpcStateReader>,
    ) -> Self {
        let mut handler = Self::new(provider, reader);
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
        observed_repository_count: count(manager.observed_repository_count)?,
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
    async fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>> {
        self.with_latest_state(|storage| self.reader.validators(storage))
    }

    async fn get_validator(&self, address: Address) -> RpcResult<Option<ValidatorDetailInfo>> {
        self.with_latest_state(|storage| self.reader.validator(storage, address))
    }

    async fn get_epoch_info(&self) -> RpcResult<EpochInfo> {
        self.with_latest_state(|storage| self.reader.epoch(storage))
    }

    async fn upgrade_key_v1(
        &self,
        context: Bytes,
        proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
        legacy_direct_dev_source: bool,
    ) -> RpcResult<Bytes> {
        tee::TeeRpc::new(&*self.provider, self.chain_identity, self.reader.as_ref())
            .upgrade_key(context, proof, legacy_direct_dev_source)
            .await
    }

    async fn tee_renewal_schedule_v1(&self) -> RpcResult<TeeRenewalScheduleV1> {
        tee::TeeRpc::new(&*self.provider, self.chain_identity, self.reader.as_ref())
            .renewal_schedule(self.tee_renewal_schedule)
    }

    async fn get_stake(&self, address: Address) -> RpcResult<U256> {
        self.with_latest_state(|storage| self.reader.stake(storage, address))
    }

    async fn get_slash_info(&self, address: Address) -> RpcResult<SlashInfo> {
        self.with_latest_state(|storage| self.reader.slash_info(storage, address))
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
        Ok(self.reader.emission_info())
    }

    async fn get_slash_config(&self) -> RpcResult<SlashConfig> {
        self.with_latest_state(|storage| read_effective_slash_config(self.reader.as_ref(), storage))
    }

    async fn get_participation(&self, address: Address) -> RpcResult<ParticipationInfo> {
        self.with_latest_state(|storage| self.reader.participation(storage, address))
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
        self.get_finalization_for_height(height).await
    }

    async fn get_consensus_block(&self, height: u64) -> RpcResult<Bytes> {
        self.get_consensus_block_for_height(height).await
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

fn invalid_params_err(msg: impl std::fmt::Display) -> jsonrpsee::types::ErrorObject<'static> {
    jsonrpsee::types::ErrorObject::owned(
        jsonrpsee::types::error::INVALID_PARAMS_CODE,
        msg.to_string(),
        None::<()>,
    )
}
