//! Engine-layer transport implementations for the follower.
//!
//! `outbe-consensus` defines the transport seam ([`FinalizedSource`],
//! [`LocalBlockSource`], [`TipSource`]). This module provides the concrete
//! implementations that the engine layer can build. The engine layer can build
//! them because they need the reth node handle (local block reads) and an RPC
//! client (upstream finalized blocks + tip discovery). `outbe-consensus` depends
//! on neither of them. Both halves
//! are wired: a follower fetches finalized blocks from a validator's
//! `outbe_getFinalization` and verifies them against the epoch committee.
//!
//! * [`RethLocalBlockSource`] - REAL: reads already-imported blocks from the
//!   reth execution DB by hash. Used to serve the marshal's `Request::Block`.
//! * [`UpstreamRpcClient`] - the upstream finalized-block + tip transport: a
//!   jsonrpsee HTTP client. Tip discovery calls `outbe_consensusStatus`.
//!   Finalized-block fetch calls `outbe_getFinalization(height)` and decodes the
//!   returned `(finalizationHex, blockHex)` into a [`CertifiedFinalizedBlock`].
//!   The client decodes the certificate with the UNBOUNDED committee codec config.
//!   This config is a permissive length upper bound, the same one the marshal's
//!   archive uses. Thus the client does not need the epoch committee size to
//!   decode. The marshal re-verifies the certificate against the actual committee
//!   afterwards.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::B256;
use commonware_codec::Read as _;
use commonware_consensus::types::Height;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_cryptography::certificate::Verifier as _;
use jsonrpsee::core::client::{ClientT, Error as ClientError};
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use jsonrpsee::rpc_params;
use outbe_consensus::block::ConsensusBlock;
use outbe_consensus::follow::{
    CertifiedFinalizedBlock, FinalizedSource, LocalBlockSource, TipSource,
};
use outbe_consensus::hybrid::HybridScheme;
use outbe_consensus::marshal_types::Finalization;
use outbe_node::OutbeFullNode;
use reth_ethereum::storage::{BlockReader, TransactionVariant};
use tracing::{debug, warn};

/// Reads already-imported blocks from the local reth execution DB by hash.
///
/// The follower imports finalized blocks through the executor (FCU + newPayload).
/// Thus, by the time the marshal asks to backfill a `Request::Block(digest)`, the
/// block is in the EL DB. This is the same lookup the validator path's resolver
/// performs against peers, but sourced locally.
#[derive(Clone)]
pub struct RethLocalBlockSource {
    node: OutbeFullNode,
}

impl RethLocalBlockSource {
    pub fn new(node: OutbeFullNode) -> Self {
        Self { node }
    }
}

impl LocalBlockSource for RethLocalBlockSource {
    fn get_block_by_digest(
        &self,
        digest: outbe_consensus::digest::Digest,
    ) -> impl Future<Output = Option<ConsensusBlock>> + Send {
        let node = self.node.clone();
        async move {
            let hash: B256 = digest.0;
            match node
                .provider
                .recovered_block(hash.into(), TransactionVariant::NoHash)
            {
                Ok(Some(recovered)) => {
                    Some(ConsensusBlock::from_sealed(recovered.into_sealed_block()))
                }
                Ok(None) => {
                    debug!(%hash, "local EL has no block for requested digest");
                    None
                }
                Err(error) => {
                    debug!(%hash, %error, "failed reading block from local EL");
                    None
                }
            }
        }
    }
}

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
struct UpstreamFinalizationProof {
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
///   [`CertifiedFinalizedBlock`]. The client decodes the certificate with the
///   UNBOUNDED committee codec config (a permissive length bound, the same the
///   marshal's archive uses). Thus the client needs no committee-size knowledge.
///   The marshal re-verifies the cert against the actual epoch committee.
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
            .map_err(|e| {
                eyre::eyre!("failed to build upstream RPC client for {normalized}: {e}")
            })?;
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
    ) -> Option<outbe_consensus::follow::upstream::AncestorFinalityProof> {
        self.get_finalization(height).await.map(|certified| {
            outbe_consensus::follow::upstream::AncestorFinalityProof {
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
            .map_err(|e| eyre::eyre!("upstream eth_call tributeOfferPublicKey failed: {e}"))?;
        let bytes = alloy_primitives::hex::decode(result.trim_start_matches("0x"))
            .map_err(|e| eyre::eyre!("malformed eth_call result from upstream: {e}"))?;
        decode_tribute_offer_public_key(&bytes)
    }
}

fn decode_tribute_offer_public_key(bytes: &[u8]) -> eyre::Result<B256> {
    if bytes.len() != 32 {
        return Err(eyre::eyre!(
            "upstream tributeOfferPublicKey returned {} bytes instead of one ABI word",
            bytes.len()
        ));
    }
    Ok(B256::from_slice(bytes))
}

/// Decode an `outbe_getFinalization` proof into a `CertifiedFinalizedBlock`.
///
/// This function decodes the certificate with the unbounded committee config (a
/// permissive upper bound on length). This function does NOT establish trust. The
/// marshal verifies the cert against the epoch committee. Returns `None` on any
/// malformed field.
fn decode_finalization_proof(proof: &UpstreamFinalizationProof) -> Option<CertifiedFinalizedBlock> {
    let fin_bytes = alloy_primitives::hex::decode(proof.finalization_hex.trim_start_matches("0x"))
        .inspect_err(|error| debug!(%error, "malformed finalizationHex from upstream"))
        .ok()?;
    let block_bytes = alloy_primitives::hex::decode(proof.block_hex.trim_start_matches("0x"))
        .inspect_err(|error| debug!(%error, "malformed blockHex from upstream"))
        .ok()?;

    let cert_cfg = HybridScheme::<MinSig>::certificate_codec_config_unbounded();
    let mut fin_reader: &[u8] = &fin_bytes;
    let finalization = Finalization::read_cfg(&mut fin_reader, &cert_cfg)
        .inspect_err(|error| debug!(%error, "failed to decode upstream finalization"))
        .ok()?;
    if !fin_reader.is_empty() {
        debug!("trailing bytes after upstream finalization");
        return None;
    }

    let mut block_reader: &[u8] = &block_bytes;
    let block = ConsensusBlock::read_cfg(&mut block_reader, &())
        .inspect_err(|error| debug!(%error, "failed to decode upstream block"))
        .ok()?;
    if !block_reader.is_empty() {
        debug!("trailing bytes after upstream block");
        return None;
    }

    Some(CertifiedFinalizedBlock {
        finalization,
        block,
    })
}

impl FinalizedSource for UpstreamRpcClient {
    async fn get_finality_proof(
        &self,
        height: Height,
    ) -> Option<outbe_consensus::follow::upstream::AncestorFinalityProof> {
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
        let proof = outbe_consensus::follow::upstream::AncestorFinalityProof {
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
