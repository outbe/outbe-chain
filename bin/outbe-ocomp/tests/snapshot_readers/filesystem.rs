use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

pub(super) fn fingerprint(root: &Path) -> Vec<(std::path::PathBuf, u32, Vec<u8>)> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<(std::path::PathBuf, u32, Vec<u8>)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else if metadata.file_type().is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            vec![]
        };
        entries.push((
            path.strip_prefix(root).unwrap().to_path_buf(),
            metadata.permissions().mode(),
            bytes,
        ));
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = vec![];
    visit(root, root, &mut entries);
    entries.sort();
    entries
}
