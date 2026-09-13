use super::*;

pub(super) struct ThreadDropRecorder {
    pub(super) runner_returned: Arc<AtomicBool>,
    pub(super) dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

impl Drop for ThreadDropRecorder {
    fn drop(&mut self) {
        assert!(
            self.runner_returned.load(Ordering::SeqCst),
            "the lifetime pin must outlive the runner"
        );
        *self.dropped_on.lock().expect("drop recorder lock") = Some(std::thread::current().id());
    }
}

pub(super) struct ExecutionTeardownSentinel(pub(super) Arc<AtomicBool>);

pub(super) fn full_node_admission_anchor() -> super::LocalTeeAdmissionAnchorV1 {
    super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: alloy_primitives::B256::repeat_byte(0x77),
    }
}

impl Drop for ExecutionTeardownSentinel {
    fn drop(&mut self) {
        assert!(
            self.0.load(Ordering::SeqCst),
            "consensus must be joined before execution resources are torn down"
        );
    }
}
