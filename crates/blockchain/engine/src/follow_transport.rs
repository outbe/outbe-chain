//! Engine-layer transport implementations for the follower.
//!
//! `outbe-consensus` defines the transport seam ([`FinalizedSource`],
//! [`LocalBlockSource`], [`TipSource`]). This module provides the concrete
//! implementations the engine layer can build, because they need the reth node
//! handle (local block reads) and an RPC client (upstream finalized blocks +
//! tip discovery) — neither of which `outbe-consensus` depends on.
//!
//! * [`RethLocalBlockSource`] — REAL: reads already-imported blocks from the
//!   reth execution DB by hash. Used to serve the marshal's `Request::Block`.
//! * [`UpstreamRpcClient`] — the upstream finalized-block + tip transport. The
//!   serving side (an upstream node's consensus `getFinalization` RPC) and this
//!   jsonrpsee client are an EXTERNAL-RPC surface; until that surface is agreed
//!   and added, this is a clearly-named NOT-YET-WIRED transport that fails fast
//!   rather than silently pretending to sync. See `run_follow_stack`.

use std::future::Future;
use std::sync::Arc;

use alloy_primitives::B256;
use commonware_consensus::types::Height;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use jsonrpsee::rpc_params;
use outbe_consensus::block::ConsensusBlock;
use outbe_consensus::follow::{
    CertifiedFinalizedBlock, FinalizedSource, LocalBlockSource, TipSource,
};
use outbe_node::OutbeFullNode;
use reth_ethereum::storage::{BlockReader, TransactionVariant};
use tracing::{debug, warn};

/// Reads already-imported blocks from the local reth execution DB by hash.
///
/// The follower imports finalized blocks through the executor (FCU + newPayload),
/// so by the time the marshal asks to backfill a `Request::Block(digest)` the
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

/// Minimal view of the upstream's `outbe_consensusStatus` response — we only
/// need the finalized tip for sync progress. Deserializing a subset keeps this
/// independent of the full `ConsensusStatusInfo` shape.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpstreamConsensusStatus {
    last_finalized_block: u64,
}

/// The upstream finalized-block + tip transport: a jsonrpsee HTTP client against
/// an upstream node's `outbe_*` RPC.
///
/// **Tip discovery is REAL** — it calls the upstream's existing
/// `outbe_consensusStatus` and reads `lastFinalizedBlock`.
///
/// **Finalized-block fetch is NOT YET WIRED.** It needs a serving RPC on the
/// upstream that returns, for a height, the native-encoded `ConsensusBlock` plus
/// the finalization certificate bytes (mirroring Tempo's
/// `consensus_getFinalization`). That method does not yet exist on outbe's
/// served RPC surface, and ADDING it is an external-RPC change that must be
/// agreed first. Until then `get_finalization` returns `None`; the follower's
/// anchor bootstrap therefore fails fast at startup (see `run_follow_stack`),
/// so the node refuses to pretend it is syncing. Wiring is a localized change:
/// implement `get_finalization` to call the agreed serving method and decode
/// `(Finalization, ConsensusBlock)`.
#[derive(Clone)]
pub struct UpstreamRpcClient {
    client: Arc<HttpClient>,
    url: String,
}

impl UpstreamRpcClient {
    /// Build an HTTP client for `url`. Accepts `http://host:port` (or `host:port`,
    /// which is prefixed with `http://`).
    pub fn new(url: &str) -> eyre::Result<Self> {
        let normalized = if url.contains("://") {
            url.to_string()
        } else {
            format!("http://{url}")
        };
        let client = HttpClientBuilder::default()
            .build(&normalized)
            .map_err(|e| eyre::eyre!("failed to build upstream RPC client for {normalized}: {e}"))?;
        Ok(Self {
            client: Arc::new(client),
            url: normalized,
        })
    }

    /// Whether the upstream finalized-block-fetch transport is wired. Used by
    /// `run_follow_stack` to fail fast rather than spin without progress.
    pub const fn finalized_fetch_wired() -> bool {
        false
    }
}

impl FinalizedSource for UpstreamRpcClient {
    fn get_finalization(
        &self,
        _height: Height,
    ) -> impl Future<Output = Option<CertifiedFinalizedBlock>> + Send {
        // NOT YET WIRED — see the type-level note. Returns None so the
        // driver/resolver treat it as "upstream lacks it"; the startup
        // fail-fast in run_follow_stack is what prevents a silent no-op node.
        async move { None }
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
