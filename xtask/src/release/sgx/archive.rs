use super::require_nonempty_regular_file;
use super::tree_entries;
use super::Sha256Digest;
use super::TreeEntry;

use eyre::bail;

use eyre::Result;
use eyre::WrapErr;

use sha2::Digest as _;
use sha2::Sha256;

use std::collections::BTreeSet;
use std::fs;
use std::fs::File;

use std::io::Read;

use std::os::unix::fs::PermissionsExt;
use std::path::Component;
use std::path::Path;

use walkdir::WalkDir;

pub(super) fn validate_archive_member_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_string_lossy().contains('\\')
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!(
            "Processor DCAP archive contains unsafe path: {}",
            path.display()
        );
    }
    Ok(())
}

pub fn write_deterministic_bundle_archive(
    bundle: &Path,
    output: &Path,
    source_date_epoch: i64,
) -> Result<()> {
    if source_date_epoch < 0 {
        bail!("SOURCE_DATE_EPOCH must be non-negative");
    }
    if output.exists() {
        bail!("signed SGX archive already exists: {}", output.display());
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("create archive directory: {}", parent.display()))?;
    }
    let file = File::create(output)
        .wrap_err_with(|| format!("create signed SGX archive: {}", output.display()))?;
    let mut archive = tar::Builder::new(file);
    archive.follow_symlinks(false);
    for item in WalkDir::new(bundle).min_depth(1).sort_by_file_name() {
        let item =
            item.wrap_err_with(|| format!("walk signed SGX bundle: {}", bundle.display()))?;
        let path = item.path();
        let relative = path
            .strip_prefix(bundle)
            .wrap_err("derive archive relative path")?;
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("read archive input: {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("signed SGX bundle contains symlink: {}", relative.display());
        }
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(source_date_epoch as u64);
        header.set_mode(metadata.permissions().mode() & 0o7777);
        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive
                .append_data(&mut header, relative, std::io::empty())
                .wrap_err_with(|| format!("archive directory: {}", relative.display()))?;
        } else if metadata.is_file() {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.len());
            header.set_cksum();
            let mut input = File::open(path)
                .wrap_err_with(|| format!("open archive input: {}", path.display()))?;
            archive
                .append_data(&mut header, relative, &mut input)
                .wrap_err_with(|| format!("archive file: {}", relative.display()))?;
        } else {
            bail!(
                "signed SGX bundle contains unsupported entry: {}",
                relative.display()
            );
        }
    }
    archive.finish().wrap_err("finish signed SGX archive")?;
    Ok(())
}

pub(super) fn verify_bundle_archive(
    bundle: &Path,
    archive_path: &Path,
    source_date_epoch: i64,
) -> Result<()> {
    require_nonempty_regular_file(archive_path, "signed SGX bundle archive")?;
    let input = File::open(archive_path)
        .wrap_err_with(|| format!("open signed SGX archive: {}", archive_path.display()))?;
    let mut archive = tar::Archive::new(input);
    let mut observed = Vec::new();
    let mut paths = BTreeSet::new();
    for item in archive.entries().wrap_err("read signed SGX archive")? {
        let mut item = item.wrap_err("read signed SGX archive entry")?;
        let path = item.path().wrap_err("read signed SGX archive path")?;
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!(
                "signed SGX archive contains unsafe path: {}",
                path.display()
            );
        }
        let path = path.to_string_lossy().replace('\\', "/");
        if !paths.insert(path.clone()) {
            bail!("signed SGX archive contains duplicate path: {path}");
        }
        let header = item.header();
        if header.uid()? != 0 || header.gid()? != 0 || header.mtime()? != source_date_epoch as u64 {
            bail!("signed SGX archive has non-deterministic ownership/time: {path}");
        }
        let mode = format!("{:04o}", header.mode()? & 0o7777);
        if header.entry_type().is_dir() {
            observed.push(TreeEntry {
                digest: None,
                mode,
                path,
                size: None,
                kind: "directory".to_owned(),
            });
        } else if header.entry_type().is_file() {
            let size = header.size()?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut read = 0u64;
            loop {
                let count = item.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                read += count as u64;
            }
            if read != size {
                bail!("signed SGX archive entry size mismatch: {path}");
            }
            observed.push(TreeEntry {
                digest: Some(Sha256Digest {
                    algorithm: "sha256".to_owned(),
                    value: hex::encode(hasher.finalize()),
                }),
                mode,
                path,
                size: Some(size),
                kind: "file".to_owned(),
            });
        } else {
            bail!("signed SGX archive contains non-file entry: {path}");
        }
    }
    observed.sort_by(|left, right| left.path.cmp(&right.path));
    if observed != tree_entries(bundle)? {
        bail!("signed SGX archive does not exactly reproduce the verified bundle tree");
    }
    Ok(())
}
