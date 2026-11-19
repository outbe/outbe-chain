//! Reversible process-local authority, independent of resident execution keys.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};
use std::thread::{self, ThreadId};

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

#[derive(Clone)]
struct InstalledRefresh {
    generation: u64,
    hook: Arc<dyn Fn(u64) + Send + Sync>,
}

struct AuthorityShared {
    observed_finalized_height: AtomicU64,
    applied_finalized_height: AtomicU64,
    /// Bumped on install and on guard stop. In-flight refresh publishes only its own generation.
    refresh_generation: AtomicU64,
    /// Set when the guard stops. Cleared only by the next [`TeeAuthorityChannel::install_sync_refresh`].
    retired: AtomicBool,
    refresh: Mutex<Option<InstalledRefresh>>,
    /// Serializes door-side refresh so a second reader waits for the first verdict.
    refresh_gate: Mutex<()>,
    refresh_owner_thread: Mutex<Option<ThreadId>>,
    /// Serializes publication with guard retirement.
    publish_gate: Mutex<()>,
    /// `notify_one` permit for the lease guard. Independent of the shared lifecycle notifier.
    guard_wakeup: tokio::sync::Notify,
    sender: Weak<watch::Sender<TeeAuthorityState>>,
}

impl AuthorityShared {
    fn lock_refresh_gate(&self) -> RefreshGateGuard<'_> {
        let gate = lock_mutex(&self.refresh_gate);
        *lock_mutex(&self.refresh_owner_thread) = Some(thread::current().id());
        RefreshGateGuard {
            owner: &self.refresh_owner_thread,
            _gate: gate,
        }
    }
}

struct RefreshGateGuard<'a> {
    owner: &'a Mutex<Option<ThreadId>>,
    _gate: MutexGuard<'a, ()>,
}

impl Drop for RefreshGateGuard<'_> {
    fn drop(&mut self) {
        *lock_mutex(self.owner) = None;
    }
}

/// Publisher owned by the admission lifecycle, never a source of key material.
#[derive(Clone)]
pub struct TeeAuthorityChannel {
    sender: Arc<watch::Sender<TeeAuthorityState>>,
    shared: Arc<AuthorityShared>,
}

impl TeeAuthorityChannel {
    /// Creates a channel from facts already verified by the admission lifecycle.
    pub fn new(initial: TeeAuthorityState) -> Self {
        let (sender, _) = watch::channel(initial);
        let sender = Arc::new(sender);
        let shared = Arc::new(AuthorityShared {
            observed_finalized_height: AtomicU64::new(0),
            applied_finalized_height: AtomicU64::new(0),
            refresh_generation: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            refresh: Mutex::new(None),
            refresh_gate: Mutex::new(()),
            refresh_owner_thread: Mutex::new(None),
            publish_gate: Mutex::new(()),
            guard_wakeup: tokio::sync::Notify::new(),
            sender: Arc::downgrade(&sender),
        });
        Self { sender, shared }
    }

    /// Replaces the snapshot even when no subscribers are currently present.
    pub fn publish(&self, state: TeeAuthorityState) {
        self.sender.send_replace(state);
    }

    /// Subscribes to the current snapshot and subsequent reversible transitions.
    pub fn subscribe(&self) -> TeeAuthorityWatch {
        TeeAuthorityWatch {
            receiver: self.sender.subscribe(),
            shared: Arc::clone(&self.shared),
        }
    }

    /// Copies the current verdict without retaining a watch read lock.
    pub fn snapshot(&self) -> TeeAuthorityState {
        *self.sender.borrow()
    }

    /// Records a finalized height the admission owner has not applied yet.
    pub fn note_observed_finalized_height(&self, height: u64) {
        note_monotonic(&self.shared.observed_finalized_height, height);
    }

    /// Records the finalized height whose admission verdict is already published.
    pub fn note_applied_finalized_height(&self, height: u64) {
        note_monotonic(&self.shared.applied_finalized_height, height);
    }

    /// Highest finalized height noted for this publisher.
    pub fn observed_finalized_height(&self) -> u64 {
        self.shared
            .observed_finalized_height
            .load(Ordering::Acquire)
    }

    /// Highest finalized height applied to the published verdict.
    pub fn applied_finalized_height(&self) -> u64 {
        self.shared.applied_finalized_height.load(Ordering::Acquire)
    }

    /// One-permit wakeup for lease-guard polling. Lifecycle fan-out stays on the shared notifier.
    pub fn guard_notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.shared.guard_wakeup.notified()
    }

    /// Holds the door-refresh lock. A caller that already holds it must not call this again.
    pub fn lock_refresh_gate(&self) -> impl Drop + '_ {
        self.shared.lock_refresh_gate()
    }

    /// Generation captured by the refresh that is allowed to publish.
    pub fn refresh_generation(&self) -> u64 {
        self.shared.refresh_generation.load(Ordering::Acquire)
    }

    /// Accepts `sampled` only while that generation is still an active guard.
    ///
    /// Sample before waiting on [`Self::lock_refresh_gate`]. A stop refuses both
    /// the sampled generation and the generation stored by the stop. Generation
    /// zero remains publishable until the first guard is installed. Only a new
    /// [`Self::install_sync_refresh`] accepts a generation again.
    pub fn accept_sampled_refresh(&self, sampled: u64) -> Option<u64> {
        let _publish = lock_mutex(&self.shared.publish_gate);
        if self.shared.retired.load(Ordering::Acquire) {
            return None;
        }
        let current = self.shared.refresh_generation.load(Ordering::Acquire);
        (current == sampled).then_some(current)
    }

    /// Publishes only when `generation` is still the installed guard generation.
    pub fn publish_if_generation(&self, generation: u64, state: TeeAuthorityState) -> bool {
        let _gate = lock_mutex(&self.shared.publish_gate);
        if self.shared.retired.load(Ordering::Acquire)
            || self.shared.refresh_generation.load(Ordering::Acquire) != generation
        {
            return false;
        }
        if *self.sender.borrow() != state {
            self.sender.send_replace(state);
        }
        true
    }

    /// Publishes closure for `generation` and keeps a resident execution key.
    pub fn publish_failure_if_generation(&self, generation: u64, has_resident: bool) -> bool {
        let _gate = lock_mutex(&self.shared.publish_gate);
        if self.shared.retired.load(Ordering::Acquire)
            || self.shared.refresh_generation.load(Ordering::Acquire) != generation
        {
            return false;
        }
        let current = *self.sender.borrow();
        let closed = failure_state(current, has_resident);
        if current != closed {
            self.sender.send_replace(closed);
        }
        true
    }

    /// Installs the admission owner's synchronous refresh.
    ///
    /// The hook receives its generation for [`Self::publish_if_generation`].
    pub fn install_sync_refresh(&self, hook: impl Fn(u64) + Send + Sync + 'static) {
        let _publish = lock_mutex(&self.shared.publish_gate);
        self.shared.retired.store(false, Ordering::Release);
        let generation = self
            .shared
            .refresh_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        *lock_mutex(&self.shared.refresh) = Some(InstalledRefresh {
            generation,
            hook: Arc::new(hook),
        });
    }

    /// Drops the synchronous refresh and invalidates an in-flight generation.
    pub fn clear_sync_refresh(&self) {
        let _publish = lock_mutex(&self.shared.publish_gate);
        self.retire_generation();
    }

    /// Caller holds `publish_gate`. Marks the generation retired and drops the hook.
    fn retire_generation(&self) {
        self.shared.retired.store(true, Ordering::Release);
        self.shared
            .refresh_generation
            .fetch_add(1, Ordering::AcqRel);
        *lock_mutex(&self.shared.refresh) = None;
    }

    /// Stops the guard: invalidates its generation, then publishes closure under the same lock.
    pub fn retire_publisher(&self, has_resident: bool) {
        let _publish = lock_mutex(&self.shared.publish_gate);
        self.retire_generation();
        let current = *self.sender.borrow();
        let closed = failure_state(current, has_resident);
        if current != closed {
            self.sender.send_replace(closed);
        }
    }
}

impl std::fmt::Debug for TeeAuthorityChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeeAuthorityChannel")
            .field("state", &self.snapshot())
            .field(
                "observed_finalized_height",
                &self.observed_finalized_height(),
            )
            .field("applied_finalized_height", &self.applied_finalized_height())
            .finish()
    }
}

impl Default for TeeAuthorityChannel {
    fn default() -> Self {
        Self::new(TeeAuthorityState::default())
    }
}

/// Read-only, cloneable view for consensus, private RPC and key-transfer gates.
#[derive(Clone)]
pub struct TeeAuthorityWatch {
    receiver: watch::Receiver<TeeAuthorityState>,
    shared: Arc<AuthorityShared>,
}

impl TeeAuthorityWatch {
    /// Copies the latest verdict; losing the publisher closes authority, not execution.
    pub fn snapshot(&self) -> TeeAuthorityState {
        let state = *self.receiver.borrow();
        if self.receiver.has_changed().is_err() {
            closed_without_publisher(state)
        } else {
            state
        }
    }

    /// Reads the provider whenever a refresh hook is installed.
    ///
    /// A second caller waits on the refresh lock and then reads again. A lagging
    /// provider, or a newer observed height that arrived during the read, does
    /// not return the previous `Armed` verdict. A missing hook closes authority
    /// when the observed height is ahead and keeps a resident execution key.
    pub fn ensure_current(&self) -> TeeAuthorityState {
        if *lock_mutex(&self.shared.refresh_owner_thread) == Some(thread::current().id()) {
            return self.snapshot();
        }
        let _gate = self.shared.lock_refresh_gate();
        self.converge()
    }

    fn converge(&self) -> TeeAuthorityState {
        for pass in 0..2 {
            let installed = lock_mutex(&self.shared.refresh).clone();
            let Some(installed) = installed else {
                return self.snapshot_or_close_lag();
            };
            let generation = installed.generation;
            (installed.hook)(generation);
            if self.applied_finalized_height() >= self.observed_finalized_height() {
                return self.snapshot();
            }
            if self.refresh_generation() != generation {
                return self.snapshot();
            }
            if pass == 1 {
                self.close_lagging_authority(Some(generation));
            }
        }
        self.snapshot()
    }

    fn refresh_generation(&self) -> u64 {
        self.shared.refresh_generation.load(Ordering::Acquire)
    }

    fn snapshot_or_close_lag(&self) -> TeeAuthorityState {
        if self.observed_finalized_height() > self.applied_finalized_height() {
            self.close_lagging_authority(None);
        }
        self.snapshot()
    }

    /// Records a finalized height the admission owner has not applied yet.
    pub fn note_observed_finalized_height(&self, height: u64) {
        note_monotonic(&self.shared.observed_finalized_height, height);
    }

    /// Highest finalized height noted for this watch.
    pub fn observed_finalized_height(&self) -> u64 {
        self.shared
            .observed_finalized_height
            .load(Ordering::Acquire)
    }

    /// Highest finalized height applied to the published verdict.
    pub fn applied_finalized_height(&self) -> u64 {
        self.shared.applied_finalized_height.load(Ordering::Acquire)
    }

    /// Waits for a publication; publisher shutdown retains the typed watch error.
    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.receiver.changed().await
    }

    fn close_lagging_authority(&self, generation: Option<u64>) {
        let current = self.snapshot();
        if !current.is_authority_open() {
            return;
        }
        let Some(sender) = self.shared.sender.upgrade() else {
            return;
        };
        let closed = closed_without_publisher(current);
        if let Some(generation) = generation {
            TeeAuthorityChannel {
                sender,
                shared: Arc::clone(&self.shared),
            }
            .publish_if_generation(generation, closed);
        } else {
            // No hook owns this fallback verdict.
            sender.send_replace(closed);
        }
    }

    fn wake_guard(&self) {
        self.shared.guard_wakeup.notify_one();
    }
}

impl std::fmt::Debug for TeeAuthorityWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeeAuthorityWatch")
            .field("state", &self.snapshot())
            .field(
                "observed_finalized_height",
                &self.observed_finalized_height(),
            )
            .field("applied_finalized_height", &self.applied_finalized_height())
            .finish()
    }
}

fn note_monotonic(slot: &AtomicU64, height: u64) {
    slot.fetch_max(height, Ordering::Release);
}

fn failure_state(current: TeeAuthorityState, has_resident: bool) -> TeeAuthorityState {
    if has_resident || current.has_verified_resident_key() {
        TeeAuthorityState::SuspendedPolicyMismatch
    } else {
        TeeAuthorityState::UnkeyedAwaitingAdmission
    }
}

fn closed_without_publisher(state: TeeAuthorityState) -> TeeAuthorityState {
    if state.has_verified_resident_key() {
        TeeAuthorityState::SuspendedPolicyMismatch
    } else {
        TeeAuthorityState::UnkeyedAwaitingAdmission
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// Notes the finalized height and stores one lease-guard permit on the installed watch.
pub fn note_finalized_authority_revision(height: u64) {
    if let Some(watch) = tee_authority_watch() {
        watch.note_observed_finalized_height(height);
        watch.wake_guard();
    }
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
    use std::sync::atomic::AtomicBool;

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

    #[test]
    fn lagged_finalized_head_refreshes_or_closes_before_the_cached_verdict() {
        let channel = TeeAuthorityChannel::new(ARMED);
        let watch = channel.subscribe();
        channel.note_applied_finalized_height(4);
        let verdict = std::sync::Arc::new(std::sync::Mutex::new(ARMED));
        let published = std::sync::Arc::clone(&verdict);
        let publisher = channel.clone();
        channel.install_sync_refresh(move |generation| {
            let state = *published.lock().expect("verdict lock");
            assert!(publisher.publish_if_generation(generation, state));
            publisher.note_applied_finalized_height(publisher.observed_finalized_height());
        });
        assert!(watch.ensure_current().is_authority_open());
        for (height, closed) in [
            (5, TeeAuthorityState::SuspendedJailed),
            (6, TeeAuthorityState::SuspendedLeaseExpired),
        ] {
            *verdict.lock().expect("verdict lock") = closed;
            channel.note_observed_finalized_height(height);
            let state = watch.ensure_current();
            assert_eq!(state, closed);
            assert!(state.has_verified_resident_key());
            assert!(!state.is_authority_open());
            assert_eq!(watch.snapshot(), closed);
        }
        for hooked in [false, true] {
            let lagging = TeeAuthorityChannel::new(ARMED);
            if hooked {
                lagging.install_sync_refresh(|_| {});
            }
            lagging.note_applied_finalized_height(1);
            lagging.note_observed_finalized_height(2);
            let closed = lagging.subscribe().ensure_current();
            assert_eq!(closed, TeeAuthorityState::SuspendedPolicyMismatch);
            assert!(closed.has_verified_resident_key());
            assert!(!closed.is_authority_open());
            assert_eq!(lagging.applied_finalized_height(), 1);
        }
    }

    #[test]
    fn installed_watch_records_a_monotonic_finalized_revision() {
        let _guard = tee_authority_test_guard();
        let channel = TeeAuthorityChannel::new(ARMED);
        install_tee_authority_watch(channel.subscribe()).unwrap();
        note_finalized_authority_revision(12);
        assert_eq!(channel.observed_finalized_height(), 12);
        note_finalized_authority_revision(11);
        assert_eq!(channel.observed_finalized_height(), 12);
        clear_tee_authority_watch_for_test();
        note_finalized_authority_revision(20);
        assert_eq!(channel.observed_finalized_height(), 12);
    }

    #[test]
    fn concurrent_reader_waits_until_refresh_publishes_the_closed_verdict() {
        use std::sync::{Barrier, Mutex};
        use std::time::Duration;
        let channel = TeeAuthorityChannel::new(ARMED);
        let watch = channel.subscribe();
        channel.note_applied_finalized_height(1);
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let publisher = channel.clone();
        let started_hook = Arc::clone(&started);
        let release_hook = Arc::clone(&release);
        let first_pass = Arc::new(AtomicBool::new(true));
        channel.install_sync_refresh(move |generation| {
            if first_pass.swap(false, Ordering::AcqRel) {
                started_hook.wait();
                release_hook.wait();
            }
            assert!(publisher.publish_if_generation(generation, TeeAuthorityState::SuspendedJailed));
            publisher.note_applied_finalized_height(publisher.observed_finalized_height());
        });
        channel.note_observed_finalized_height(2);
        let first = watch.clone();
        let reader = std::thread::spawn(move || first.ensure_current());
        started.wait();
        let second = watch.clone();
        let waiting = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&waiting);
        let concurrent = std::thread::spawn(move || {
            let state = second.ensure_current();
            *slot.lock().expect("verdict slot") = Some(state);
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(waiting.lock().expect("verdict slot").is_none());
        release.wait();
        assert_eq!(
            reader.join().expect("reader"),
            TeeAuthorityState::SuspendedJailed
        );
        concurrent.join().expect("concurrent reader");
        let state = waiting.lock().expect("verdict slot").expect("verdict");
        assert_eq!(state, TeeAuthorityState::SuspendedJailed);
        assert!(state.has_verified_resident_key());
        assert!(!state.is_authority_open());
    }

    #[test]
    fn provider_lag_or_a_newer_observed_height_does_not_return_stale_armed() {
        use std::sync::atomic::AtomicBool;
        let channel = TeeAuthorityChannel::new(ARMED);
        let watch = channel.subscribe();
        let provider_height = Arc::new(AtomicU64::new(5));
        let passes = Arc::new(AtomicU64::new(0));
        let publisher = channel.clone();
        let height = Arc::clone(&provider_height);
        let seen = Arc::clone(&passes);
        channel.install_sync_refresh(move |generation| {
            let pass = seen.fetch_add(1, Ordering::AcqRel);
            let head = height.load(Ordering::Acquire);
            if pass == 0 {
                assert!(publisher.publish_if_generation(generation, ARMED));
                publisher.note_applied_finalized_height(head);
                publisher.note_observed_finalized_height(head + 1);
                height.store(head + 1, Ordering::Release);
                return;
            }
            assert!(publisher.publish_if_generation(generation, TeeAuthorityState::SuspendedJailed));
            publisher.note_applied_finalized_height(height.load(Ordering::Acquire));
        });
        channel.note_applied_finalized_height(4);
        channel.note_observed_finalized_height(5);
        let state = watch.ensure_current();
        assert_eq!(state, TeeAuthorityState::SuspendedJailed);
        assert!(state.has_verified_resident_key());
        assert!(passes.load(Ordering::Acquire) >= 2);

        let channel = TeeAuthorityChannel::new(ARMED);
        let watch = channel.subscribe();
        channel.note_applied_finalized_height(7);
        channel.note_observed_finalized_height(7);
        let provider_height = Arc::new(AtomicU64::new(8));
        let jailed = Arc::new(AtomicBool::new(true));
        let publisher = channel.clone();
        let height = Arc::clone(&provider_height);
        let closed = Arc::clone(&jailed);
        channel.install_sync_refresh(move |generation| {
            let head = height.load(Ordering::Acquire);
            if head < publisher.observed_finalized_height() {
                return;
            }
            let state = if closed.load(Ordering::Acquire) {
                TeeAuthorityState::SuspendedJailed
            } else {
                ARMED
            };
            assert!(publisher.publish_if_generation(generation, state));
            publisher.note_applied_finalized_height(head);
        });
        let state = watch.ensure_current();
        assert_eq!(state, TeeAuthorityState::SuspendedJailed);
        assert_eq!(channel.applied_finalized_height(), 8);
    }

    #[test]
    fn nested_refresh_reuses_only_the_same_channels_snapshot() {
        let outer = TeeAuthorityChannel::new(ARMED);
        let inner = TeeAuthorityChannel::new(ARMED);
        let outer_watch = outer.subscribe();
        let inner_watch = inner.subscribe();
        let publisher = inner.clone();
        inner.install_sync_refresh(move |generation| {
            assert!(publisher.publish_if_generation(generation, TeeAuthorityState::SuspendedJailed));
        });
        let outer_reentry = outer_watch.clone();
        let (send, receive) = std::sync::mpsc::channel();
        outer.install_sync_refresh(move |_| {
            let states = (outer_reentry.ensure_current(), inner_watch.ensure_current());
            send.send(states).unwrap();
        });
        let reader = std::thread::spawn(move || outer_watch.ensure_current());
        let states = receive.recv_timeout(std::time::Duration::from_secs(5));
        outer.clear_sync_refresh();
        inner.clear_sync_refresh();
        assert_eq!(states.unwrap(), (ARMED, TeeAuthorityState::SuspendedJailed));
        assert_eq!(reader.join().unwrap(), ARMED);
    }

    #[test]
    fn stopped_generation_cannot_republish_armed() {
        use std::sync::Barrier;
        let channel = TeeAuthorityChannel::new(ARMED);
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let publisher = channel.clone();
        let started_hook = Arc::clone(&started);
        let release_hook = Arc::clone(&release);
        channel.install_sync_refresh(move |generation| {
            started_hook.wait();
            release_hook.wait();
            assert!(!publisher.publish_if_generation(generation, ARMED));
        });
        channel.note_observed_finalized_height(3);
        let watch = channel.subscribe();
        let reader = std::thread::spawn(move || watch.ensure_current());
        started.wait();
        channel.retire_publisher(true);
        release.wait();
        let state = reader.join().expect("reader");
        assert!(!state.is_authority_open());
        assert!(state.has_verified_resident_key());
        assert!(!channel.snapshot().is_authority_open());
        assert!(!channel.publish_if_generation(1, ARMED));
    }

    #[test]
    fn retired_refresh_rejects_the_sampled_generation_and_the_generation_after_stop() {
        let channel = TeeAuthorityChannel::new(ARMED);
        assert_eq!(channel.accept_sampled_refresh(0), Some(0));
        channel.install_sync_refresh(|_| {});
        let active = channel.refresh_generation();
        assert_eq!(channel.accept_sampled_refresh(active), Some(active));
        assert!(channel.accept_sampled_refresh(0).is_none());
        channel.retire_publisher(true);
        assert!(channel.accept_sampled_refresh(active).is_none());
        assert!(channel
            .accept_sampled_refresh(channel.refresh_generation())
            .is_none());
        assert!(!channel.snapshot().is_authority_open());
        assert!(channel.snapshot().has_verified_resident_key());
        channel.install_sync_refresh(|_| {});
        let restarted = channel.refresh_generation();
        assert_eq!(channel.accept_sampled_refresh(restarted), Some(restarted));
        assert!(channel.accept_sampled_refresh(active).is_none());
    }
}
