//! Local guest OS image management (`outbe-up image build|fetch|verify|list|remove`).

use std::{
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_vm::{
    image::{
        list_local_images, publish_local_image_verified_cancellable, remove_local_image,
        ManagedImageInfo,
    },
    local::default_vmm_home,
    vmm::{LocalUnixVmmTransport, VmmClient},
};
use outbe_operator_vm_image::{
    archive::{
        normalize_sha256, verify_archive_and_extract, verify_archive_and_extract_cancellable,
        verify_archive_file_and_extract_cancellable, verify_image_dir,
        verify_image_dir_against_archive_manifest, MAX_EXTRACTED_BYTES,
    },
    build as build_vm_image, parse_flavors, Flavor, VmImageOptions,
};
use reqwest::Url;
use rustix::fs::{openat, Mode, OFlags, CWD};
use serde::Serialize;
use tokio::signal::unix::{signal, SignalKind};

const MAX_REDIRECT_HOPS: usize = 5;
const DEFAULT_FETCH_TIMEOUT_SECS: u64 = 600;

pub struct BuildArgs {
    pub source: PathBuf,
    pub software_bundle: PathBuf,
    pub output: PathBuf,
    pub flavors: String,
    pub dstack_repo: String,
    pub runtime_libs_dir: Option<PathBuf>,
    pub toolchain_image: String,
    pub mkosi_image: String,
    pub cpuset_cpus: Option<String>,
    pub jobs: usize,
    pub check: bool,
    pub publish: bool,
    pub vmm_home: Option<PathBuf>,
    pub json: bool,
}

pub struct FetchArgs {
    pub url: String,
    pub sha256: String,
    pub os_image_hash: Option<String>,
    pub vmm_home: Option<PathBuf>,
    pub timeout_secs: u64,
    pub allow_insecure_loopback_http: bool,
    pub json: bool,
}

pub struct ImportArgs {
    pub archive: PathBuf,
    pub sha256: String,
    pub os_image_hash: Option<String>,
    pub vmm_home: Option<PathBuf>,
    pub json: bool,
}

pub struct VerifyArgs {
    pub dir: PathBuf,
    pub manifest: Option<PathBuf>,
    pub flavor: Option<Flavor>,
    pub json: bool,
}

pub struct ListArgs {
    pub vmm_home: Option<PathBuf>,
    pub json: bool,
}

pub struct RemoveArgs {
    pub name: String,
    pub vmm_home: Option<PathBuf>,
    pub vmm_socket: Option<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct FetchOutput {
    /// Effective URL sanitized to scheme + host + optional port + path (no userinfo, query, or fragment).
    url: String,
    host: String,
    image_name: String,
    flavor: Flavor,
    os_image_hash: String,
    archive_sha256: String,
    target_dir: PathBuf,
    reused: bool,
}

#[derive(Debug, Serialize)]
struct ImportOutput {
    archive: PathBuf,
    image_name: String,
    flavor: Flavor,
    os_image_hash: String,
    archive_sha256: String,
    target_dir: PathBuf,
    reused: bool,
}

#[derive(Debug, Serialize)]
struct VerifyOutput {
    dir: PathBuf,
    flavor: Option<Flavor>,
    os_image_hash: String,
    verified_files: usize,
    manifest: String,
}

#[derive(Debug, Serialize)]
struct RemoveOutput {
    removed: String,
    target_dir: PathBuf,
}

#[derive(Debug, Serialize)]
struct BuildPublishedEntry {
    flavor: String,
    image_name: String,
    os_image_hash: String,
    target_dir: PathBuf,
    reused: bool,
}

#[derive(Debug, Serialize)]
struct BuildOutput {
    output_dir: PathBuf,
    manifest: outbe_operator_vm_image::VmImageManifest,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    published: Vec<BuildPublishedEntry>,
}

pub fn build(args: BuildArgs) -> Result<()> {
    let flavors = parse_flavors(&args.flavors)?;
    let manifest = build_vm_image(&VmImageOptions {
        repo_root: &args.source,
        software_bundle: &args.software_bundle,
        output_dir: &args.output,
        flavors: &flavors,
        dstack_repo: &args.dstack_repo,
        runtime_libs_dir: args.runtime_libs_dir.as_deref(),
        toolchain_image: &args.toolchain_image,
        mkosi_image: &args.mkosi_image,
        cpuset_cpus: args.cpuset_cpus.as_deref(),
        jobs: args.jobs,
        check: args.check,
    })?;

    let mut published = Vec::new();
    if args.publish {
        let vmm_home = resolve_vmm_home(args.vmm_home)?;
        let image_root = vmm_home.join("image");
        fs::create_dir_all(&image_root)
            .wrap_err_with(|| format!("create VMM image root {}", image_root.display()))?;
        let never_cancel = AtomicBool::new(false);
        for (flavor_name, artifact) in &manifest.flavors {
            let archive_rel = artifact.archive.as_deref().ok_or_else(|| {
                eyre::eyre!("built flavor `{flavor_name}` is missing packed distribution archive")
            })?;
            let archive_sha = artifact.archive_sha256.as_deref().ok_or_else(|| {
                eyre::eyre!("built flavor `{flavor_name}` is missing archive SHA-256")
            })?;
            let archive_path = args.output.join(archive_rel);
            let work = tempfile::Builder::new()
                .prefix(".build-publish-")
                .tempdir_in(&image_root)
                .wrap_err_with(|| {
                    format!("create publish staging dir in {}", image_root.display())
                })?;
            let extract_dir = work.path().join("extracted");
            fs::create_dir(&extract_dir)?;
            let archive_manifest = verify_archive_and_extract(
                &archive_path,
                archive_sha,
                Some(&artifact.os_image_hash),
                &extract_dir,
            )?;
            let res = publish_local_image_verified_cancellable(
                &vmm_home,
                &extract_dir,
                None,
                flavor_name,
                &never_cancel,
                |existing_dir| {
                    verify_image_dir_against_archive_manifest(
                        existing_dir,
                        &archive_manifest,
                        &never_cancel,
                    )
                    .map(|_| ())
                },
            )?;
            published.push(BuildPublishedEntry {
                flavor: flavor_name.clone(),
                image_name: res.name,
                os_image_hash: res.digest,
                target_dir: res.target_dir,
                reused: res.reused,
            });
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&BuildOutput {
                output_dir: args.output,
                manifest,
                published,
            })?
        );
    } else {
        println!("Built VM image(s) in {}", args.output.display());
        for (flavor, artifact) in &manifest.flavors {
            println!("  {flavor}: os_image_hash={}", artifact.os_image_hash);
            if let Some(archive) = &artifact.archive {
                println!(
                    "    archive={archive} sha256={}",
                    artifact.archive_sha256.as_deref().unwrap_or("-")
                );
            }
        }
        for entry in &published {
            println!(
                "  published {} -> {} ({})",
                entry.image_name,
                entry.target_dir.display(),
                if entry.reused { "reused" } else { "new" }
            );
        }
    }
    Ok(())
}

pub async fn fetch(args: FetchArgs) -> Result<()> {
    let expected_sha256 = normalize_sha256(&args.sha256, "--sha256")?;
    let expected_os_hash = args
        .os_image_hash
        .as_deref()
        .map(|raw| normalize_sha256(raw, "--os-image-hash"))
        .transpose()?;
    let timeout = Duration::from_secs(args.timeout_secs.max(1));
    let vmm_home = resolve_vmm_home(args.vmm_home)?;
    let image_root = vmm_home.join("image");
    fs::create_dir_all(&image_root)
        .wrap_err_with(|| format!("create VMM image root {}", image_root.display()))?;

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_notify = Arc::new(tokio::sync::Notify::new());
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let cancel_signal = Arc::clone(&cancel);
    let notify_signal = Arc::clone(&cancel_notify);
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
                notify_signal.notify_waiters();
            }
            _ = sigterm.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
                notify_signal.notify_waiters();
            }
        }
    });

    let work = tempfile::Builder::new()
        .prefix(".fetch-")
        .tempdir_in(&image_root)
        .wrap_err_with(|| format!("create fetch staging dir in {}", image_root.display()))?;
    let archive_path = work.path().join("image.tar.zst");

    let fetch_result = async {
        let parsed = parse_and_validate_http_url(&args.url, args.allow_insecure_loopback_http)?;
        let (effective_url, effective_host, mut archive_file) = tokio::select! {
            res = fetch_http_to_file(
                &parsed,
                &archive_path,
                args.allow_insecure_loopback_http,
                timeout,
                &cancel,
            ) => res?,
            _ = cancel_notify.notified() => {
                bail!("image fetch canceled by signal");
            }
        };

        ensure!(
            !cancel.load(Ordering::Relaxed),
            "image fetch canceled by signal"
        );

        let extract_dir = work.path().join("extracted");
        fs::create_dir(&extract_dir)?;
        let expected_sha_clone = expected_sha256.clone();
        let expected_os_clone = expected_os_hash.clone();
        let archive_label = archive_path.display().to_string();
        let extract_clone = extract_dir.clone();
        let vmm_home_clone = vmm_home.clone();
        let cancel_worker = Arc::clone(&cancel);

        let (manifest, published) = tokio::task::spawn_blocking(move || -> Result<_> {
            // Hash and extract from the EXACT SAME open staging FD returned by fetch_http_to_file
            // (zero reopen by path between download, hash, and extraction).
            let manifest = verify_archive_file_and_extract_cancellable(
                &mut archive_file,
                &archive_label,
                &expected_sha_clone,
                expected_os_clone.as_deref(),
                &extract_clone,
                &cancel_worker,
            )?;
            let manifest_for_verify = manifest.clone();
            let cancel_for_verify = Arc::clone(&cancel_worker);
            let published = publish_local_image_verified_cancellable(
                &vmm_home_clone,
                &extract_clone,
                None,
                manifest.flavor.as_str(),
                &cancel_worker,
                move |existing_dir| {
                    verify_image_dir_against_archive_manifest(
                        existing_dir,
                        &manifest_for_verify,
                        &cancel_for_verify,
                    )
                    .map(|_| ())
                },
            )?;
            Ok((manifest, published))
        })
        .await
        .wrap_err("image verification worker panicked")??;

        Ok::<FetchOutput, eyre::Report>(FetchOutput {
            url: effective_url,
            host: effective_host,
            image_name: published.name,
            flavor: manifest.flavor,
            os_image_hash: published.digest,
            archive_sha256: expected_sha256,
            target_dir: published.target_dir,
            reused: published.reused,
        })
    }
    .await;

    signal_task.abort();
    let out = fetch_result?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Source host:    {}", out.host);
        println!("Source URL:     {}", out.url);
        println!("Image:          {}", out.image_name);
        println!("Flavor:         {}", out.flavor.as_str());
        println!("OS image hash:  {}", out.os_image_hash);
        println!("Archive SHA256: {}", out.archive_sha256);
        println!(
            "Location:       {} ({})",
            out.target_dir.display(),
            if out.reused { "reused" } else { "published" }
        );
    }
    Ok(())
}

pub async fn import(args: ImportArgs) -> Result<()> {
    let expected_sha256 = normalize_sha256(&args.sha256, "--sha256")?;
    let expected_os_hash = args
        .os_image_hash
        .as_deref()
        .map(|raw| normalize_sha256(raw, "--os-image-hash"))
        .transpose()?;

    let vmm_home = resolve_vmm_home(args.vmm_home)?;
    let image_root = vmm_home.join("image");
    fs::create_dir_all(&image_root)
        .wrap_err_with(|| format!("create VMM image root {}", image_root.display()))?;

    let cancel = Arc::new(AtomicBool::new(false));
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let cancel_signal = Arc::clone(&cancel);
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
            }
            _ = sigterm.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
            }
        }
    });

    let work = tempfile::Builder::new()
        .prefix(".import-")
        .tempdir_in(&image_root)
        .wrap_err_with(|| format!("create import staging dir in {}", image_root.display()))?;
    let extract_dir = work.path().join("extracted");
    fs::create_dir(&extract_dir)?;

    let archive_clone = args.archive.clone();
    let expected_sha_clone = expected_sha256.clone();
    let expected_os_clone = expected_os_hash.clone();
    let extract_clone = extract_dir.clone();
    let vmm_home_clone = vmm_home.clone();
    let cancel_worker = Arc::clone(&cancel);

    let import_res = tokio::task::spawn_blocking(move || -> Result<_> {
        let manifest = verify_archive_and_extract_cancellable(
            &archive_clone,
            &expected_sha_clone,
            expected_os_clone.as_deref(),
            &extract_clone,
            &cancel_worker,
        )?;
        let manifest_for_verify = manifest.clone();
        let cancel_for_verify = Arc::clone(&cancel_worker);
        let published = publish_local_image_verified_cancellable(
            &vmm_home_clone,
            &extract_clone,
            None,
            manifest.flavor.as_str(),
            &cancel_worker,
            move |existing_dir| {
                verify_image_dir_against_archive_manifest(
                    existing_dir,
                    &manifest_for_verify,
                    &cancel_for_verify,
                )
                .map(|_| ())
            },
        )?;
        Ok((manifest, published))
    })
    .await
    .wrap_err("image import worker panicked")?;

    signal_task.abort();
    let (manifest, published) = import_res?;

    let out = ImportOutput {
        archive: args.archive,
        image_name: published.name,
        flavor: manifest.flavor,
        os_image_hash: published.digest,
        archive_sha256: expected_sha256,
        target_dir: published.target_dir,
        reused: published.reused,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Archive:        {}", out.archive.display());
        println!("Image:          {}", out.image_name);
        println!("Flavor:         {}", out.flavor.as_str());
        println!("OS image hash:  {}", out.os_image_hash);
        println!("Archive SHA256: {}", out.archive_sha256);
        println!(
            "Location:       {} ({})",
            out.target_dir.display(),
            if out.reused { "reused" } else { "published" }
        );
    }
    Ok(())
}

pub fn verify(args: VerifyArgs) -> Result<()> {
    let result = verify_image_dir(&args.dir, args.manifest.as_deref(), args.flavor)?;
    let out = VerifyOutput {
        dir: args.dir,
        flavor: result.flavor,
        os_image_hash: result.os_image_hash,
        verified_files: result.verified_files,
        manifest: result.manifest_source,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("Verified:      {}", out.dir.display());
        if let Some(flavor) = out.flavor {
            println!("Flavor:        {}", flavor.as_str());
        }
        println!("OS image hash: {}", out.os_image_hash);
        println!("Files checked: {}", out.verified_files);
        println!("Manifest:      {}", out.manifest);
    }
    Ok(())
}

pub fn list(args: ListArgs) -> Result<()> {
    let vmm_home = resolve_vmm_home(args.vmm_home)?;
    let images = list_local_images(&vmm_home)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&images)?);
    } else if images.is_empty() {
        println!(
            "No outbe-up managed guest images in {}",
            vmm_home.join("image").display()
        );
    } else {
        println!("{:<36} {:<6} {:<16} PATH", "NAME", "FLAVOR", "DIGEST");
        for img in &images {
            print_image_row(img);
        }
    }
    Ok(())
}

fn print_image_row(img: &ManagedImageInfo) {
    println!(
        "{:<36} {:<6} {:<16} {}",
        img.name,
        img.flavor,
        &img.os_image_hash[..16.min(img.os_image_hash.len())],
        img.path.display()
    );
}

pub fn remove(args: RemoveArgs) -> Result<()> {
    let vmm_home = resolve_vmm_home(args.vmm_home)?;
    let vmm_socket = match args.vmm_socket {
        Some(path) => path,
        None => vmm_home.join("run/vmm.sock"),
    };
    let transport = LocalUnixVmmTransport::new(&vmm_socket, &vmm_home);
    let client = VmmClient::new(transport, vmm_socket.display().to_string());
    let removed_dir = remove_local_image(&vmm_home, &client, &args.name)?;

    let out = RemoveOutput {
        removed: args.name,
        target_dir: removed_dir,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "Removed image {} ({})",
            out.removed,
            out.target_dir.display()
        );
    }
    Ok(())
}

fn resolve_vmm_home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path),
        None => default_vmm_home(),
    }
}

/// Strip credentials (userinfo), query string, and fragment from a URL so presigned S3 credentials
/// are never written to stdout, stderr, JSON, or error messages.
pub fn sanitize_url(url: &Url) -> String {
    let mut sanitized = format!("{}://", url.scheme());
    if let Some(host) = url.host_str() {
        sanitized.push_str(host);
    }
    if let Some(port) = url.port() {
        sanitized.push_str(&format!(":{port}"));
    }
    sanitized.push_str(url.path());
    sanitized
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(host)) => {
            host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        }
        None => false,
    }
}

fn parse_and_validate_http_url(raw: &str, allow_loopback_http: bool) -> Result<Url> {
    let url = Url::parse(raw).map_err(|_| eyre::eyre!("invalid image URL"))?;
    match url.scheme() {
        "https" => {}
        "http" if allow_loopback_http && is_loopback_host(&url) => {}
        "http" => {
            return Err(eyre::eyre!(
                "refusing plaintext http:// URL ({}); use https://",
                sanitize_url(&url)
            ));
        }
        other => {
            return Err(eyre::eyre!(
                "unsupported image URL scheme '{other}' (expected https://)"
            ));
        }
    }
    ensure!(url.host_str().is_some(), "image URL must include a host");
    Ok(url)
}

async fn fetch_http_to_file(
    url: &Url,
    dest_path: &Path,
    allow_loopback_http: bool,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<(String, String, fs::File)> {
    let safe_initial = sanitize_url(url);
    let initial_scheme = url.scheme().to_owned();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECT_HOPS {
            return attempt.error("too many HTTP redirects");
        }
        let target_scheme = attempt.url().scheme().to_owned();
        if target_scheme != initial_scheme {
            return attempt.error(format!(
                "refusing cross-scheme redirect from {initial_scheme} to {target_scheme}"
            ));
        }
        if initial_scheme == "http" && (!allow_loopback_http || !is_loopback_host(attempt.url())) {
            return attempt.error("refusing non-loopback http redirect");
        }
        attempt.follow()
    });

    let client = reqwest::Client::builder()
        .referer(false)
        .redirect(policy)
        .connect_timeout(Duration::from_secs(30).min(timeout))
        .timeout(timeout)
        .build()
        .wrap_err("build HTTPS client")?;

    let mut response =
        client.get(url.clone()).send().await.map_err(|err| {
            eyre::eyre!("download failed for {safe_initial}: {}", err.without_url())
        })?;

    let effective_url = sanitize_url(response.url());
    let effective_host = response.url().host_str().unwrap_or_default().to_owned();

    let status = response.status();
    ensure!(
        status.is_success(),
        "HTTP {} downloading {effective_url}",
        status.as_u16()
    );

    let expected_len = response.content_length();
    if let Some(len) = expected_len {
        ensure!(
            len <= MAX_EXTRACTED_BYTES,
            "remote image archive ({len} bytes) exceeds maximum {MAX_EXTRACTED_BYTES} bytes"
        );
    }

    let fd = openat(
        CWD,
        dest_path,
        OFlags::CREATE | OFlags::EXCL | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .wrap_err_with(|| format!("create temporary download file {}", dest_path.display()))?;
    let mut file = fs::File::from(fd);
    let mut written: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(|err| {
        eyre::eyre!(
            "download interrupted for {effective_url}: {}",
            err.without_url()
        )
    })? {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "image download canceled by signal"
        );
        written = written
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| eyre::eyre!("download size overflow"))?;
        ensure!(
            written <= MAX_EXTRACTED_BYTES,
            "downloaded archive exceeded maximum {MAX_EXTRACTED_BYTES} bytes"
        );
        file.write_all(&chunk)?;
    }
    file.sync_all()?;

    if let Some(len) = expected_len {
        ensure!(
            written == len,
            "truncated download for {effective_url}: received {written} of {len} bytes"
        );
    }
    Ok((effective_url, effective_host, file))
}

pub const fn default_fetch_timeout_secs() -> u64 {
    DEFAULT_FETCH_TIMEOUT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_strips_userinfo_query_and_fragment() {
        let raw = "https://user:secretpass@s3.amazonaws.com:443/bucket/outbe-vm-image.tar.zst?X-Amz-Signature=deadbeef&X-Amz-Credential=AKIA123#frag";
        let url = Url::parse(raw).unwrap();
        let sanitized = sanitize_url(&url);
        assert_eq!(
            sanitized,
            "https://s3.amazonaws.com/bucket/outbe-vm-image.tar.zst"
        );
        assert!(!sanitized.contains("secretpass"));
        assert!(!sanitized.contains("deadbeef"));
        assert!(!sanitized.contains("AKIA123"));
        assert!(!sanitized.contains("frag"));
    }

    #[test]
    fn http_url_validation_rejects_plaintext_non_loopback_s3_and_unsupported_schemes() {
        let err = parse_and_validate_http_url(
            "http://example.com/image.tar.zst?X-Amz-Signature=SECRET",
            false,
        )
        .expect_err("non-loopback http must be rejected");
        let rendered = err.to_string();
        assert!(
            rendered.contains("refusing plaintext http://"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("SECRET"),
            "error must never leak query string: {rendered}"
        );

        let s3_err = parse_and_validate_http_url("s3://my-bucket/image.tar.zst", true)
            .expect_err("s3:// must be rejected");
        assert!(
            s3_err
                .to_string()
                .contains("unsupported image URL scheme 's3'"),
            "{s3_err}"
        );

        assert!(parse_and_validate_http_url("http://127.0.0.1:8080/a.tar.zst", false).is_err());
        assert!(parse_and_validate_http_url("http://127.0.0.1:8080/a.tar.zst", true).is_ok());
        assert!(parse_and_validate_http_url("file:///etc/passwd", true).is_err());
    }
}
