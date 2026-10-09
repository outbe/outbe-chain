//! Exact host fixture CLI parsing and output primitives shared by prepare and assemble.

use std::{fs, path::Path};

pub(super) fn argument(arguments: &[String], name: &str) -> Result<String, String> {
    let index = arguments
        .iter()
        .position(|argument| argument == name)
        .ok_or_else(|| format!("missing {name}"))?;
    arguments
        .get(index + 1)
        .cloned()
        .ok_or_else(|| format!("missing value for {name}"))
}

pub(super) fn parse_u64(value: &str, label: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{label} is not a canonical u64"))
}

pub(super) fn ensure_empty_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        let mut entries =
            fs::read_dir(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if entries.next().is_some() {
            return Err(format!("output directory is not empty: {}", path.display()));
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|error| format!("create {}: {error}", path.display()))
}

pub(super) fn write_new(path: &Path, value: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    output
        .write_all(value)
        .map_err(|error| format!("write {}: {error}", path.display()))
}
