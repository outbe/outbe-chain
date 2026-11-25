//! Outbe RPC API trait definition.

use alloy_primitives::{Address, Bytes, B256};
use jsonrpsee::proc_macros::rpc;
use outbe_primitives::consensus::{ConsensusOperatingMode, RandomnessStatus};
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;

/// Response type for consensus status.
///
/// **Note:** This is a finalized snapshot, not real-time consensus state.
/// The reporter updates the fields on each finalization event.
/// - `current_view`: last *finalized* Simplex view (not the live voting view)
/// - `connected_peers`: number of signers in the last certificate bitmap
///   (not the actual number of P2P connections)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsensusStatusInfo {
    /// Last finalized Simplex view (updated on finalization, not real-time).
    pub current_view: u64,
    /// Number of signers in the last finalized certificate (not live P2P peers).
    pub connected_peers: u32,
    pub is_active: bool,
    pub has_threshold_shares: bool,
    pub last_finalized_block: u64,
    pub last_vrf_seed: Option<B256>,
    pub randomness_status: RandomnessStatus,
    pub vrf_material_version: u64,
    pub last_dkg_activation_height: u64,
    pub next_planned_activation_height: u64,
    pub vrf_expiry_height: u64,
    /// Next height at which the validator set freezes for a DKG ceremony.
    /// `None` means this node does not expose a DKG schedule.
    pub next_dkg_freeze_height: Option<u64>,
    /// Whether a planned operator interruption is currently outside the DKG window.
    pub maintenance_safe: Option<bool>,
    pub is_validator: bool,
    /// Current ontological consensus operating mode of this node.
    pub mode: ConsensusOperatingMode,
    /// Local TEE-enclave health that the canary observes. This is local health,
    /// not consensus data. The value is `disabled` when the canary worker is off.
    pub enclave: EnclaveHealthInfo,
}

/// Operator-visible local TEE-enclave health. The node's periodic canary
/// decrypt (`--tee-canary.*`) supplies it. It has the same role as
/// local runtime status. It is never a consensus input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnclaveHealthInfo {
    pub state: EnclaveHealth,
    /// True when the last canary decrypt succeeded.
    pub ready: bool,
    pub offer_key_ready: bool,
    pub last_ok_ago_millis: Option<u64>,
    pub last_canary_latency_ms: Option<u64>,
    pub consecutive_failures: u64,
    pub last_failure_class: Option<String>,
    /// Resident enclave context uptime.
    pub uptime_s: Option<u64>,
    pub heap_current_bytes: Option<u64>,
    pub heap_peak_bytes: Option<u64>,
    /// `null` before the first observation; `false` for resident canary-only telemetry.
    pub health_probe_supported: Option<bool>,
}

/// Stable JSON enclave-health vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnclaveHealth {
    Disabled,
    Starting,
    Ready,
    Degraded,
    Unavailable,
}

impl Default for EnclaveHealthInfo {
    fn default() -> Self {
        Self {
            state: EnclaveHealth::Disabled,
            ready: false,
            offer_key_ready: false,
            last_ok_ago_millis: None,
            last_canary_latency_ms: None,
            consecutive_failures: 0,
            last_failure_class: None,
            uptime_s: None,
            heap_current_bytes: None,
            heap_peak_bytes: None,
            health_probe_supported: None,
        }
    }
}

/// Response type for `getFinalization`: the finalized certificate + block for a
/// height, as hex of their commonware-codec encodings. A follower's resolver
/// reconstructs the marshal delivery as the decoded `finalizationHex` followed
/// by the decoded `blockHex`. Then the resolver verifies the certificate against
/// the epoch committee. Hex (0x-prefixed) keeps the wire JSON-friendly. The
/// caller does NOT trust the bytes. Verification happens against the committee,
/// not this RPC.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizationProof {
    /// Hex of the `commonware_codec::Encode` finalization certificate.
    pub finalization_hex: String,
    /// Hex of the `commonware_codec::Encode` finalized `ConsensusBlock`.
    pub block_hex: String,
}

/// A direct certificate for a descendant and the ordered blocks preceding it.
/// The consumer authenticates every parent link and the signing committee.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AncestorFinalizationProof {
    #[serde(flatten)]
    pub certified: FinalizationProof,
    pub ancestor_blocks_hex: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RadiclePhaseInfo {
    Disabled,
    JoiningUnbound,
    Ready,
    RuntimeDegraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RadiclePhaseErrorInfo {
    SidecarUnavailable,
    LocalNodeIdUnavailable,
    BindingMismatch,
    ActiveBindingMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadicleStatusInfo {
    pub phase: RadiclePhaseInfo,
    pub phase_error: Option<RadiclePhaseErrorInfo>,
    pub local_node_id: Option<B256>,
    pub last_seen_finalized_number: Option<u64>,
    pub last_seen_finalized_hash: Option<B256>,
    pub last_converged_finalized_number: Option<u64>,
    pub last_converged_finalized_hash: Option<B256>,
    pub desired_repository_count: u64,
    pub observed_repository_count: u64,
    pub available_repository_count: u64,
    pub pending_repository_count: u64,
    pub resolved_peer_count: u64,
    pub unresolved_peer_count: u64,
    pub connected_peer_count: u64,
    pub finality_regressions: u64,
    pub finality_conflicts: u64,
    pub provider_failures: u64,
    pub endpoint_failures: u64,
    pub uds_failures: u64,
    pub tcp_status_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiclePeerInfo {
    pub validator: Address,
    pub sender_bls_public_key: Bytes,
    pub request_id: B256,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub node_id: B256,
    pub addresses: Vec<String>,
    pub anchor_number: u64,
    pub anchor_hash: B256,
    pub valid_until_height: u64,
    pub signature: Bytes,
    pub encoded_frame: Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RadicleRepositoryStateInfo {
    Available,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadicleRepositoryInfo {
    pub repo_id: String,
    pub state: RadicleRepositoryStateInfo,
}

/// Outbe custom RPC namespace.
///
/// Enable with `--http.api outbe`.
#[rpc(server, namespace = "outbe")]
pub trait OutbeApi {
    /// Export the network key only for a finalized upgrade candidate.
    #[method(name = "upgradeKeyV1")]
    async fn upgrade_key_v1(
        &self,
        encoded_context: Bytes,
        proof: outbe_tee::upgrade_transfer::UpgradeKeyProofV1,
        legacy_direct_dev_source: bool,
    ) -> jsonrpsee::core::RpcResult<Bytes>;

    /// Exact finalized epoch/freeze schedule for host-side TEE renewal alerts.
    #[method(name = "teeRenewalScheduleV1")]
    async fn tee_renewal_schedule_v1(
        &self,
        checkpoint_hash: Option<B256>,
    ) -> jsonrpsee::core::RpcResult<TeeRenewalScheduleV1>;

    /// Returns finalized consensus snapshot (view, certificate signers, shares, is_validator).
    #[method(name = "consensusStatus")]
    async fn consensus_status(&self) -> jsonrpsee::core::RpcResult<ConsensusStatusInfo>;

    /// Immutable local Radicle integration health snapshot.
    #[method(name = "radicleStatus")]
    async fn radicle_status(&self) -> jsonrpsee::core::RpcResult<RadicleStatusInfo>;

    /// Canonical signed endpoint evidence for verified active peers.
    #[method(name = "radiclePeers")]
    async fn radicle_peers(&self) -> jsonrpsee::core::RpcResult<Vec<RadiclePeerInfo>>;

    /// Desired public repositories and their local availability state.
    #[method(name = "radicleRepositories")]
    async fn radicle_repositories(&self) -> jsonrpsee::core::RpcResult<Vec<RadicleRepositoryInfo>>;

    /// Returns the finalized certificate + block at `height` (hex-encoded), for
    /// `--upstream` followers to backfill and verify. Only nodes that run
    /// consensus (validators), or a follower that has itself synced the height,
    /// serve it. Otherwise, the call returns an error. The caller verifies the
    /// certificate against the epoch committee. This RPC is a bytes transport,
    /// not a trust root.
    ///
    /// Rejects `height > lastFinalizedBlock` with JSON-RPC `-32602` before
    /// enqueueing. Returns JSON-RPC `-32005` when the bounded finalization
    /// request queue is full or the request exceeds its 5s timeout.
    #[method(name = "getFinalization")]
    async fn get_finalization(&self, height: u64) -> jsonrpsee::core::RpcResult<FinalizationProof>;

    /// Canonical consensus block bytes for commitment-bound ancestor recovery.
    /// This endpoint conveys no finality authority. Consumers must authenticate
    /// the requested digest through an independently verified descendant.
    ///
    /// Rejects `height > lastFinalizedBlock` with JSON-RPC `-32602` before
    /// enqueueing. Returns JSON-RPC `-32005` when the bounded finalization
    /// request queue is full or the request exceeds its 5s timeout.
    #[method(name = "getConsensusBlock")]
    async fn get_consensus_block(&self, height: u64) -> jsonrpsee::core::RpcResult<Bytes>;

    /// Returns a direct finalization certificate for `height` or its nearest
    /// certified descendant within 64 blocks together with intermediate
    /// ancestor block bytes.
    ///
    /// Rejects `height > lastFinalizedBlock` with JSON-RPC `-32602` before
    /// enqueueing. Enforces a 5s total walk budget and returns JSON-RPC
    /// `-32005` on queue saturation or timeout.
    #[method(name = "getFinalityProof")]
    async fn get_finality_proof(
        &self,
        height: u64,
    ) -> jsonrpsee::core::RpcResult<AncestorFinalizationProof>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radicle_rpc_types_are_canonical() {
        let status = RadicleStatusInfo {
            phase: RadiclePhaseInfo::Ready,
            phase_error: None,
            local_node_id: Some(B256::repeat_byte(0x11)),
            last_seen_finalized_number: Some(7),
            last_seen_finalized_hash: Some(B256::repeat_byte(0x22)),
            last_converged_finalized_number: Some(7),
            last_converged_finalized_hash: Some(B256::repeat_byte(0x22)),
            desired_repository_count: 2,
            observed_repository_count: 2,
            available_repository_count: 1,
            pending_repository_count: 1,
            resolved_peer_count: 3,
            unresolved_peer_count: 1,
            connected_peer_count: 2,
            finality_regressions: 0,
            finality_conflicts: 0,
            provider_failures: 0,
            endpoint_failures: 0,
            uds_failures: 0,
            tcp_status_failures: 0,
        };
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["phase"], "ready");
        assert_eq!(json["observedRepositoryCount"], 2);
        assert_eq!(
            json["localNodeId"],
            format!("{:#x}", B256::repeat_byte(0x11))
        );

        let repository = RadicleRepositoryInfo {
            repo_id: "11".repeat(20),
            state: RadicleRepositoryStateInfo::Pending,
        };
        let json = serde_json::to_value(repository).unwrap();
        assert_eq!(json["repoId"].as_str().unwrap().len(), 40);
        assert_eq!(json["state"], "pending");
    }

    #[test]
    fn test_consensus_status_info_serialization_camel_case() {
        let validator_info = ConsensusStatusInfo {
            current_view: 42,
            connected_peers: 3,
            is_active: true,
            has_threshold_shares: true,
            last_finalized_block: 41,
            last_vrf_seed: Some(B256::ZERO),
            randomness_status: RandomnessStatus::Healthy,
            vrf_material_version: 2,
            last_dkg_activation_height: 21,
            next_planned_activation_height: 21021,
            vrf_expiry_height: 21621,
            next_dkg_freeze_height: Some(20_721),
            maintenance_safe: Some(true),
            is_validator: true,
            mode: ConsensusOperatingMode::Signer,
            enclave: EnclaveHealthInfo::default(),
        };

        let json = serde_json::to_string(&validator_info).unwrap();

        for expected_field in [
            "\"currentView\"",
            "\"connectedPeers\"",
            "\"isActive\"",
            "\"hasThresholdShares\"",
            "\"lastFinalizedBlock\"",
            "\"lastVrfSeed\"",
            "\"randomnessStatus\"",
            "\"vrfMaterialVersion\"",
            "\"lastDkgActivationHeight\"",
            "\"nextPlannedActivationHeight\"",
            "\"vrfExpiryHeight\"",
            "\"nextDkgFreezeHeight\"",
            "\"maintenanceSafe\"",
            "\"isValidator\"",
            "\"mode\"",
            "\"signer\"",
        ] {
            assert!(
                json.contains(expected_field),
                "must contain {expected_field}: {json}"
            );
        }
        assert!(!json.contains("current_view"), "must not use snake_case");
        assert!(!json.contains("connected_peers"), "must not use snake_case");

        let follower_info = ConsensusStatusInfo {
            current_view: 100,
            connected_peers: 5,
            is_active: false,
            has_threshold_shares: false,
            last_finalized_block: 99,
            last_vrf_seed: None,
            randomness_status: RandomnessStatus::Expired,
            vrf_material_version: 5,
            last_dkg_activation_height: 100,
            next_planned_activation_height: 21100,
            vrf_expiry_height: 21700,
            next_dkg_freeze_height: None,
            maintenance_safe: None,
            is_validator: false,
            mode: ConsensusOperatingMode::Follower,
            enclave: EnclaveHealthInfo::default(),
        };

        let follower_json = serde_json::to_string(&follower_info).unwrap();
        let deserialized: ConsensusStatusInfo = serde_json::from_str(&follower_json).unwrap();

        assert_eq!(deserialized.current_view, 100);
        assert_eq!(deserialized.connected_peers, 5);
        assert!(!deserialized.is_active);
        assert!(!deserialized.has_threshold_shares);
        assert_eq!(deserialized.last_finalized_block, 99);
        assert!(deserialized.last_vrf_seed.is_none());
        assert_eq!(deserialized.randomness_status, RandomnessStatus::Expired);
        assert_eq!(deserialized.vrf_material_version, 5);
        assert_eq!(deserialized.last_dkg_activation_height, 100);
        assert_eq!(deserialized.next_planned_activation_height, 21100);
        assert_eq!(deserialized.vrf_expiry_height, 21700);
        assert!(!deserialized.is_validator);
        assert_eq!(deserialized.mode, ConsensusOperatingMode::Follower);
    }
}
