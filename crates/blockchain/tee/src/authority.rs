//! Reversible process-local authority, independent of resident execution keys.

use std::sync::RwLock;

use alloy_primitives::B256;
use tokio::sync::watch;

/// Current admission verdict. This is not a validator-role or DKG-share verdict.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TeeAuthorityState {
    /// Verified resident key and finalized admission permit privileged operations.
    Armed { enclave_id: B256, valid_until: u64 },
    /// Founding DKG installed a verified key before the first finalized registry lease.
    ArmedBootstrap { enclave_id: B256 },
    /// Jailing closes authority without deleting a resident execution key.
    SuspendedJailed,
    /// An expired lease closes authority without deleting a resident execution key.
    SuspendedLeaseExpired,
    /// Finalized policy or identity no longer permits privileged operations.
    SuspendedPolicyMismatch,
    /// No verified resident key has been admitted in this process.
    #[default]
    UnkeyedAwaitingAdmission,
}

impl TeeAuthorityState {
    /// Whether TEE authority is open; callers must still apply their role gates.
    pub const fn is_authority_open(self) -> bool {
        matches!(self, Self::Armed { .. } | Self::ArmedBootstrap { .. })
    }

    /// Whether block execution can retain the verified in-memory key.
    pub const fn has_verified_resident_key(self) -> bool {
        !matches!(self, Self::UnkeyedAwaitingAdmission)
    }

    /// Whether donor key transfer may rely on a finalized registry lease.
    pub const fn has_finalized_lease(&self) -> bool {
        matches!(self, Self::Armed { .. })
    }
}

/// Publisher owned by the admission lifecycle, never a source of key material.
#[derive(Clone, Debug)]
pub struct TeeAuthorityChannel {
    sender: watch::Sender<TeeAuthorityState>,
}

impl TeeAuthorityChannel {
    /// Creates a channel from facts already verified by the admission lifecycle.
    pub fn new(initial: TeeAuthorityState) -> Self {
        let (sender, _) = watch::channel(initial);
        Self { sender }
    }

    /// Replaces the snapshot even when no subscribers are currently present.
    pub fn publish(&self, state: TeeAuthorityState) {
        self.sender.send_replace(state);
    }

    /// Subscribes to the current snapshot and subsequent reversible transitions.
    pub fn subscribe(&self) -> TeeAuthorityWatch {
        TeeAuthorityWatch {
            receiver: self.sender.subscribe(),
        }
    }

    /// Copies the current verdict without retaining a watch read lock.
    pub fn snapshot(&self) -> TeeAuthorityState {
        *self.sender.borrow()
    }
}

impl Default for TeeAuthorityChannel {
    fn default() -> Self {
        Self::new(TeeAuthorityState::default())
    }
}

/// Read-only, cloneable view for consensus, private RPC and key-transfer gates.
#[derive(Clone, Debug)]
pub struct TeeAuthorityWatch {
    receiver: watch::Receiver<TeeAuthorityState>,
}

impl TeeAuthorityWatch {
    /// Copies the latest verdict; losing the publisher closes authority, not execution.
    pub fn snapshot(&self) -> TeeAuthorityState {
        let state = *self.receiver.borrow();
        if self.receiver.has_changed().is_err() {
            if state.has_verified_resident_key() {
                TeeAuthorityState::SuspendedPolicyMismatch
            } else {
                TeeAuthorityState::UnkeyedAwaitingAdmission
            }
        } else {
            state
        }
    }

    /// Waits for a publication; publisher shutdown retains the typed watch error.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.receiver.changed().await
    }
}

/// Failure to bind the process-wide authority publisher.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TeeAuthorityInstallError {
    /// Reversible updates must use the existing publisher, not replace its watch.
    #[error("TEE authority watch is already installed")]
    AlreadyInstalled,
    /// A panicked installation left the port unavailable; readers fail closed.
    #[error("TEE authority port lock poisoned")]
    LockPoisoned,
}

static TEE_AUTHORITY_WATCH: RwLock<Option<TeeAuthorityWatch>> = RwLock::new(None);

/// Binds the shared admission watch before starting consensus and private RPC.
pub fn install_tee_authority_watch(
    watch: TeeAuthorityWatch,
) -> Result<(), TeeAuthorityInstallError> {
    let mut slot = TEE_AUTHORITY_WATCH
        .write()
        .map_err(|_| TeeAuthorityInstallError::LockPoisoned)?;
    if slot.is_some() {
        return Err(TeeAuthorityInstallError::AlreadyInstalled);
    }
    *slot = Some(watch);
    Ok(())
}

/// Reads the same process-wide authority source from any infrastructure crate.
pub fn tee_authority_watch() -> Option<TeeAuthorityWatch> {
    TEE_AUTHORITY_WATCH.read().ok()?.clone()
}

/// Missing or unavailable installation always denies authority and key presence.
pub fn current_tee_authority_state() -> TeeAuthorityState {
    tee_authority_watch().map_or_else(TeeAuthorityState::default, |watch| watch.snapshot())
}

/// Clears only process-local test authority. Hold `tee_authority_test_guard` while using the port.
#[cfg(any(test, feature = "test-utils"))]
pub fn clear_tee_authority_watch_for_test() {
    *TEE_AUTHORITY_WATCH
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    TEE_AUTHORITY_WATCH.clear_poison();
}

/// Serializes process-port tests and clears their installation on scope exit.
#[cfg(any(test, feature = "test-utils"))]
pub struct TeeAuthorityTestGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

#[cfg(any(test, feature = "test-utils"))]
impl Drop for TeeAuthorityTestGuard {
    fn drop(&mut self) {
        clear_tee_authority_watch_for_test();
    }
}

/// Starts a clean, exclusive process-port test scope; drop before starting another scope.
#[cfg(any(test, feature = "test-utils"))]
pub fn tee_authority_test_guard() -> TeeAuthorityTestGuard {
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    clear_tee_authority_watch_for_test();
    TeeAuthorityTestGuard { _guard: guard }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARMED: TeeAuthorityState = TeeAuthorityState::Armed {
        enclave_id: B256::repeat_byte(7),
        valid_until: 123,
    };

    #[tokio::test]
    async fn suspensions_preserve_execution_and_authority_can_reopen() {
        let channel = TeeAuthorityChannel::default();
        let mut watch = channel.subscribe();
        let other = watch.clone();
        assert_eq!(
            watch.snapshot(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
        assert!(!watch.snapshot().is_authority_open());
        assert!(!watch.snapshot().has_verified_resident_key());
        for suspended in [
            TeeAuthorityState::SuspendedJailed,
            TeeAuthorityState::SuspendedLeaseExpired,
            TeeAuthorityState::SuspendedPolicyMismatch,
        ] {
            for state in [ARMED, suspended, ARMED] {
                channel.publish(state);
                // Synchronous consumers see revocation before polling changed().
                assert_eq!(other.snapshot(), state);
                assert_eq!(channel.snapshot(), state);
                watch.changed().await.unwrap();
                assert_eq!(watch.snapshot(), state);
                assert!(state.has_verified_resident_key());
                assert_eq!(state.is_authority_open(), state == ARMED);
            }
        }
        channel.publish(TeeAuthorityState::UnkeyedAwaitingAdmission);
        assert!(!other.snapshot().has_verified_resident_key());
        assert!(!other.snapshot().is_authority_open());
    }

    #[tokio::test]
    async fn publication_without_subscribers_and_publisher_shutdown() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TeeAuthorityChannel>();
        assert_send_sync::<TeeAuthorityWatch>();
        let channel = TeeAuthorityChannel::default();
        channel.publish(ARMED);
        let mut watch = channel.subscribe();
        assert_eq!(watch.snapshot(), ARMED);
        drop(channel);
        assert!(watch.changed().await.is_err());
        assert_eq!(watch.snapshot(), TeeAuthorityState::SuspendedPolicyMismatch);
        assert!(!watch.snapshot().is_authority_open());
        let unkeyed = TeeAuthorityChannel::default().subscribe();
        assert_eq!(
            unkeyed.snapshot(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
    }

    #[test]
    fn process_port_is_shared_live_and_scoped_test_installations_do_not_leak() {
        for _ in 0..2 {
            let _guard = tee_authority_test_guard();
            assert!(tee_authority_watch().is_none());
            assert_eq!(
                current_tee_authority_state(),
                TeeAuthorityState::UnkeyedAwaitingAdmission
            );
            let channel = TeeAuthorityChannel::new(ARMED);
            install_tee_authority_watch(channel.subscribe()).unwrap();
            let watch = tee_authority_watch().unwrap();
            assert_eq!(current_tee_authority_state(), ARMED);
            assert_eq!(
                install_tee_authority_watch(channel.subscribe()),
                Err(TeeAuthorityInstallError::AlreadyInstalled)
            );
            channel.publish(TeeAuthorityState::SuspendedJailed);
            assert_eq!(watch.snapshot(), TeeAuthorityState::SuspendedJailed);
            assert_eq!(
                current_tee_authority_state(),
                TeeAuthorityState::SuspendedJailed
            );
            clear_tee_authority_watch_for_test();
            assert!(tee_authority_watch().is_none());
            install_tee_authority_watch(channel.subscribe()).unwrap();
            // Dropping this scope must clear the second installation as well.
        }
        let _guard = tee_authority_test_guard();
        assert!(tee_authority_watch().is_none());
    }

    #[test]
    fn bootstrap_opens_execution_authority_but_not_finalized_lease_transfer() {
        let bootstrap = TeeAuthorityState::ArmedBootstrap {
            enclave_id: B256::repeat_byte(7),
        };
        for (state, open, key, lease) in [
            (
                TeeAuthorityState::UnkeyedAwaitingAdmission,
                false,
                false,
                false,
            ),
            (bootstrap, true, true, false),
            (ARMED, true, true, true),
            (TeeAuthorityState::SuspendedJailed, false, true, false),
            (TeeAuthorityState::SuspendedLeaseExpired, false, true, false),
            (
                TeeAuthorityState::SuspendedPolicyMismatch,
                false,
                true,
                false,
            ),
        ] {
            assert_eq!(state.is_authority_open(), open);
            assert_eq!(state.has_verified_resident_key(), key);
            assert_eq!(state.has_finalized_lease(), lease);
        }
        let channel = TeeAuthorityChannel::new(bootstrap);
        let watch = channel.subscribe();
        channel.publish(ARMED);
        assert_eq!(watch.snapshot(), ARMED);
        assert!(watch.snapshot().has_finalized_lease());
    }
}
