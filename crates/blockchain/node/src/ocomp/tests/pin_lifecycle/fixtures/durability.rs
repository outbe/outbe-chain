use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum FailSync {
    File,
    Directory,
}

pub(in super::super) struct FailOnceDurability {
    point: FailSync,
    armed: AtomicBool,
    failed: AtomicBool,
}

impl FailOnceDurability {
    pub(in super::super) fn at(point: FailSync) -> Self {
        Self {
            point,
            armed: AtomicBool::new(true),
            failed: AtomicBool::new(false),
        }
    }

    pub(in super::super) fn disarmed(point: FailSync) -> Self {
        Self {
            point,
            armed: AtomicBool::new(false),
            failed: AtomicBool::new(false),
        }
    }

    pub(in super::super) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
}

impl FsyncFailurePolicy for FailOnceDurability {
    const FILE_ERROR: &'static str = "injected file fsync failure";
    const DIRECTORY_ERROR: &'static str = "injected directory fsync failure";
    fn should_fail(&self, point: FailSync) -> bool {
        self.point == point
            && self.armed.load(Ordering::SeqCst)
            && !self.failed.swap(true, Ordering::SeqCst)
    }
}

/// Fault decision and diagnostic owned by each independent test policy.
pub(in super::super) trait FsyncFailurePolicy: Send + Sync {
    const FILE_ERROR: &'static str;
    const DIRECTORY_ERROR: &'static str;
    fn should_fail(&self, point: FailSync) -> bool;
}

impl<P: FsyncFailurePolicy> JournalDurability for P {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        if self.should_fail(FailSync::File) {
            return Err(io::Error::other(P::FILE_ERROR));
        }
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        if self.should_fail(FailSync::Directory) {
            return Err(io::Error::other(P::DIRECTORY_ERROR));
        }
        directory.sync_all()
    }
}
