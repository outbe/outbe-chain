use super::canonical_json;
use super::release_platform;
use super::Sha256Digest;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;
use filetime::FileTime;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sha2::Digest as _;
use sha2::Sha256;

use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::BufReader;
use std::io::Read;
use std::io::Write as _;

use std::path::Component;
use std::path::Path;

use walkdir::WalkDir;

pub(super) fn file_artifact(
    path: &Path,
    name: &str,
    kind: &str,
    media_type: &str,
    tee: Value,
) -> Result<Value> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            eyre!(
                "release artifact needs a UTF-8 file name: {}",
                path.display()
            )
        })?;
    require_nonempty_regular_file(path, name)?;
    let metadata = fs::metadata(path)?;
    Ok(serde_json::json!({
        "classification": "production",
        "digest": file_digest(path)?,
        "features": [],
        "install_profiles": ["full-node", "validator"],
        "kind": kind,
        "media_type": media_type,
        "name": name,
        "network_compatibility": "network-manifest-required",
        "package": "outbe-tee-enclave",
        "path": format!("release/{file_name}"),
        "platform": release_platform(),
        "role": "tee-enclave",
        "size": metadata.len(),
        "tee": tee
    }))
}

pub(super) fn require_nonempty_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).wrap_err_with(|| format!("read {label}: {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        bail!(
            "{label} must be a non-empty regular file: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn file_digest(path: &Path) -> Result<Sha256Digest> {
    let file =
        File::open(path).wrap_err_with(|| format!("open for hashing: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .wrap_err_with(|| format!("hash file: {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256Digest {
        algorithm: "sha256".to_owned(),
        value: hex::encode(hasher.finalize()),
    })
}

pub(super) fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn verify_checksums(root: &Path, name: &str) -> Result<()> {
    let checksum_path = root.join(name);
    let content = fs::read_to_string(&checksum_path)
        .wrap_err_with(|| format!("read checksums: {}", checksum_path.display()))?;
    if content.is_empty() {
        bail!("checksum file is empty: {}", checksum_path.display());
    }
    for (index, line) in content.lines().enumerate() {
        let Some((digest, relative)) = line.split_once("  ") else {
            bail!(
                "invalid checksum row {} in {}",
                index + 1,
                checksum_path.display()
            );
        };
        if !is_lower_hex(digest, 64) {
            bail!("invalid checksum digest at row {}", index + 1);
        }
        let relative = safe_relative_path(relative)?;
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .wrap_err_with(|| format!("checksum input is missing: {}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!(
                "checksum input is not a safe regular file: {}",
                path.display()
            );
        }
        if file_digest(&path)?.value != digest {
            bail!("checksum mismatch: {}", path.display());
        }
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe relative artifact path: {value}");
    }
    Ok(path)
}

pub(super) fn write_checksums(root: &Path, name: &str) -> Result<()> {
    let mut rows = Vec::new();
    for item in WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let item = item.wrap_err("walk output for checksums")?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)
            .wrap_err("derive checksum path")?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(path).wrap_err("read checksum input metadata")?;
        if metadata.file_type().is_symlink() {
            bail!("output contains symlink: {relative}");
        }
        if metadata.is_file() && relative != name {
            rows.push((relative, file_digest(path)?.value));
        }
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    let content = rows
        .into_iter()
        .map(|(path, digest)| format!("{digest}  {path}\n"))
        .collect::<String>();
    fs::write(root.join(name), content).wrap_err("write output checksums")
}

pub(super) fn write_canonical<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("create metadata directory: {}", parent.display()))?;
    }
    fs::write(path, canonical_json(value)?)
        .wrap_err_with(|| format!("write canonical metadata: {}", path.display()))
}

pub(super) fn write_new_file(path: &Path, bytes: &[u8], label: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("create {label} directory: {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create new {label}: {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .wrap_err_with(|| format!("write {label}: {}", path.display()))
}

pub(super) fn read_canonical_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("read JSON metadata: {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("missing or unsafe JSON input: {}", path.display());
    }
    let bytes = fs::read(path).wrap_err_with(|| format!("read JSON input: {}", path.display()))?;
    let value: T = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("parse JSON input: {}", path.display()))?;
    if bytes != canonical_json(&value)? {
        bail!(
            "JSON input is not canonical outbe-canonical-json-v1: {}",
            path.display()
        );
    }
    Ok(value)
}

pub(super) fn normalize_tree_mtime(root: &Path, source_date_epoch: i64) -> Result<()> {
    if source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    let timestamp = FileTime::from_unix_time(source_date_epoch, 0);
    let mut entries = WalkDir::new(root)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .wrap_err("walk output for timestamp normalization")?;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));
    for entry in entries {
        let metadata = fs::symlink_metadata(entry.path()).wrap_err("read timestamp target")?;
        if metadata.file_type().is_symlink() {
            bail!(
                "cannot normalize symlink timestamp: {}",
                entry.path().display()
            );
        }
        filetime::set_file_times(entry.path(), timestamp, timestamp)
            .wrap_err_with(|| format!("normalize timestamp: {}", entry.path().display()))?;
    }
    Ok(())
}
