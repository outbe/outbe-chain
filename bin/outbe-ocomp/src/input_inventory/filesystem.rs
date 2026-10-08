use super::*;

pub(super) fn digest_file_observing(
    path: &Path,
    on_progress: &impl Fn(),
) -> Result<B256, TributeInventoryError> {
    let mut file = open_regular_readonly(path)?;
    let mut hasher = Keccak256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error("hash inventory file", path, source))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        on_progress();
    }
    Ok(B256::from_slice(&hasher.finalize()))
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), TributeInventoryError> {
    reject_symlink_ancestors(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(TributeInventoryError::UnsafePath(path.to_path_buf()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|source| io_error("create inventory directory", path, source))?;
        }
        Err(source) => return Err(io_error("inspect inventory directory", path, source)),
    }
    reject_symlink_ancestors(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
        .map_err(|source| io_error("set inventory directory permissions", path, source))
}

pub(super) fn inspect_private_directory(path: &Path) -> Result<(), TributeInventoryError> {
    reject_symlink_ancestors(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect inventory directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TributeInventoryError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

pub(super) fn reject_symlink_ancestors(path: &Path) -> Result<(), TributeInventoryError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(TributeInventoryError::UnsafePath(ancestor.to_path_buf()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("inspect inventory ancestor", ancestor, source)),
        }
    }
    Ok(())
}

pub(super) fn open_regular_readonly(path: &Path) -> Result<File, TributeInventoryError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect inventory file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TributeInventoryError::UnsafePath(path.to_path_buf()));
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open inventory file", path, source))
}

pub(super) fn read_exact_file(
    path: &Path,
    expected: usize,
) -> Result<Vec<u8>, TributeInventoryError> {
    let mut file = open_regular_readonly(path)?;
    let actual = usize::try_from(
        file.metadata()
            .map_err(|source| io_error("stat inventory file", path, source))?
            .len(),
    )
    .map_err(|_| TributeInventoryError::IntegerOverflow)?;
    if actual != expected {
        return Err(TributeInventoryError::Corrupt("inventory file length"));
    }
    let mut bytes = vec![0_u8; expected];
    file.read_exact(&mut bytes)
        .map_err(|source| io_error("read inventory file", path, source))?;
    Ok(bytes)
}

pub(super) fn persist_new(path: &Path, bytes: &[u8]) -> Result<(), TributeInventoryError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
        .map_err(|source| io_error("create inventory file", path, source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("persist inventory file", path, source))
}

pub(super) fn persist_atomic(
    root: &Path,
    path: &Path,
    bytes: &[u8],
) -> Result<(), TributeInventoryError> {
    let temp = path.with_extension("tmp");
    if path_exists(&temp)? {
        let metadata = fs::symlink_metadata(&temp)
            .map_err(|source| io_error("inspect inventory temp", &temp, source))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(TributeInventoryError::UnsafePath(temp));
        }
        fs::remove_file(&temp)
            .map_err(|source| io_error("remove inventory temp", &temp, source))?;
    }
    persist_new(&temp, bytes)?;
    fs::rename(&temp, path).map_err(|source| io_error("install inventory file", path, source))?;
    sync_directory(root)
}

pub(super) fn remove_owned_build_directory(path: &Path) -> Result<(), TributeInventoryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(TributeInventoryError::UnsafePath(path.to_path_buf()))
        }
        Ok(_) => {
            fs::remove_dir_all(path)
                .map_err(|source| io_error("remove incomplete inventory build", path, source))?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect inventory build directory", path, source)),
    }
}

pub(super) fn recover_unsealed_inventory(root: &Path) -> Result<(), TributeInventoryError> {
    remove_owned_build_directory(&root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY))?;
    let mut removed = false;
    for path in [
        root.join(OWNERS_FILE),
        root.join(ISOS_FILE),
        root.join(BODIES_FILE),
        root.join(format!("{OWNERS_FILE}.tmp")),
        root.join(format!("{ISOS_FILE}.tmp")),
        root.join(format!("{BODIES_FILE}.tmp")),
        root.join(HEADER_FILE).with_extension("tmp"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(TributeInventoryError::UnsafePath(path));
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|source| {
                    io_error("remove incomplete inventory file", &path, source)
                })?;
                removed = true;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(io_error("inspect incomplete inventory file", &path, source));
            }
        }
    }
    if removed {
        sync_directory(root)?;
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> Result<(), TributeInventoryError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error("fsync inventory directory", path, source))
}

pub(super) fn path_exists(path: &Path) -> Result<bool, TributeInventoryError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect inventory path", path, source)),
    }
}

pub(super) struct InventoryLock {
    file: File,
}

pub(super) fn open_inventory_lock_file(root: &Path) -> Result<File, TributeInventoryError> {
    let path = root.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|source| io_error("open inventory lock", &path, source))?;
    Ok(file)
}

impl InventoryLock {
    #[allow(unsafe_code)]
    pub(super) fn acquire(root: &Path) -> Result<Self, TributeInventoryError> {
        let file = open_inventory_lock_file(root)?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(TributeInventoryError::Locked);
        }
        Ok(Self { file })
    }
}

impl Drop for InventoryLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` remains open for the complete flock call.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
