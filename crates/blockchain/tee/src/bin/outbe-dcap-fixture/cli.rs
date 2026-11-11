//! Exact host fixture CLI parsing and output primitives shared by prepare and assemble.

use std::{fs, path::Path};

use outbe_tee::errors::DcapFixtureError;

pub(super) fn argument(
    arguments: &[String],
    name: &'static str,
) -> Result<String, DcapFixtureError> {
    let index = arguments
        .iter()
        .position(|argument| argument == name)
        .ok_or_else(|| DcapFixtureError::MissingArgument(name.to_owned()))?;
    arguments
        .get(index + 1)
        .cloned()
        .ok_or_else(|| DcapFixtureError::MissingArgumentValue(name.to_owned()))
}

pub(super) fn parse_u64(value: &str, label: &'static str) -> Result<u64, DcapFixtureError> {
    value.parse().map_err(DcapFixtureError::invalid_u64(label))
}

pub(super) fn ensure_empty_directory(path: &Path) -> Result<(), DcapFixtureError> {
    if path.exists() {
        let mut entries = fs::read_dir(path).map_err(DcapFixtureError::read_path(path))?;
        if entries.next().is_some() {
            return Err(DcapFixtureError::OutputDirectoryNotEmpty(
                path.display().to_string(),
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(DcapFixtureError::create_path(path))
}

pub(super) fn write_new(path: &Path, value: &[u8]) -> Result<(), DcapFixtureError> {
    use std::io::Write;

    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(DcapFixtureError::create_path(path))?;
    output
        .write_all(value)
        .map_err(DcapFixtureError::write_path(path))
}
