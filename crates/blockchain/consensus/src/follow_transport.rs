//! Follower transports over an upstream JSON-RPC endpoint.
//!
//! Upstream RPC responses remain bounded and their certificates are re-verified
//! by the marshal against the committee.
//!
//! * [`UpstreamRpcClient`] - the upstream finalized-block + tip transport: a
//!   jsonrpsee HTTP client. Tip discovery calls `outbe_consensusStatus`.
//!   Finalized-block fetch calls `outbe_getFinalization(height)` and decodes the
//!   returned `(finalizationHex, blockHex)` into a [`CertifiedFinalizedBlock`].
//!   The client decodes the certificate bounded by `MAX_COMMITTEE_MEMBERS`.
//!   The marshal re-verifies the certificate against the actual committee
//!   afterwards.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::block::ConsensusBlock;
use crate::errors::FollowTransportError;
use crate::follow::{CertifiedFinalizedBlock, FinalizedSource, TipSource};
use alloy_primitives::{Address, B256};
use commonware_codec::Read as _;
use commonware_consensus::types::Height;
use jsonrpsee::core::client::{ClientT, Error as ClientError};
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use jsonrpsee::rpc_params;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_tee::registry_view::{ChainRpc, FinalizedStateRpc, RegistryRpc};
use tracing::{debug, warn};

/// Minimal view of the upstream's `outbe_consensusStatus` response. We only
/// need the finalized tip for sync progress. Deserializing a subset keeps this
/// independent of the full `ConsensusStatusInfo` shape.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamConsensusStatus {
    last_finalized_block: u64,
}

/// The upstream's `outbe_getFinalization` response (mirrors `FinalizationProof`
/// in `outbe-rpc`): hex of the encoded finalization cert + the encoded block.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpstreamFinalizationProof {
    finalization_hex: String,
    block_hex: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamAncestorFinalityProof {
    #[serde(flatten)]
    certified: UpstreamFinalizationProof,
    ancestor_blocks_hex: Vec<String>,
}

/// The upstream finalized-block + tip transport: a jsonrpsee HTTP client against
/// an upstream node's `outbe_*` RPC.
///
/// * Tip discovery -> `outbe_consensusStatus.lastFinalizedBlock`.
/// * Finalized-block fetch -> `outbe_getFinalization(height)`, decoded into a
///   [`CertifiedFinalizedBlock`]. The client decodes the certificate bounded by
///   `MAX_COMMITTEE_MEMBERS`. The marshal re-verifies the cert against the
///   actual epoch committee.
#[derive(Clone)]
pub struct UpstreamRpcClient {
    client: Arc<HttpClient>,
    url: String,
    methods: Arc<UpstreamMethods>,
}

/// Explicit bounds of one upstream request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpstreamLimits {
    /// Wall-clock bound of one RPC request.
    pub request_timeout: Duration,
    /// Largest accepted response body, in bytes.
    pub max_response_bytes: u32,
}

impl UpstreamLimits {
    /// Production bounds.
    ///
    /// * 10 s per request: a finality-proof request followed by its legacy
    ///   fallback stays inside the follower resolver's 30 s resolution deadline.
    /// * 10 MiB per response body (the jsonrpsee default, now explicit). The
    ///   follower resolver runs a bounded number of requests at once and caps
    ///   every delivered value separately.
    pub const DEFAULT: Self = Self {
        request_timeout: Duration::from_secs(10),
        max_response_bytes: 10 * 1024 * 1024,
    };
}

/// JSON-RPC 2.0 "method not found".
const JSONRPC_METHOD_NOT_FOUND: i32 = -32601;

/// What a failed call of an optional upstream method means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallFailure {
    /// The upstream does not serve the method: use the legacy method from now on.
    MethodNotFound,
    /// Timeout, transport or upstream error: retry the same method later, never
    /// switch methods because of it.
    Transient,
}

impl CallFailure {
    fn of(error: &ClientError) -> Self {
        match error {
            ClientError::Call(object) if object.code() == JSONRPC_METHOD_NOT_FOUND => {
                Self::MethodNotFound
            }
            _ => Self::Transient,
        }
    }
}

/// Optional upstream methods this client learned are missing. Only a
/// method-not-found answer sets a flag; nothing transient does.
#[derive(Default)]
struct UpstreamMethods {
    finality_proof_missing: AtomicBool,
    consensus_block_missing: AtomicBool,
}

impl UpstreamRpcClient {
    /// Build an HTTP client for `url` with production [`UpstreamLimits`].
    /// Accepts `http://host:port` (or `host:port`, to which this function adds the
    /// `http://` prefix).
    pub fn new(url: &str) -> eyre::Result<Self> {
        Self::with_limits(url, UpstreamLimits::DEFAULT)
    }

    /// Build an HTTP client for `url` with explicit request bounds.
    pub fn with_limits(url: &str, limits: UpstreamLimits) -> eyre::Result<Self> {
        let normalized = if url.contains("://") {
            url.to_string()
        } else {
            format!("http://{url}")
        };
        let client = HttpClientBuilder::default()
            .request_timeout(limits.request_timeout)
            .max_response_size(limits.max_response_bytes)
            .build(&normalized)
            .map_err(
                |e| crate::errors::FollowTransportError::BuildUpstreamRpcClient {
                    url: normalized.clone(),
                    error: format!("{e}"),
                },
            )?;
        Ok(Self {
            client: Arc::new(client),
            url: normalized,
            methods: Arc::default(),
        })
    }

    /// The legacy single-block proof, used when the upstream has no
    /// `outbe_getFinalityProof`.
    async fn legacy_finality_proof(
        &self,
        height: Height,
    ) -> Option<crate::follow::upstream::AncestorFinalityProof> {
        self.get_finalization(height).await.map(|certified| {
            crate::follow::upstream::AncestorFinalityProof {
                certified,
                ancestors: Vec::new(),
            }
        })
    }
}

impl UpstreamRpcClient {
    /// Query the upstream's on-chain tribute offer public key
    /// (`TeeRegistry.tributeOfferPublicKey()`, selector `0x1b640a92`). A non-zero
    /// value means the chain is TEE-bootstrapped. Then a follower that re-executes
    /// offer / enclave-registration txs needs a local enclave that holds the offer
    /// key. This function reads the key from the UPSTREAM, not from the follower's
    /// local state. The follower starts at genesis, where the bootstrap tx that
    /// sets this key has not run yet. Thus a local read would spuriously report a
    /// non-TEE chain.
    pub async fn tribute_offer_public_key(&self) -> eyre::Result<alloy_primitives::B256> {
        let call = serde_json::json!({
            "to": "0x000000000000000000000000000000000000ee0a",
            "data": "0x1b640a92",
        });
        let result: String = self
            .client
            .request("eth_call", rpc_params![call, "latest"])
            .await
            .map_err(|e| {
                crate::errors::FollowTransportError::UpstreamTributeOfferPublicKeyCall(
                    e.to_string(),
                )
            })?;
        let bytes =
            alloy_primitives::hex::decode(result.trim_start_matches("0x")).map_err(|e| {
                crate::errors::FollowTransportError::MalformedEthCallResult(e.to_string())
            })?;
        decode_tribute_offer_public_key(&bytes)
    }

    /// Query the canonical block hash at `height` on the upstream endpoint via
    /// `eth_getBlockByNumber`. Returns `Ok(None)` when the upstream has no block
    /// at `height` yet (or the response carries no `hash` field).
    pub async fn block_hash_by_number(
        &self,
        height: u64,
    ) -> Result<Option<B256>, FollowTransportError> {
        let block: serde_json::Value = self
            .client
            .request(
                "eth_getBlockByNumber",
                rpc_params![format!("0x{height:x}"), false],
            )
            .await
            .map_err(|error| FollowTransportError::UpstreamRpcRequest {
                method: "eth_getBlockByNumber",
                error: error.to_string(),
            })?;
        block
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .map(str::parse::<B256>)
            .transpose()
            .map_err(FollowTransportError::from)
    }
}

impl ChainRpc for UpstreamRpcClient {
    type Error = FollowTransportError;

    async fn chain_id(&self) -> Result<u64, Self::Error> {
        let encoded: String = self
            .client
            .request("eth_chainId", rpc_params![])
            .await
            .map_err(|error| FollowTransportError::UpstreamRpcRequest {
                method: "eth_chainId",
                error: error.to_string(),
            })?;
        u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(&encoded), 16).map_err(|source| {
            FollowTransportError::ParseHexU64Result {
                method: "eth_chainId",
                source,
            }
        })
    }
}

impl FinalizedStateRpc for UpstreamRpcClient {
    async fn finalized_block(&self) -> Result<serde_json::Value, Self::Error> {
        self.client
            .request("eth_getBlockByNumber", rpc_params!["finalized", false])
            .await
            .map_err(|error| FollowTransportError::UpstreamRpcRequest {
                method: "eth_getBlockByNumber",
                error: error.to_string(),
            })
    }

    async fn call_at(
        &self,
        to: Address,
        data: &[u8],
        block_tag: &str,
    ) -> Result<Vec<u8>, Self::Error> {
        let call = serde_json::json!({
            "to": format!("{to:#x}"),
            "data": format!("0x{}", hex::encode(data)),
        });
        let encoded: String = self
            .client
            .request("eth_call", rpc_params![call, block_tag])
            .await
            .map_err(|error| FollowTransportError::UpstreamRpcRequest {
                method: "eth_call",
                error: error.to_string(),
            })?;
        hex::decode(encoded.strip_prefix("0x").unwrap_or(&encoded)).map_err(|source| {
            FollowTransportError::DecodeHexBytesResult {
                method: "eth_call",
                source,
            }
        })
    }
}

impl RegistryRpc for UpstreamRpcClient {
    async fn tee_renewal_schedule_v1(&self) -> Result<TeeRenewalScheduleV1, Self::Error> {
        let schedule: TeeRenewalScheduleV1 = self
            .client
            .request("outbe_teeRenewalScheduleV1", rpc_params![])
            .await
            .map_err(|error| FollowTransportError::UpstreamRpcRequest {
                method: "outbe_teeRenewalScheduleV1",
                error: error.to_string(),
            })?;
        schedule.validate()?;
        Ok(schedule)
    }
}

fn decode_tribute_offer_public_key(bytes: &[u8]) -> eyre::Result<B256> {
    if bytes.len() != 32 {
        return Err(
            crate::errors::FollowTransportError::UnexpectedTributeOfferPublicKeyLength(bytes.len())
                .into(),
        );
    }
    Ok(B256::from_slice(bytes))
}

/// Decode an `outbe_getFinalization` proof into a `CertifiedFinalizedBlock`.
///
/// This function decodes the certificate bounded by `MAX_COMMITTEE_MEMBERS`.
/// This function does NOT establish trust. The marshal verifies the cert
/// against the epoch committee. Returns `None` on any malformed field.
pub(super) fn decode_finalization_proof(
    proof: &UpstreamFinalizationProof,
) -> Option<CertifiedFinalizedBlock> {
    let fin_bytes = alloy_primitives::hex::decode(proof.finalization_hex.trim_start_matches("0x"))
        .inspect_err(|error| debug!(%error, "malformed finalizationHex from upstream"))
        .ok()?;
    let block_bytes = alloy_primitives::hex::decode(proof.block_hex.trim_start_matches("0x"))
        .inspect_err(|error| debug!(%error, "malformed blockHex from upstream"))
        .ok()?;

    crate::follow::decode_public_finalized_block(
        &fin_bytes,
        &block_bytes,
        crate::bls::MAX_COMMITTEE_MEMBERS,
    )
    .inspect_err(|error| debug!(%error, "failed to decode upstream finalized block"))
    .ok()
}

impl FinalizedSource for UpstreamRpcClient {
    async fn get_finality_proof(
        &self,
        height: Height,
    ) -> Option<crate::follow::upstream::AncestorFinalityProof> {
        if self.methods.finality_proof_missing.load(Ordering::Acquire) {
            return self.legacy_finality_proof(height).await;
        }
        let proof: UpstreamAncestorFinalityProof = match self
            .client
            .request("outbe_getFinalityProof", rpc_params![height.get()])
            .await
        {
            Ok(value) => value,
            Err(error) => match CallFailure::of(&error) {
                CallFailure::MethodNotFound => {
                    self.methods
                        .finality_proof_missing
                        .store(true, Ordering::Release);
                    return self.legacy_finality_proof(height).await;
                }
                CallFailure::Transient => {
                    debug!(url = %self.url, height = height.get(), %error, "upstream getFinalityProof failed");
                    return None;
                }
            },
        };
        if proof.ancestor_blocks_hex.len() > 64 {
            return None;
        }
        let certified = decode_finalization_proof(&proof.certified)?;
        let mut ancestors = Vec::with_capacity(proof.ancestor_blocks_hex.len());
        for encoded in proof.ancestor_blocks_hex {
            let bytes = alloy_primitives::hex::decode(encoded.trim_start_matches("0x")).ok()?;
            let mut input = bytes.as_slice();
            let block = ConsensusBlock::read_cfg(&mut input, &()).ok()?;
            if !input.is_empty() {
                return None;
            }
            ancestors.push(block);
        }
        let proof = crate::follow::upstream::AncestorFinalityProof {
            certified,
            ancestors,
        };
        proof.validate_envelope(height).ok()?;
        Some(proof)
    }

    async fn get_block(&self, height: Height) -> Option<ConsensusBlock> {
        if self.methods.consensus_block_missing.load(Ordering::Acquire) {
            return self.get_finalization(height).await.map(|value| value.block);
        }
        let bytes: alloy_primitives::Bytes = match self
            .client
            .request("outbe_getConsensusBlock", rpc_params![height.get()])
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => match CallFailure::of(&error) {
                CallFailure::MethodNotFound => {
                    self.methods
                        .consensus_block_missing
                        .store(true, Ordering::Release);
                    return self.get_finalization(height).await.map(|value| value.block);
                }
                CallFailure::Transient => {
                    debug!(url = %self.url, height = height.get(), %error, "upstream getConsensusBlock failed");
                    return None;
                }
            },
        };
        let mut input = bytes.as_ref();
        let block = ConsensusBlock::read_cfg(&mut input, &()).ok()?;
        (input.is_empty() && block.number() == height.get()).then_some(block)
    }

    fn get_finalization(
        &self,
        height: Height,
    ) -> impl Future<Output = Option<CertifiedFinalizedBlock>> + Send {
        let client = self.client.clone();
        let url = self.url.clone();
        async move {
            let proof: UpstreamFinalizationProof = match client
                .request("outbe_getFinalization", rpc_params![height.get()])
                .await
            {
                Ok(proof) => proof,
                Err(error) => {
                    // A "not available" upstream answer is expected while the
                    // upstream catches up. Downgrade the log to debug. The
                    // driver/marshal handles the retry.
                    debug!(%url, height = height.get(), %error, "upstream getFinalization failed");
                    return None;
                }
            };
            decode_finalization_proof(&proof)
        }
    }
}

impl TipSource for UpstreamRpcClient {
    fn finalized_tip(&self) -> impl Future<Output = Option<Height>> + Send {
        let client = self.client.clone();
        let url = self.url.clone();
        async move {
            match client
                .request::<UpstreamConsensusStatus, _>("outbe_consensusStatus", rpc_params![])
                .await
            {
                Ok(status) => Some(Height::new(status.last_finalized_block)),
                Err(error) => {
                    warn!(%url, %error, "failed to query upstream consensus status (tip)");
                    None
                }
            }
        }
    }
}

/// Read the offer-key commitment from the selected certified upstream.
pub async fn read_upstream_tribute_offer_public_key(url: &str) -> eyre::Result<B256> {
    UpstreamRpcClient::new(url)?
        .tribute_offer_public_key()
        .await
}

#[cfg(test)]
#[path = "follow_transport_tests.rs"]
mod upstream_tests;

#[cfg(test)]
mod tests {
    use super::decode_tribute_offer_public_key;
    use alloy_primitives::B256;

    #[test]
    fn offer_key_rpc_word_is_exactly_32_bytes() {
        let expected = B256::repeat_byte(0x42);
        assert_eq!(
            decode_tribute_offer_public_key(expected.as_slice()).unwrap(),
            expected
        );
        for malformed in [vec![], vec![0x42; 31], vec![0x42; 33]] {
            assert!(decode_tribute_offer_public_key(&malformed).is_err());
        }
    }
}
