use crate::executor::{AccountedParentArtifact, AccountedParentArtifactProvider};

use alloy_primitives::B256;

use outbe_primitives::{
    consensus::ConsensusExecutionBridge, reshare_artifact::decode_outbe_block_artifacts,
    OutbeHeader,
};

use reth_primitives_traits::AlloyBlockHeader as _;

use reth_provider::HeaderProvider;

/// cache-side helper. Pulls an exact `(block_number, block_hash)`
/// entry from the consensus bridge's execution-summary cache.
fn cached_accounted_parent_artifact(
    summary_cache: &ConsensusExecutionBridge,
    block_number: u64,
    block_hash: B256,
) -> Option<AccountedParentArtifact> {
    summary_cache
        .cached_execution_summary(block_number, block_hash)
        .map(|cached| AccountedParentArtifact {
            summary: cached.summary,
            timestamp: cached.timestamp,
            state_root: cached.state_root,
        })
}

/// bridge-only [`AccountedParentArtifactProvider`]. Returns the
/// cached `(block_number, block_hash)` entry from the consensus bridge.
/// Used in proposer/validator modes where the bridge is available but no
/// Reth provider has been wired (legacy `new_with_bridge` constructor).
#[derive(Clone)]
pub(super) struct BridgeAccountedParentArtifactProvider {
    summary_cache: ConsensusExecutionBridge,
}

impl BridgeAccountedParentArtifactProvider {
    pub(super) fn new(summary_cache: ConsensusExecutionBridge) -> Self {
        Self { summary_cache }
    }
}

impl AccountedParentArtifactProvider for BridgeAccountedParentArtifactProvider {
    fn execution_summary_by_hash(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<AccountedParentArtifact>, reth_evm::execute::ProviderError> {
        Ok(cached_accounted_parent_artifact(
            &self.summary_cache,
            block_number,
            block_hash,
        ))
    }
}

/// composite [`AccountedParentArtifactProvider`] backed by a Reth
/// [`HeaderProvider`] with an optional consensus-bridge cache layered on top.
///
/// Lookup order (per trait contract):
/// 1. Exact `(block_number, block_hash)` cache hit (when `summary_cache` is
///    present).
/// 2. `provider.sealed_header_by_hash(block_hash)` - exact-hash, tree-state
///    aware. Caller of [`HeaderProvider::sealed_header_by_hash`] sees both
///    canonical AND unfinalized side-chain headers, so this branch is the
///    primary V2 path and resolves correctly across reorgs.
/// 3. Canonical-by-number `sealed_header(block_number)` ONLY when its hash
///    equals `block_hash` (explicit double-check). This is a defence-in-depth
///    branch for providers whose `sealed_header_by_hash` default impl is
///    overridden to return `None` for canonical entries.
///
/// **Visibility-miss normalization**: the trait contract for
/// `execution_summary_by_hash` says `Ok(None)` means "I do not currently have
/// this parent". Reth's `HeaderProvider` surfaces a not-yet-visible header as
/// `Err(ProviderError::HeaderNotFound)` (e.g. during the FCU-Valid -> MDBX-commit
/// race, when consensus has finalized the parent but Reth has not persisted
/// the sealed header yet). Both provider branches normalize that variant to
/// `Ok(None)` so the executor's checked `parent_artifact_hint` fallback can
/// engage. Other `Err` variants (real I/O / database corruption) propagate.
///
/// Construction: pass `Some(cache)` to keep the bridge fast-path, or `None`
/// for full-node mode (no consensus bridge, provider-backed only).
#[derive(Clone)]
pub struct RethAccountedParentArtifactProvider<P> {
    provider: P,
    summary_cache: Option<ConsensusExecutionBridge>,
}

impl<P> RethAccountedParentArtifactProvider<P> {
    pub fn new(provider: P, summary_cache: Option<ConsensusExecutionBridge>) -> Self {
        Self {
            provider,
            summary_cache,
        }
    }

    fn cached_artifact(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Option<AccountedParentArtifact> {
        self.summary_cache
            .as_ref()
            .and_then(|cache| cached_accounted_parent_artifact(cache, block_number, block_hash))
    }
}

impl<P> AccountedParentArtifactProvider for RethAccountedParentArtifactProvider<P>
where
    P: HeaderProvider<Header = OutbeHeader> + Clone + Send + Sync + 'static,
{
    fn execution_summary_by_hash(
        &self,
        block_number: u64,
        block_hash: B256,
    ) -> Result<Option<AccountedParentArtifact>, reth_evm::execute::ProviderError> {
        // (1) Cache hit - exact `(block_number, block_hash)` only.
        if let Some(cached) = self.cached_artifact(block_number, block_hash) {
            return Ok(Some(cached));
        }

        // (2) Exact-hash provider lookup. Sees tree-state, so unfinalized
        // side-chain parents are resolvable as long as the import path has
        // already inserted the header. During the FCU-Valid -> MDBX-commit
        // race the provider may surface `HeaderNotFound`; the trait
        // contract treats that as a visibility miss (`Ok(None)`), letting
        // the executor fall through to its checked `parent_artifact_hint`.
        match self.provider.sealed_header_by_hash(block_hash) {
            Ok(Some(sealed)) => {
                // `(block_number, block_hash)` must match the resolved
                // header. A header whose number diverges from the metadata is
                // a protocol violation - reject loudly rather than silently
                // using a wrong block. This is NOT a visibility miss.
                if sealed.header().number() != block_number {
                    return Err(reth_evm::execute::ProviderError::HeaderNotFound(
                        block_hash.into(),
                    ));
                }
                return Ok(decode_accounted_parent_artifact(sealed.header()));
            }
            Ok(None) => {}
            Err(reth_evm::execute::ProviderError::HeaderNotFound(_)) => {}
            Err(error) => return Err(error),
        }

        // (3) Canonical-by-number fallback - gated by explicit hash equality
        //. If the canonical entry at `block_number` does not hash to
        // `block_hash`, return `Ok(None)`. The caller (executor) maps `None`
        // to a `BlockExecutionError` - never a silent wrong-parent acceptance.
        // `HeaderNotFound` here is also a visibility miss; other `Err`
        // variants propagate.
        match self.provider.sealed_header(block_number) {
            Ok(Some(sealed)) => {
                if sealed.hash() == block_hash {
                    return Ok(decode_accounted_parent_artifact(sealed.header()));
                }
            }
            Ok(None) => {}
            Err(reth_evm::execute::ProviderError::HeaderNotFound(_)) => {}
            Err(error) => return Err(error),
        }

        Ok(None)
    }
}

/// decode `OutbeBlockArtifacts.execution_summary` from
/// `header.extra_data` and pair it with the header timestamp. Returns
/// `None` if the artifact bytes don't decode (invalid header) or if the
/// header carries no `execution_summary` field.
fn decode_accounted_parent_artifact(header: &OutbeHeader) -> Option<AccountedParentArtifact> {
    let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref()).ok()?;
    artifacts
        .execution_summary
        .map(|summary| AccountedParentArtifact {
            summary,
            timestamp: header.timestamp(),
            state_root: Some(header.state_root()),
        })
}
