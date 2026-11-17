//! Reversible process-local authority, independent of resident execution keys.

use alloy_primitives::B256;
use tokio::sync::watch;

/// Current admission verdict. This is not a validator-role or DKG-share verdict.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TeeAuthorityState {
    /// Verified resident key and finalized admission permit privileged operations.
    Armed { enclave_id: B256, valid_until: u64 },
    /// Jailing closes authority without deleting a resident execution key.
    SuspendedJailed { has_verified_resident_key: bool },
    /// An expired lease closes authority without deleting a resident execution key.
    SuspendedLeaseExpired { has_verified_resident_key: bool },
    /// Finalized policy or identity no longer permits privileged operations.
    SuspendedPolicyMismatch { has_verified_resident_key: bool },
    /// No verified resident key has been admitted in this process.
    #[default]
    UnkeyedAwaitingAdmission,
}

impl TeeAuthorityState {
    /// Whether TEE authority is open; callers must still apply their role gates.
    pub const fn is_authority_open(self) -> bool {
        matches!(self, Self::Armed { .. })
    }

    /// Whether block execution can retain the verified in-memory key.
    pub const fn has_verified_resident_key(self) -> bool {
        match self {
            Self::Armed { .. } => true,
            Self::SuspendedJailed {
                has_verified_resident_key,
            }
            | Self::SuspendedLeaseExpired {
                has_verified_resident_key,
            }
            | Self::SuspendedPolicyMismatch {
                has_verified_resident_key,
            } => has_verified_resident_key,
            Self::UnkeyedAwaitingAdmission => false,
        }
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
                TeeAuthorityState::SuspendedPolicyMismatch {
                    has_verified_resident_key: true,
                }
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
            TeeAuthorityState::SuspendedJailed {
                has_verified_resident_key: true,
            },
            TeeAuthorityState::SuspendedLeaseExpired {
                has_verified_resident_key: true,
            },
            TeeAuthorityState::SuspendedPolicyMismatch {
                has_verified_resident_key: true,
            },
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
        for state in [
            TeeAuthorityState::SuspendedJailed {
                has_verified_resident_key: false,
            },
            TeeAuthorityState::SuspendedLeaseExpired {
                has_verified_resident_key: false,
            },
            TeeAuthorityState::SuspendedPolicyMismatch {
                has_verified_resident_key: false,
            },
        ] {
            assert!(!state.has_verified_resident_key());
            assert!(!state.is_authority_open());
        }
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
        assert_eq!(
            watch.snapshot(),
            TeeAuthorityState::SuspendedPolicyMismatch {
                has_verified_resident_key: true
            }
        );
        assert!(!watch.snapshot().is_authority_open());
        let unkeyed = TeeAuthorityChannel::default().subscribe();
        assert_eq!(
            unkeyed.snapshot(),
            TeeAuthorityState::UnkeyedAwaitingAdmission
        );
    }
}
