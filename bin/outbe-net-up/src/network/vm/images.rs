//! Guest OS image source resolution, SHA-256 verification, and host import/fetch.

use std::{
    fs,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh};
use outbe_software_bundle::sha256_file;
use serde::Deserialize;

use super::{
    remote_bin::{cleanup_remote_dir, make_remote_temp_dir, RemoteOutbeUp},
    shell_quote_tilde_path,
    types::{RemoteImagePublishOutput, RemoteListedImage},
    ImageSource,
};

#[derive(Clone)]
pub(crate) enum ResolvedImageSource {
    Archive { path: PathBuf, sha256: String },
    Url { url: String, sha256: String },
}

pub(crate) struct HostImageTarget<'a> {
    pub(crate) ssh: &'a Ssh,
    pub(crate) bin: &'a RemoteOutbeUp,
    pub(crate) vmm_home: &'a str,
}

pub(crate) fn resolve_image_source(source: &ImageSource) -> Result<(ResolvedImageSource, String)> {
    match source {
        ImageSource::Archive { path, sha256 } => {
            ensure!(
                path.is_file(),
                "guest OS image archive does not exist: {}",
                path.display()
            );
            let actual = sha256_file(path)?;
            if let Some(expected) = sha256 {
                let norm = normalize_sha256(expected)?;
                ensure!(
                    actual == norm,
                    "local image archive SHA-256 mismatch: expected {norm}, got {actual}"
                );
            }
            let id = format!("archive:{actual}");
            Ok((
                ResolvedImageSource::Archive {
                    path: path.clone(),
                    sha256: actual,
                },
                id,
            ))
        }
        ImageSource::Url { url, sha256 } => {
            ensure!(
                url.starts_with("https://") && !url.chars().any(char::is_whitespace),
                "image URL must be an https:// URL without whitespace (got `{}`)",
                url.split('?').next().unwrap_or(url)
            );
            let norm = normalize_sha256(sha256)?;
            let id = format!("url:{norm}");
            Ok((
                ResolvedImageSource::Url {
                    url: url.clone(),
                    sha256: norm,
                },
                id,
            ))
        }
    }
}

fn normalize_sha256(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    ensure!(
        trimmed.len() == 64 && trimmed.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected 64-character hex SHA-256 digest, got `{raw}`"
    );
    Ok(trimmed)
}

/// Read `os_image_hash` from `image-archive-manifest.json` inside a local `.tar.zst` guest image
/// archive so `import_image_on_host` can skip `scp` when the host already has the image.
fn read_local_archive_os_image_hash(local_archive: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct LocalArchiveManifest {
        os_image_hash: String,
    }

    let file = fs::File::open(local_archive).ok()?;
    let decoder = zstd::Decoder::new(file).ok()?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().ok()?;
    for entry in entries.flatten() {
        let path = entry.path().ok()?;
        let normalized = path
            .to_str()?
            .trim_start_matches("./")
            .trim_start_matches('/');
        if normalized == "image-archive-manifest.json" {
            let manifest: LocalArchiveManifest = serde_json::from_reader(entry).ok()?;
            return normalize_sha256(&manifest.os_image_hash).ok();
        }
    }
    None
}

fn find_reusable_host_image(
    exec: &dyn HostExec,
    target: &HostImageTarget<'_>,
    expected_hash: &str,
) -> Option<RemoteImagePublishOutput> {
    let list_cmd = target.bin.cmd(&format!(
        "image list --vmm-home {} --json",
        shell_quote_tilde_path(target.vmm_home)
    ));
    let listed = run_host_json::<dyn HostExec, Vec<RemoteListedImage>>(
        exec,
        target.ssh,
        &target.ssh.host,
        &list_cmd,
    )
    .ok()?;
    let existing = listed
        .into_iter()
        .find(|img| img.has_archive_manifest && img.os_image_hash == expected_hash)?;
    Some(RemoteImagePublishOutput {
        image_name: existing.name,
        os_image_hash: existing.os_image_hash,
        target_dir: existing.path,
        reused: true,
    })
}

pub(crate) fn publish_image_on_host(
    exec: &dyn HostExec,
    target: &HostImageTarget<'_>,
    source: &ResolvedImageSource,
) -> Result<RemoteImagePublishOutput> {
    match source {
        ResolvedImageSource::Archive { path, sha256 } => {
            import_image_on_host(exec, target, path, sha256)
        }
        ResolvedImageSource::Url { url, sha256 } => fetch_image_on_host(exec, target, url, sha256),
    }
}

pub(crate) fn import_image_on_host(
    exec: &dyn HostExec,
    target: &HostImageTarget<'_>,
    local_archive: &Path,
    sha256: &str,
) -> Result<RemoteImagePublishOutput> {
    // F7: check `outbe-up image list --json` first and skip uploading the archive via `scp` when
    // the host already has a verified managed image with the same `os_image_hash`.
    if let Some(expected_hash) = read_local_archive_os_image_hash(local_archive) {
        if let Some(reused) = find_reusable_host_image(exec, target, &expected_hash) {
            return Ok(reused);
        }
    }

    let filename = local_archive
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| {
            eyre::eyre!(
                "image archive path has no UTF-8 filename: {}",
                local_archive.display()
            )
        })?;
    let remote_dir = make_remote_temp_dir(exec, target.ssh, "outbe-net-up-img")?;
    let remote_archive = format!("{remote_dir}/{filename}");
    let res = (|| {
        exec.copy_to(
            target.ssh,
            &target.ssh.host,
            &[local_archive],
            &remote_dir,
            false,
        )
        .wrap_err_with(|| format!("upload image archive to {}", target.ssh.host))?;
        let import_cmd = target.bin.cmd(&format!(
            "image import --archive {} --sha256 {} --vmm-home {} --json",
            shell_quote(&remote_archive),
            shell_quote(sha256),
            shell_quote_tilde_path(target.vmm_home),
        ));
        run_host_json(exec, target.ssh, &target.ssh.host, &import_cmd)
            .wrap_err_with(|| format!("import guest OS image on {}", target.ssh.host))
    })();
    let _ = cleanup_remote_dir(exec, target.ssh, &remote_dir);
    res
}

pub(crate) fn fetch_image_on_host(
    exec: &dyn HostExec,
    target: &HostImageTarget<'_>,
    url: &str,
    sha256: &str,
) -> Result<RemoteImagePublishOutput> {
    let fetch_cmd = target.bin.cmd(&format!(
        "image fetch --url {} --sha256 {} --vmm-home {} --json",
        shell_quote(url),
        shell_quote(sha256),
        shell_quote_tilde_path(target.vmm_home),
    ));
    run_host_json(exec, target.ssh, &target.ssh.host, &fetch_cmd)
        .wrap_err_with(|| format!("fetch guest OS image on {}", target.ssh.host))
}
