//! Immutable evidence publication shared by native domain packs.
use super::MemberDigestV1;
use eyre::{ensure, Result, WrapErr};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn publish_member(root: &Path, relative: &str, bytes: &[u8]) -> Result<MemberDigestV1> {
    let target = checked_target(root, relative)?;
    atomic_publish(&target, bytes)?;
    Ok(MemberDigestV1 {
        path: relative.to_owned(),
        length: bytes
            .len()
            .try_into()
            .wrap_err("member length does not fit u64")?,
        sha256: sha256_hex(bytes),
    })
}

pub fn publish_manifest<T: serde::Serialize + ?Sized>(
    root: &Path,
    manifest: &T,
) -> Result<PathBuf> {
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    let path = root.join("run-manifest.json");
    atomic_publish(&path, &bytes)?;
    Ok(path)
}

pub fn hash_file(path: &Path) -> Result<MemberDigestV1> {
    let bytes = fs::read(path).wrap_err_with(|| format!("read {}", path.display()))?;
    Ok(MemberDigestV1 {
        path: path.to_string_lossy().into_owned(),
        length: bytes
            .len()
            .try_into()
            .wrap_err("file length does not fit u64")?,
        sha256: sha256_hex(&bytes),
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn checked_target(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    ensure!(
        !relative_path.is_absolute(),
        "evidence member path is absolute"
    );
    ensure!(
        relative_path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_))),
        "evidence member path contains traversal or prefix"
    );
    ensure!(
        relative != "run-manifest.json"
            && relative != "closure-report.json"
            && relative != "closure-report.md"
            && relative != "closure-report.sha256",
        "reserved evidence path {relative}"
    );
    Ok(root.join(relative_path))
}

fn atomic_publish(target: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        !target.exists(),
        "refusing to replace immutable evidence {}",
        target.display()
    );
    let parent = target
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence target has no parent"))?;
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create evidence directory {}", parent.display()))?;

    let temporary = temporary_path(target);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .wrap_err_with(|| format!("create temporary evidence {}", temporary.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)
            .wrap_err_with(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .wrap_err_with(|| format!("fsync {}", temporary.display()))?;
        drop(file);
        fs::hard_link(&temporary, target).wrap_err_with(|| {
            format!(
                "link immutable evidence {} -> {} without replacement",
                temporary.display(),
                target.display()
            )
        })?;
        fs::remove_file(&temporary)
            .wrap_err_with(|| format!("remove published temporary {}", temporary.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(target: &Path) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence))
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .wrap_err_with(|| format!("fsync directory {}", path.display()))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_manifest_has_one_dedicated_immutable_publication_path() {
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        let manifest = serde_json::json!({"result": "failed", "complete": false});

        let path = publish_manifest(directory.path(), &manifest)
            .expect("publish generic final run manifest");
        let published: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read published final run manifest"))
                .expect("decode published final run manifest");
        assert_eq!(published, manifest);

        let error = publish_manifest(directory.path(), &manifest)
            .expect_err("final run manifest must not be replaceable");
        assert!(error
            .to_string()
            .contains("refusing to replace immutable evidence"));
    }
}
