//! Common context for every authenticated enclave call.
//!
//! Execution uses the exact state being executed (including historical replay),
//! never a process-wide latest height. Background calls use a node snapshot.
//! These are NodeHost inputs, not a replacement for admission/finality proofs.
use std::{
    cell::Cell,
    marker::PhantomData,
    rc::Rc,
    sync::{Arc, OnceLock, RwLock},
};

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    addresses::UPDATE_ADDRESS,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EnclaveContextKindV1 {
    /// Initialization before chain state is available; zero is not a current tip.
    #[default]
    Bootstrap,
    /// Exact execution/eth_call state, which may precede the live chain tip.
    Execution,
    /// State selected by the node/CLI for a background operation.
    Snapshot,
    /// An older client sent no context. Versioned logic must reject this kind.
    Legacy,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnclaveCallContextV1 {
    pub kind: EnclaveContextKindV1,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub block_number: u64,
    pub block_timestamp: u64,
    /// Canonical on-chain u32 encoding; zero is the pre-upgrade baseline.
    pub protocol_version: u32,
}

impl EnclaveCallContextV1 {
    pub fn from_storage(storage: &StorageHandle<'_>) -> Result<Self> {
        let version = storage.sload(UPDATE_ADDRESS, U256::ZERO)?;
        let timestamp = storage.timestamp()?;
        if version > U256::from(u32::MAX) || timestamp > U256::from(u64::MAX) {
            return Err(PrecompileError::Fatal(
                "enclave call context integer overflow".into(),
            ));
        }
        Ok(Self {
            kind: EnclaveContextKindV1::Execution,
            chain_id: storage.chain_id()?,
            genesis_hash: storage.genesis_hash()?,
            block_number: storage.block_number()?,
            block_timestamp: timestamp.to(),
            protocol_version: version.to(),
        })
    }
}

thread_local! {
    static CURRENT: Cell<Option<EnclaveCallContextV1>> = const { Cell::new(None) };
}

/// Read the request-local context inside any enclave handler. No global tip is
/// substituted: nested operations and concurrent sessions remain isolated.
pub fn current() -> Option<EnclaveCallContextV1> {
    CURRENT.with(Cell::get)
}

/// Synchronous scope only: cannot move to another thread or cross an await.
pub struct ContextScope {
    previous: Option<EnclaveCallContextV1>,
    _not_send: PhantomData<Rc<()>>,
}
impl ContextScope {
    pub fn enter(context: EnclaveCallContextV1) -> Self {
        Self {
            previous: CURRENT.with(|slot| slot.replace(Some(context))),
            _not_send: PhantomData,
        }
    }
    pub fn from_storage(storage: &StorageHandle<'_>) -> Result<Self> {
        Ok(Self::enter(EnclaveCallContextV1::from_storage(storage)?))
    }
}
impl Drop for ContextScope {
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.set(self.previous));
    }
}

type ContextProvider = dyn Fn() -> std::result::Result<EnclaveCallContextV1, String> + Send + Sync;
static PROVIDER: OnceLock<Arc<ContextProvider>> = OnceLock::new();
static SNAPSHOT: RwLock<Option<EnclaveCallContextV1>> = RwLock::new(None);

/// CLI/startup snapshot when no live node provider is installed. This never
/// overrides an execution scope or the node's exact snapshot resolver.
pub fn set_snapshot(context: EnclaveCallContextV1) -> std::result::Result<(), &'static str> {
    *SNAPSHOT
        .write()
        .map_err(|_| "enclave snapshot lock poisoned")? = Some(context);
    Ok(())
}

/// Install the node's read-only snapshot resolver once. Execution scopes take
/// precedence, so eth_call/historical re-execution never inherit the live tip.
pub fn install_provider(provider: Arc<ContextProvider>) -> std::result::Result<(), &'static str> {
    PROVIDER
        .set(provider)
        .map_err(|_| "enclave context provider already installed")
}

pub fn resolve() -> std::result::Result<EnclaveCallContextV1, crate::TransportError> {
    if let Some(context) = current() {
        return Ok(context);
    }
    match PROVIDER.get() {
        Some(provider) => provider().map_err(crate::TransportError::EnclaveError),
        None => Ok(SNAPSHOT
            .read()
            .map_err(|_| {
                crate::TransportError::EnclaveError("enclave snapshot lock poisoned".into())
            })?
            .unwrap_or_default()),
    }
}

pub fn starts_stream(request: &crate::protocol::EnclaveRequest) -> bool {
    use crate::protocol::EnclaveRequest::*;
    matches!(
        request,
        BeginUpgradeKeyTransferV1 { .. }
            | BeginDcapVerificationV1 { .. }
            | BeginDcapOnboardingVerificationV1 { .. }
            | BeginDcapOnboardingArtifactIngestV1 { .. }
    )
}
pub fn continues_stream(request: &crate::protocol::EnclaveRequest) -> bool {
    use crate::protocol::EnclaveRequest::*;
    matches!(
        request,
        DcapVerificationChunkV1 { .. }
            | FinishDcapVerificationV1 { .. }
            | DcapOnboardingArtifactChunkV1 { .. }
            | CommitDcapOnboardingArtifactRecordV1 { .. }
            | FinishDcapOnboardingArtifactIngestV1 { .. }
    )
}
fn finishes_stream(request: &crate::protocol::EnclaveRequest) -> bool {
    use crate::protocol::EnclaveRequest::*;
    matches!(
        request,
        FinishDcapVerificationV1 { .. } | FinishDcapOnboardingArtifactIngestV1 { .. }
    )
}

/// A multi-frame operation is one call, even if the chain advances while the
/// caller downloads its next record. Keep the Begin context through Finish.
#[derive(Default)]
pub struct StreamContext {
    context: Option<EnclaveCallContextV1>,
}
impl StreamContext {
    pub fn for_request(
        &mut self,
        request: &crate::protocol::EnclaveRequest,
    ) -> std::result::Result<EnclaveCallContextV1, crate::TransportError> {
        let context = if continues_stream(request) {
            self.context.ok_or_else(|| {
                crate::TransportError::EnclaveError("stream has no call context".into())
            })?
        } else {
            resolve()?
        };
        if starts_stream(request) {
            self.context = Some(context);
        }
        if finishes_stream(request) {
            self.context = None;
        }
        Ok(context)
    }

    pub fn accept(
        &mut self,
        request: &crate::protocol::EnclaveRequest,
        context: EnclaveCallContextV1,
    ) -> std::result::Result<(), crate::TransportError> {
        if continues_stream(request) && self.context != Some(context) {
            return Err(crate::TransportError::EnclaveError(
                "enclave stream context changed".into(),
            ));
        }
        if starts_stream(request) {
            self.context = Some(context);
        }
        if finishes_stream(request) {
            self.context = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_is_nested_thread_local_and_restored_after_panic() {
        let old = EnclaveCallContextV1 {
            block_number: 9,
            protocol_version: 1,
            ..Default::default()
        };
        let new = EnclaveCallContextV1 {
            block_number: 10,
            protocol_version: 2,
            ..old
        };
        let _scope = ContextScope::enter(old);
        let _ = std::panic::catch_unwind(|| {
            let _nested = ContextScope::enter(new);
            assert_eq!(current(), Some(new));
            assert_eq!(std::thread::spawn(current).join().unwrap(), None);
            panic!("test unwind");
        });
        assert_eq!(current(), Some(old));
    }

    #[test]
    fn exact_storage_context_tracks_activation_and_historical_replay() {
        use outbe_primitives::storage::hashmap::HashMapStorageProvider;
        let mut provider = HashMapStorageProvider::new(42);
        StorageHandle::enter(&mut provider, |storage| {
            storage.set_block_timestamp(U256::from(100)).unwrap();
            storage
                .sstore(UPDATE_ADDRESS, U256::ZERO, U256::from(1))
                .unwrap();
            let old = EnclaveCallContextV1::from_storage(&storage).unwrap();
            storage
                .sstore(UPDATE_ADDRESS, U256::ZERO, U256::from(2))
                .unwrap();
            assert_eq!(
                EnclaveCallContextV1::from_storage(&storage)
                    .unwrap()
                    .protocol_version,
                2
            );
            storage
                .sstore(UPDATE_ADDRESS, U256::ZERO, U256::from(1))
                .unwrap();
            assert_eq!(EnclaveCallContextV1::from_storage(&storage).unwrap(), old);
            assert_eq!(old.block_timestamp, 100);
            storage
                .sstore(
                    UPDATE_ADDRESS,
                    U256::ZERO,
                    U256::from(u32::MAX) + U256::from(1),
                )
                .unwrap();
            assert!(EnclaveCallContextV1::from_storage(&storage).is_err());
        });
    }

    #[test]
    fn stream_freezes_begin_context_and_rejects_changed_frames() {
        use crate::protocol::EnclaveRequest;
        let begin = EnclaveRequest::BeginDcapVerificationV1 {
            request_hash: B256::ZERO,
            evidence_len: 1,
            policy_len: 1,
            block_timestamp: 7,
        };
        let chunk = EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: B256::ZERO,
            offset: 0,
            bytes: vec![1],
        };
        let finish = EnclaveRequest::FinishDcapVerificationV1 {
            request_hash: B256::ZERO,
        };
        let old = EnclaveCallContextV1 {
            block_number: 100,
            protocol_version: 2,
            ..Default::default()
        };
        let next = EnclaveCallContextV1 {
            block_number: 101,
            protocol_version: 3,
            ..old
        };
        let mut sender = StreamContext::default();
        let mut receiver = StreamContext::default();
        {
            let _scope = ContextScope::enter(old);
            receiver
                .accept(&begin, sender.for_request(&begin).unwrap())
                .unwrap();
        }
        let _scope = ContextScope::enter(next);
        assert_eq!(sender.for_request(&chunk).unwrap(), old);
        assert!(receiver.accept(&chunk, next).is_err());
        receiver.accept(&chunk, old).unwrap();
        receiver
            .accept(&finish, sender.for_request(&finish).unwrap())
            .unwrap();
        assert_eq!(sender.for_request(&EnclaveRequest::Health).unwrap(), next);
        assert!(sender.for_request(&chunk).is_err());
    }
}
