use commonware_cryptography::{Hasher as _, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub(in crate::stack::tests) fn inventory(
    root: &Path,
) -> BTreeMap<PathBuf, (bool, u32, u64, Vec<u8>)> {
    fn visit(base: &Path, path: &Path, rows: &mut BTreeMap<PathBuf, (bool, u32, u64, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(!metadata.file_type().is_symlink());
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = u32::from(metadata.permissions().readonly());
            if metadata.is_dir() {
                rows.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    (true, mode, 0, vec![]),
                );
                visit(base, &path, rows);
            } else {
                let mut hasher = Sha256::default();
                let mut file = fs::File::open(&path).unwrap();
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let n = file.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buffer[..n]);
                }
                rows.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    (
                        false,
                        mode,
                        metadata.len(),
                        hasher.finalize().1.as_ref().to_vec(),
                    ),
                );
            }
        }
    }
    let mut rows = BTreeMap::new();
    visit(root, root, &mut rows);
    rows
}

fn copy_entries(source: &Path, destination: &Path) {
    assert!(source.is_dir());
    assert!(!destination.exists());
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from).unwrap();
        assert!(!metadata.file_type().is_symlink());
        if metadata.is_dir() {
            copy_entries(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
            fs::set_permissions(&to, metadata.permissions()).unwrap();
        }
    }
    fs::set_permissions(destination, fs::metadata(source).unwrap().permissions()).unwrap();
}

pub(super) fn copy_tree(source: &Path, destination: &Path) {
    copy_entries(source, destination);
    // Hash each native file once per side, not again at every parent directory.
    assert_eq!(inventory(source), inventory(destination));
}
