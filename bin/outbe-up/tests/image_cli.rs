use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use outbe_operator_vm_image::{
    archive::{
        pack_flavor_archive, sha256_file_streaming, ArchiveBuildMetadata, ARCHIVE_MANIFEST_NAME,
    },
    Flavor,
};
use rustix::{
    fs::{flock, FlockOperation},
    process::{kill_process, Pid, Signal},
};
use serde_json::Value;

const OUTBE_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-up");

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_sample_image_tree(dir: &Path, digest: &str) {
    write_mode(
        &dir.join("digest.txt"),
        format!("{digest}\n").as_bytes(),
        0o644,
    );
    write_mode(&dir.join("ovmf.fd"), b"fake-ovmf-firmware", 0o644);
    write_mode(&dir.join("bzImage"), b"fake-linux-kernel", 0o644);
    write_mode(&dir.join("initramfs.cpio.gz"), b"fake-initramfs", 0o644);
    write_mode(&dir.join("rootfs.squashfs"), b"fake-squashfs-rootfs", 0o644);
    write_mode(
        &dir.join("metadata.json"),
        br#"{"bios":"ovmf.fd","kernel":"bzImage","initrd":"initramfs.cpio.gz","rootfs":"rootfs.squashfs"}"#,
        0o644,
    );
    write_mode(&dir.join("disk.raw"), b"build-only-disk-raw", 0o644);
    let rootfs_sha = sha256_file_streaming(&dir.join("rootfs.squashfs")).unwrap();
    write_mode(
        &dir.join("sha256sum.txt"),
        format!("{rootfs_sha}  rootfs.squashfs\n").as_bytes(),
        0o644,
    );
}

fn build_sample_archive(tmp: &Path, flavor: Flavor, digest: &str) -> (PathBuf, String) {
    let src = tmp.join(format!("src-{}-{}", flavor.as_str(), &digest[..8]));
    build_sample_image_tree(&src, digest);
    let archive_path = tmp.join(format!(
        "outbe-vm-image-v1-{}-{}.tar.zst",
        flavor.as_str(),
        &digest[..16]
    ));
    let (_manifest, sha256) = pack_flavor_archive(
        &src,
        &ArchiveBuildMetadata {
            flavor,
            source_commit: &"a".repeat(40),
            dstack_rev: &"b".repeat(40),
            overlay_commit: &"c".repeat(40),
        },
        &archive_path,
    )
    .unwrap();
    (archive_path, sha256)
}

type HttpRoute = (String, u16, Vec<(String, String)>, Vec<u8>);

/// Minimal single-threaded loopback HTTP/1.1 test server serving static routes and redirects,
/// recording every raw incoming HTTP request for wire-level header assertions.
struct TestHttpServer {
    addr: String,
    captured: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestHttpServer {
    fn start(routes: Vec<HttpRoute>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_bg = Arc::clone(&captured);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let mut buf = [0_u8; 8192];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                        captured_bg.lock().unwrap().push(req.clone());
                        let target = req
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let path_only = target.split('?').next().unwrap_or(target);

                        if let Some((_, status, headers, body)) =
                            routes.iter().find(|(p, _, _, _)| p == path_only)
                        {
                            let reason = match *status {
                                200 => "OK",
                                302 => "Found",
                                _ => "Error",
                            };
                            let mut resp = format!(
                                "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                                body.len()
                            );
                            for (k, v) in headers {
                                resp.push_str(&format!("{k}: {v}\r\n"));
                            }
                            resp.push_str("\r\n");
                            let _ = stream.write_all(resp.as_bytes());
                            let _ = stream.write_all(body);
                            let _ = stream.flush();
                        } else {
                            let _ = stream.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            captured,
            stop,
            handle: Some(handle),
        }
    }

    fn captured_requests(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl Drop for TestHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Minimal Unix-socket fake VMM server responding to `POST /prpc/Status?json`.
struct FakeVmmSocket {
    socket_path: PathBuf,
    status_body: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeVmmSocket {
    fn start(socket_path: PathBuf, initial_status_json: String) -> Self {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let listener = UnixListener::bind(&socket_path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let status_body = Arc::new(Mutex::new(initial_status_json));
        let stop = Arc::new(AtomicBool::new(false));
        let body_bg = Arc::clone(&status_body);
        let stop_bg = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        // Drain full HTTP headers + Content-Length body before responding so the
                        // client never hits EPIPE on its body write.
                        let mut req = Vec::new();
                        let mut chunk = [0_u8; 4096];
                        let mut header_end = None;
                        let mut content_len = 0usize;
                        while let Ok(n) = stream.read(&mut chunk) {
                            if n == 0 {
                                break;
                            }
                            req.extend_from_slice(&chunk[..n]);
                            if header_end.is_none() {
                                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let end = pos + 4;
                                    header_end = Some(end);
                                    let head = String::from_utf8_lossy(&req[..end]);
                                    for line in head.lines() {
                                        if let Some((k, v)) = line.split_once(':') {
                                            if k.trim().eq_ignore_ascii_case("content-length") {
                                                content_len = v.trim().parse().unwrap_or(0);
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(end) = header_end {
                                if req.len() >= end + content_len {
                                    break;
                                }
                            }
                        }
                        let body = body_bg.lock().unwrap().clone();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            socket_path,
            status_body,
            stop,
            handle: Some(handle),
        }
    }

    fn set_status_json(&self, json: String) {
        *self.status_body.lock().unwrap() = json;
    }
}

impl Drop for FakeVmmSocket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[test]
fn image_fetch_http_sanitizes_presigned_urls_verifies_before_parse_blocks_cross_scheme_and_verifies_offline(
) {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "0123456789abcdef".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    let server = TestHttpServer::start(vec![
        (
            "/presigned/image.tar.zst".to_owned(),
            200,
            vec![],
            archive_bytes.clone(),
        ),
        (
            "/redirect-same-scheme".to_owned(),
            302,
            vec![(
                "Location".to_owned(),
                "/presigned/image.tar.zst?X-Amz-Signature=REDIRECT_SECRET_999".to_owned(),
            )],
            Vec::new(),
        ),
        (
            "/redirect-cross-scheme".to_owned(),
            302,
            vec![(
                "Location".to_owned(),
                "https://127.0.0.1:443/evil.tar.zst?X-Amz-Signature=CROSS_SECRET".to_owned(),
            )],
            Vec::new(),
        ),
        (
            "/corrupt.tar.zst".to_owned(),
            200,
            vec![],
            b"definitely-not-a-valid-zstd-archive".to_vec(),
        ),
    ]);

    let vmm_home = tmp.path().join("vmm-home");

    // 1. Happy path over loopback HTTP with presigned query string + userinfo + redirect:
    //    - Succeeds, publishes to <vmm_home>/image/outbe-vm-dev-<hash16>
    //    - Prints effective host & sanitized URL, NEVER leaking query/credentials in stdout/stderr/JSON.
    let presigned_url = format!(
        "http://user:TOP_SECRET_PASS@{}/redirect-same-scheme?X-Amz-Signature=SUPER_SECRET_SIG&X-Amz-Credential=AKIA_SECRET_KEY",
        server.addr
    );
    let out = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &presigned_url,
            "--sha256",
            &archive_sha,
            "--os-image-hash",
            &digest,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .args(["--allow-insecure-loopback-http", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "image fetch failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for secret in [
        "TOP_SECRET_PASS",
        "SUPER_SECRET_SIG",
        "AKIA_SECRET_KEY",
        "REDIRECT_SECRET_999",
        "X-Amz-Signature",
    ] {
        assert!(
            !stdout.contains(secret),
            "secret leaked to stdout: {stdout}"
        );
        assert!(
            !stderr.contains(secret),
            "secret leaked to stderr: {stderr}"
        );
    }
    let json: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["host"], "127.0.0.1");
    assert_eq!(
        json["url"],
        format!("http://{}/presigned/image.tar.zst", server.addr)
    );
    assert_eq!(json["image_name"], "outbe-vm-dev-0123456789abcdef");
    assert_eq!(json["flavor"], "dev");
    assert_eq!(json["os_image_hash"], digest);
    assert_eq!(json["archive_sha256"], archive_sha);
    assert_eq!(json["reused"], false);

    let published_dir = vmm_home.join("image/outbe-vm-dev-0123456789abcdef");
    assert!(published_dir.join(ARCHIVE_MANIFEST_NAME).is_file());
    assert!(!published_dir.join("disk.raw").exists());

    // 2. Re-fetch same archive -> idempotent reuse (`reused == true`).
    let out_reuse = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!("http://{}/presigned/image.tar.zst", server.addr),
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .args(["--allow-insecure-loopback-http", "--json"])
        .output()
        .unwrap();
    assert!(out_reuse.status.success());
    let reuse_json: Value = serde_json::from_slice(&out_reuse.stdout).unwrap();
    assert_eq!(reuse_json["reused"], true);

    // 3. Offline `image verify` on the fetched directory passes, then rejects tampering.
    let verify_ok = Command::new(OUTBE_UP_BIN)
        .args(["image", "verify", "--dir"])
        .arg(&published_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        verify_ok.status.success(),
        "{}",
        String::from_utf8_lossy(&verify_ok.stderr)
    );
    let vjson: Value = serde_json::from_slice(&verify_ok.stdout).unwrap();
    assert_eq!(vjson["os_image_hash"], digest);
    assert_eq!(vjson["flavor"], "dev");

    // Copy to a scratch dir, tamper one file, verify fails.
    let tampered_dir = tmp.path().join("tampered-img");
    fs::create_dir_all(&tampered_dir).unwrap();
    for entry in fs::read_dir(&published_dir).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), tampered_dir.join(entry.file_name())).unwrap();
    }
    fs::write(tampered_dir.join("bzImage"), b"EVIL-linux-kernel").unwrap();
    let verify_bad = Command::new(OUTBE_UP_BIN)
        .args(["image", "verify", "--dir"])
        .arg(&tampered_dir)
        .output()
        .unwrap();
    assert!(!verify_bad.status.success());
    assert!(
        String::from_utf8_lossy(&verify_bad.stderr).contains("SHA-256 mismatch"),
        "{}",
        String::from_utf8_lossy(&verify_bad.stderr)
    );

    // 4. Verify-before-parse: wrong --sha256 on corrupt body fails with SHA-256 mismatch (NOT zstd).
    let fresh_vmm = tmp.path().join("vmm-fresh");
    let bad_sha = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!(
                "http://{}/corrupt.tar.zst?X-Amz-Signature=SHOULD_NOT_LEAK",
                server.addr
            ),
            "--sha256",
            &"f".repeat(64),
            "--vmm-home",
        ])
        .arg(&fresh_vmm)
        .arg("--allow-insecure-loopback-http")
        .output()
        .unwrap();
    assert!(!bad_sha.status.success());
    let bad_sha_err = String::from_utf8_lossy(&bad_sha.stderr);
    assert!(
        bad_sha_err.contains("archive SHA-256 mismatch"),
        "{bad_sha_err}"
    );
    assert!(!bad_sha_err.contains("SHOULD_NOT_LEAK"), "{bad_sha_err}");
    assert!(fs::read_dir(fresh_vmm.join("image"))
        .unwrap()
        .next()
        .is_none());

    // 5. Cross-scheme redirect (http -> https) is refused and never leaks redirect query string.
    let cross = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!(
                "http://{}/redirect-cross-scheme?X-Amz-Signature=INIT_SECRET",
                server.addr
            ),
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&fresh_vmm)
        .arg("--allow-insecure-loopback-http")
        .output()
        .unwrap();
    assert!(!cross.status.success());
    let cross_err = String::from_utf8_lossy(&cross.stderr);
    assert!(
        cross_err.contains("refusing cross-scheme redirect")
            || cross_err.contains("download failed"),
        "{cross_err}"
    );
    assert!(!cross_err.contains("INIT_SECRET"), "{cross_err}");
    assert!(!cross_err.contains("CROSS_SECRET"), "{cross_err}");

    // 6. Plaintext non-loopback http:// is refused immediately without leaking query string.
    let plain_http = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            "http://example.com/image.tar.zst?X-Amz-Signature=PLAIN_SECRET",
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&fresh_vmm)
        .output()
        .unwrap();
    assert!(!plain_http.status.success());
    let plain_err = String::from_utf8_lossy(&plain_http.stderr);
    assert!(
        plain_err.contains("refusing plaintext http://"),
        "{plain_err}"
    );
    assert!(!plain_err.contains("PLAIN_SECRET"), "{plain_err}");
    // 7. s3:// scheme is unsupported (HTTPS only).
    let s3_rejected = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            "s3://outbe-releases/images/prod.tar.zst",
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&fresh_vmm)
        .output()
        .unwrap();
    assert!(!s3_rejected.status.success());
    let s3_err = String::from_utf8_lossy(&s3_rejected.stderr);
    assert!(
        s3_err.contains("unsupported image URL scheme 's3'"),
        "{s3_err}"
    );
}

#[test]
fn f2_cross_origin_redirect_never_leaks_referer_query_or_credentials() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "1122334455667788".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Prod, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    // Server B is a distinct origin (different loopback port) capturing all incoming HTTP headers.
    let server_b = TestHttpServer::start(vec![(
        "/cdn/final-image.tar.zst".to_owned(),
        200,
        vec![],
        archive_bytes,
    )]);
    let server_a = TestHttpServer::start(vec![(
        "/presigned-start".to_owned(),
        302,
        vec![(
            "Location".to_owned(),
            format!("http://{}/cdn/final-image.tar.zst", server_b.addr),
        )],
        Vec::new(),
    )]);

    let vmm_home = tmp.path().join("vmm-f2");
    let start_url = format!(
        "http://user:TOP_SECRET_CRED@{}/presigned-start?X-Amz-Signature=PRESIGNED_LEAK_TOKEN&X-Amz-Credential=AKIA_LEAK_KEY",
        server_a.addr
    );
    let out = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &start_url,
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .args(["--allow-insecure-loopback-http", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fetch failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let hop_b_requests = server_b.captured_requests();
    assert_eq!(
        hop_b_requests.len(),
        1,
        "expected exactly one request to redirect target"
    );
    let raw_req = &hop_b_requests[0];
    let lower = raw_req.to_ascii_lowercase();
    assert!(
        !lower.contains("\r\nreferer:"),
        "next-hop request leaked Referer header:\n{raw_req}"
    );
    assert!(
        !lower.contains("\r\nauthorization:"),
        "next-hop request leaked Authorization header:\n{raw_req}"
    );
    for secret in [
        "TOP_SECRET_CRED",
        "PRESIGNED_LEAK_TOKEN",
        "AKIA_LEAK_KEY",
        "X-Amz-Signature",
    ] {
        assert!(
            !raw_req.contains(secret),
            "next-hop request leaked `{secret}`:\n{raw_req}"
        );
    }
}

#[test]
fn f3_fetch_refuses_to_reuse_corrupted_installed_image() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "2233445566778899".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    let server = TestHttpServer::start(vec![(
        "/img.tar.zst".to_owned(),
        200,
        vec![],
        archive_bytes,
    )]);
    let vmm_home = tmp.path().join("vmm-f3");
    let url = format!("http://{}/img.tar.zst", server.addr);

    let first = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &url,
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .args(["--allow-insecure-loopback-http", "--json"])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    // Corrupt bzImage in place (same byte length, leaving digest.txt and sha256sum.txt untouched
    // — proving every file in the authenticated archive manifest is re-hashed before reuse).
    let kernel_path = vmm_home.join("image/outbe-vm-dev-2233445566778899/bzImage");
    let mut bytes = fs::read(&kernel_path).unwrap();
    bytes[0] ^= 0xff;
    fs::write(&kernel_path, &bytes).unwrap();

    // Re-fetching the valid archive MUST refuse reuse and MUST NOT silently overwrite.
    let second = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &url,
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .args(["--allow-insecure-loopback-http", "--json"])
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "fetch must fail when installed image is corrupted:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("refusing to reuse corrupted or unverified installed image")
            && stderr.contains("installed image SHA-256 mismatch for bzImage"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(&kernel_path).unwrap(),
        bytes,
        "fetch must not silently replace a corrupted installed image"
    );
}

#[test]
fn f4_list_and_remove_act_only_on_proven_outbe_up_images() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "33445566778899aa".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    let server = TestHttpServer::start(vec![(
        "/img.tar.zst".to_owned(),
        200,
        vec![],
        archive_bytes,
    )]);
    let vmm_home = tmp.path().join("vmm-f4");
    let fetch = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!("http://{}/img.tar.zst", server.addr),
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .arg("--allow-insecure-loopback-http")
        .output()
        .unwrap();
    assert!(fetch.status.success());

    let genuine_name = "outbe-vm-dev-33445566778899aa";
    let image_root = vmm_home.join("image");

    // 1. Foreign vendor image (non-outbe name, no archive manifest).
    let vendor_dir = image_root.join("important-vendor-image");
    build_sample_image_tree(&vendor_dir, &digest);

    // 2. Non-digest-qualified `outbe-vm-*` name.
    let bad_name_dir = image_root.join("outbe-vm-not-digest-qualified");
    build_sample_image_tree(&bad_name_dir, &digest);

    // 3. Digest-qualified name without `image-archive-manifest.json`.
    let no_manifest_digest = "4".repeat(64);
    let no_manifest_dir = image_root.join("outbe-vm-dev-4444444444444444");
    build_sample_image_tree(&no_manifest_dir, &no_manifest_digest);

    // 4. Digest-qualified name with empty `{}` `image-archive-manifest.json`.
    let empty_manifest_digest = "5".repeat(64);
    let empty_manifest_dir = image_root.join("outbe-vm-dev-5555555555555555");
    build_sample_image_tree(&empty_manifest_dir, &empty_manifest_digest);
    write_mode(
        &empty_manifest_dir.join(ARCHIVE_MANIFEST_NAME),
        b"{}",
        0o644,
    );

    // 5. Digest-qualified name whose strict manifest has mismatched flavor (`prod` != `dev`).
    let wrong_flavor_digest = "6".repeat(64);
    let wrong_flavor_dir = image_root.join("outbe-vm-dev-6666666666666666");
    build_sample_image_tree(&wrong_flavor_dir, &wrong_flavor_digest);
    write_mode(
        &wrong_flavor_dir.join(ARCHIVE_MANIFEST_NAME),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1,
            "flavor": "prod",
            "os_image_hash": wrong_flavor_digest,
            "source_commit": "a".repeat(40),
            "dstack_rev": "b".repeat(40),
            "overlay_commit": "c".repeat(40),
            "files": [
                {"path": "digest.txt", "size": 65, "sha256": "0".repeat(64), "mode": 0o644},
                {"path": "metadata.json", "size": 2, "sha256": "1".repeat(64), "mode": 0o644}
            ]
        }))
        .unwrap()
        .as_slice(),
        0o644,
    );

    // `image list --json` MUST list ONLY the single proven outbe-up image.
    let list = Command::new(OUTBE_UP_BIN)
        .args(["image", "list", "--vmm-home"])
        .arg(&vmm_home)
        .arg("--json")
        .output()
        .unwrap();
    assert!(list.status.success());
    let listed: Value = serde_json::from_slice(&list.stdout).unwrap();
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1, "unexpected listed images: {listed}");
    assert_eq!(arr[0]["name"], genuine_name);

    // `image remove` MUST refuse every foreign/unproven directory and leave them untouched.
    let sock_path = vmm_home.join("run/vmm.sock");
    let _vmm = FakeVmmSocket::start(sock_path.clone(), r#"{"vms":[],"total":0}"#.to_owned());

    for (name, dir) in [
        ("important-vendor-image", &vendor_dir),
        ("outbe-vm-not-digest-qualified", &bad_name_dir),
        ("outbe-vm-dev-4444444444444444", &no_manifest_dir),
        ("outbe-vm-dev-5555555555555555", &empty_manifest_dir),
        ("outbe-vm-dev-6666666666666666", &wrong_flavor_dir),
    ] {
        let rm = Command::new(OUTBE_UP_BIN)
            .args(["image", "remove", "--name", name, "--vmm-home"])
            .arg(&vmm_home)
            .arg("--vmm-socket")
            .arg(&sock_path)
            .output()
            .unwrap();
        assert!(
            !rm.status.success(),
            "image remove must refuse unmanaged dir `{name}`"
        );
        assert!(dir.is_dir(), "unmanaged dir `{name}` must remain intact");
    }

    // Genuine managed image removes cleanly.
    let rm_ok = Command::new(OUTBE_UP_BIN)
        .args(["image", "remove", "--name", genuine_name, "--vmm-home"])
        .arg(&vmm_home)
        .arg("--vmm-socket")
        .arg(&sock_path)
        .output()
        .unwrap();
    assert!(
        rm_ok.status.success(),
        "{}",
        String::from_utf8_lossy(&rm_ok.stderr)
    );
    assert!(!image_root.join(genuine_name).exists());
}

#[test]
fn f7_sigint_after_download_before_publish_aborts_without_publishing() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "778899aabbccddee".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    let server = TestHttpServer::start(vec![(
        "/img.tar.zst".to_owned(),
        200,
        vec![],
        archive_bytes,
    )]);

    let vmm_home = tmp.path().join("vmm-f7");
    let image_root = vmm_home.join("image");
    fs::create_dir_all(&image_root).unwrap();
    let image_name = "outbe-vm-dev-778899aabbccddee";

    // Hold the exclusive per-image lock so `outbe-up image fetch` finishes download, archive
    // SHA-256 verification, and extraction, then pauses right before publishing.
    let lock_path = image_root.join(format!(".{image_name}.lock"));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    flock(&lock_file, FlockOperation::LockExclusive).unwrap();

    let mut child = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!("http://{}/img.tar.zst", server.addr),
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .arg("--allow-insecure-loopback-http")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Wait until download + verification + extraction have completed and `.fetch-*/extracted`
    // contains `image-archive-manifest.json`.
    let wait_deadline = Instant::now() + Duration::from_secs(10);
    let mut extracted_seen = false;
    while Instant::now() < wait_deadline {
        if let Ok(entries) = fs::read_dir(&image_root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".fetch-")
                    && entry
                        .path()
                        .join("extracted")
                        .join(ARCHIVE_MANIFEST_NAME)
                        .is_file()
                {
                    extracted_seen = true;
                    break;
                }
            }
        }
        if extracted_seen {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        extracted_seen,
        "fetch did not reach post-extraction publication lock wait"
    );

    // Send SIGINT while STILL holding the lock -> child must abort promptly without publishing.
    let child_pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    kill_process(child_pid, Signal::INT).unwrap();

    let exit_deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "fetch hung after SIGINT while waiting on publication lock"
        );
        thread::sleep(Duration::from_millis(25));
    };

    drop(lock_file);

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(!status.success(), "canceled fetch must exit non-zero");
    assert!(
        stderr.contains("canceled"),
        "expected cancel error: {stderr}"
    );
    assert!(
        !image_root.join(image_name).exists(),
        "canceled fetch must not publish target image"
    );

    // Verify all `.fetch-*` and `.tmp.*` staging directories were cleaned up.
    for entry in fs::read_dir(&image_root).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".fetch-") && !name.contains(".tmp."),
            "leftover staging entry after cancel: {name}"
        );
    }
}

#[test]
fn image_list_and_remove_refuse_referenced_or_unreachable_vmm_and_delete_unreferenced() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "aaaaaaaaaaaaaaaa".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let archive_bytes = fs::read(&archive_path).unwrap();

    let server = TestHttpServer::start(vec![(
        "/img.tar.zst".to_owned(),
        200,
        vec![],
        archive_bytes,
    )]);
    let vmm_home = tmp.path().join("vmm");
    let fetch_out = Command::new(OUTBE_UP_BIN)
        .args([
            "image",
            "fetch",
            "--url",
            &format!("http://{}/img.tar.zst", server.addr),
            "--sha256",
            &archive_sha,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .arg("--allow-insecure-loopback-http")
        .output()
        .unwrap();
    assert!(fetch_out.status.success());

    let image_name = "outbe-vm-dev-aaaaaaaaaaaaaaaa";
    let image_dir = vmm_home.join("image").join(image_name);
    assert!(image_dir.is_dir());

    // `image list --json` returns the published image.
    let list_out = Command::new(OUTBE_UP_BIN)
        .args(["image", "list", "--vmm-home"])
        .arg(&vmm_home)
        .arg("--json")
        .output()
        .unwrap();
    assert!(list_out.status.success());
    let list_json: Value = serde_json::from_slice(&list_out.stdout).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 1);
    assert_eq!(list_json[0]["name"], image_name);
    assert_eq!(list_json[0]["flavor"], "dev");
    assert_eq!(list_json[0]["os_image_hash"], digest);

    // 1. Unreachable VMM socket -> fail closed, keep image directory.
    let missing_sock = vmm_home.join("run/nonexistent.sock");
    let rm_down = Command::new(OUTBE_UP_BIN)
        .args(["image", "remove", "--name", image_name, "--vmm-home"])
        .arg(&vmm_home)
        .arg("--vmm-socket")
        .arg(&missing_sock)
        .output()
        .unwrap();
    assert!(!rm_down.status.success());
    assert!(
        String::from_utf8_lossy(&rm_down.stderr).contains("failed to query VMM"),
        "{}",
        String::from_utf8_lossy(&rm_down.stderr)
    );
    assert!(image_dir.is_dir());

    // 2. Running or stopped VM references the image -> refuse, keep image directory.
    let sock_path = vmm_home.join("run/vmm.sock");
    let busy_status = serde_json::json!({
        "vms": [{
            "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "name": "val-0",
            "status": "stopped",
            "running": false,
            "configuration": { "image": image_name }
        }],
        "total": 1
    })
    .to_string();
    let vmm = FakeVmmSocket::start(sock_path.clone(), busy_status);

    let rm_busy = Command::new(OUTBE_UP_BIN)
        .args(["image", "remove", "--name", image_name, "--vmm-home"])
        .arg(&vmm_home)
        .arg("--vmm-socket")
        .arg(&sock_path)
        .output()
        .unwrap();
    assert!(!rm_busy.status.success());
    assert!(
        String::from_utf8_lossy(&rm_busy.stderr).contains("still referenced by VM(s): val-0"),
        "{}",
        String::from_utf8_lossy(&rm_busy.stderr)
    );
    assert!(image_dir.is_dir());

    // 3. Zero VMs reference the image -> remove succeeds and list is now empty.
    vmm.set_status_json(r#"{"vms":[],"total":0}"#.to_owned());
    let rm_ok = Command::new(OUTBE_UP_BIN)
        .args(["image", "remove", "--name", image_name, "--vmm-home"])
        .arg(&vmm_home)
        .arg("--vmm-socket")
        .arg(&sock_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        rm_ok.status.success(),
        "image remove failed:\n{}",
        String::from_utf8_lossy(&rm_ok.stderr)
    );
    let rm_json: Value = serde_json::from_slice(&rm_ok.stdout).unwrap();
    assert_eq!(rm_json["removed"], image_name);
    assert!(!image_dir.exists());

    let list_after = Command::new(OUTBE_UP_BIN)
        .args(["image", "list", "--vmm-home"])
        .arg(&vmm_home)
        .arg("--json")
        .output()
        .unwrap();
    assert!(list_after.status.success());
    let after_json: Value = serde_json::from_slice(&list_after.stdout).unwrap();
    assert!(after_json.as_array().unwrap().is_empty());
}

#[test]
fn version_json_reports_name_version_and_actual_binary_sha256() {
    let expected_sha = sha256_file_streaming(Path::new(OUTBE_UP_BIN)).unwrap();

    let out = Command::new(OUTBE_UP_BIN)
        .args(["version", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "version --json failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["name"], "outbe-up");
    assert!(!json["version"].as_str().unwrap_or("").is_empty());
    assert_eq!(json["binary_sha256"], expected_sha);

    let human = Command::new(OUTBE_UP_BIN).arg("version").output().unwrap();
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("outbe-up "), "{text}");
    assert!(text.contains(&expected_sha), "{text}");
}

#[test]
fn image_import_verifies_archive_sha256_before_extract_publishes_reuses_and_refuses_tampering() {
    let tmp = tempfile::tempdir().unwrap();
    let digest = "fedcba9876543210".repeat(4);
    let (archive_path, archive_sha) = build_sample_archive(tmp.path(), Flavor::Dev, &digest);
    let vmm_home = tmp.path().join("vmm-home");

    // 1. Wrong archive SHA-256 -> refused BEFORE extraction, nothing published.
    let bad_sha = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&archive_path)
        .args(["--sha256", &"0".repeat(64), "--vmm-home"])
        .arg(&vmm_home)
        .output()
        .unwrap();
    assert!(!bad_sha.status.success());
    assert!(!vmm_home
        .join("image/outbe-vm-dev-fedcba9876543210")
        .exists());

    // 2. Wrong `--os-image-hash` -> refused, nothing published.
    let bad_os = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&archive_path)
        .args([
            "--sha256",
            &archive_sha,
            "--os-image-hash",
            &"a".repeat(64),
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .output()
        .unwrap();
    assert!(!bad_os.status.success());
    assert!(!vmm_home
        .join("image/outbe-vm-dev-fedcba9876543210")
        .exists());

    // 3. Valid import publishes cleanly with expected JSON schema.
    let ok = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&archive_path)
        .args([
            "--sha256",
            &archive_sha,
            "--os-image-hash",
            &digest,
            "--vmm-home",
        ])
        .arg(&vmm_home)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "image import failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&ok.stdout),
        String::from_utf8_lossy(&ok.stderr)
    );
    let j: Value = serde_json::from_slice(&ok.stdout).unwrap();
    assert_eq!(j["image_name"], "outbe-vm-dev-fedcba9876543210");
    assert_eq!(j["flavor"], "dev");
    assert_eq!(j["os_image_hash"], digest);
    assert_eq!(j["archive_sha256"], archive_sha);
    assert_eq!(j["reused"], false);
    let target = vmm_home.join("image/outbe-vm-dev-fedcba9876543210");
    assert!(target.join(ARCHIVE_MANIFEST_NAME).is_file());

    // 4. Re-import is idempotent (`reused == true`).
    let reuse = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&archive_path)
        .args(["--sha256", &archive_sha, "--vmm-home"])
        .arg(&vmm_home)
        .arg("--json")
        .output()
        .unwrap();
    assert!(reuse.status.success());
    let rj: Value = serde_json::from_slice(&reuse.stdout).unwrap();
    assert_eq!(rj["reused"], true);

    // 5. Tampering with already-installed file causes re-import to refuse reuse (fail closed).
    fs::write(target.join("rootfs.squashfs"), b"tampered-rootfs").unwrap();
    let tampered = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&archive_path)
        .args(["--sha256", &archive_sha, "--vmm-home"])
        .arg(&vmm_home)
        .output()
        .unwrap();
    assert!(
        !tampered.status.success(),
        "import must refuse to reuse a corrupted installed image"
    );

    // 6. Symlink and FIFO at `--archive` are both refused promptly via single-FD O_NOFOLLOW | O_NONBLOCK.
    let symlink_archive = tmp.path().join("link.tar.zst");
    std::os::unix::fs::symlink(&archive_path, &symlink_archive).unwrap();
    let sym_out = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&symlink_archive)
        .args(["--sha256", &archive_sha, "--vmm-home"])
        .arg(&vmm_home)
        .output()
        .unwrap();
    assert!(!sym_out.status.success(), "symlink archive must be refused");
    assert!(
        String::from_utf8_lossy(&sym_out.stderr).contains("symlink"),
        "{}",
        String::from_utf8_lossy(&sym_out.stderr)
    );

    let fifo_archive = tmp.path().join("fifo.tar.zst");
    assert!(Command::new("mkfifo")
        .arg(&fifo_archive)
        .status()
        .unwrap()
        .success());
    let start = Instant::now();
    let fifo_out = Command::new(OUTBE_UP_BIN)
        .args(["image", "import", "--archive"])
        .arg(&fifo_archive)
        .args(["--sha256", &archive_sha, "--vmm-home"])
        .arg(&vmm_home)
        .output()
        .unwrap();
    assert!(!fifo_out.status.success(), "FIFO archive must be refused");
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "FIFO archive must fail promptly without blocking"
    );
    assert!(
        String::from_utf8_lossy(&fifo_out.stderr).contains("non-regular"),
        "{}",
        String::from_utf8_lossy(&fifo_out.stderr)
    );
}
