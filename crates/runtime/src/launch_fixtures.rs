use super::*;

pub(crate) struct ThreadDropRecorder {
    pub(crate) runner_returned: Arc<AtomicBool>,
    pub(crate) dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
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

pub(crate) struct ExecutionTeardownSentinel(pub(crate) Arc<AtomicBool>);

pub(crate) fn full_node_admission_anchor() -> super::LocalTeeAdmissionAnchorV1 {
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
