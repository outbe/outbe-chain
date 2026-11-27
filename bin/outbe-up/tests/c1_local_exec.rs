//! LocalExec end-to-end CLI integration tests for S6.3 Stop Point C1.
//!
//! Invokes the compiled `outbe-up` binary locally (the exact CLI surface `outbe-net-up` drives
//! over SSH/LocalExec) against a fake local VMM Unix-socket server across:
//! - `disk prepare --role founder --source - --bundle ... --staging-name <vm> --json` (plain + zstd stdin tar)
//! - stdin tar security rejections (`..` traversal, symlink, executable, missing `--bundle`/`--skip-key-verify`)
//! - `disk clean --list --json`, `--staging-name <vm> --json`, and `--stale --json`
//! - `vm create --owner-tag <tag> --disk-staging <vm> --consume-staging --json` (atomic consume only on Ready; preserve on failure)
//! - `vm status --json` exposing `owner_tag`, `disk_source_sha256`, `app_id`, `no_tee`, `vcpu`, `memory_gib`, `disk_gib`
//! - `--expect-vm-id <uuid>` match + mismatch refusal with ZERO VMM RPCs across `status|start|logs|stop|remove`

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use outbe_operator_node_role::bundle::{self, BundleInput};
use outbe_operator_vm::local::OWNER_TAG_ANNOTATION;
use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};
use serde_json::{json, Value};

const OUTBE_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-up");
const TEST_UUID: &str = "550e8400-e29b-41d4-a716-446655440777";
const WRONG_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TEST_MAC: &str = "52:54:00:ca:fe:01";

fn has_disk_and_vm_tools() -> bool {
    let disk_ok = ["sfdisk", "mke2fs", "debugfs"].into_iter().all(|tool| {
        Command::new("sh")
            .args([
                "-c",
                &format!("command -v {tool} >/dev/null 2>&1 || [ -x /usr/sbin/{tool} ] || [ -x /sbin/{tool} ]"),
            ])
            .status()
            .is_ok_and(|s| s.success())
    });
    let qemu_ok = Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    disk_ok && qemu_ok
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_founder_tree(tree: &Path, chain_id: u64) {
    fs::create_dir_all(tree).unwrap();
    fs::set_permissions(tree, fs::Permissions::from_mode(0o755)).unwrap();

    let hash = alloy_primitives::B256::repeat_byte(0x42);
    let policy = outbe_primitives::tee_genesis_v1::initial_tee_policy_v1(
        outbe_primitives::tee_genesis_v1::InitialTeeProfileV1::NoAttestationDev,
        chain_id,
        hash,
    )
    .unwrap();
    let genesis = json!({
        "config": {
            "chainId": chain_id,
            "teeAttestationV1": outbe_primitives::tee_genesis_v1::tee_attestation_v1_genesis_field(&policy).unwrap()
        }
    });
    write_mode(
        &tree.join("genesis.json"),
        &serde_json::to_vec(&genesis).unwrap(),
        0o644,
    );
    fs::create_dir_all(tree.join(".bb-crs")).unwrap();
    fs::set_permissions(tree.join(".bb-crs"), fs::Permissions::from_mode(0o755)).unwrap();
    write_mode(&tree.join(".bb-crs/bn254_g1.dat"), b"crs-bytes", 0o644);

    let peer_key = "a".repeat(96);
    write_mode(
        &tree.join("validator.env"),
        format!(
            concat!(
                "OUTBE_NODE_ROLE=\"founder\"\n",
                "OUTBE_EXTERNAL_IP=\"10.77.0.10\"\n",
                "OUTBE_RPC_BIND_IP=\"10.77.0.10\"\n",
                "OUTBE_VM_IP=\"10.77.0.10\"\n",
                "OUTBE_VM_PREFIX=\"24\"\n",
                "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
                "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
                "OUTBE_RPC_PORT=\"8545\"\n",
                "OUTBE_CONSENSUS_PORT=\"30400\"\n",
                "OUTBE_RETH_P2P_PORT=\"30303\"\n",
                "OUTBE_RETH_DISCOVERY_PORT=\"31303\"\n",
                "OUTBE_RADICLE_PORT=\"8776\"\n",
                "OUTBE_CERTIFIED_RPC=\"http://10.77.0.10:8545\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n",
                "OUTBE_BOOTNODES=\"enode://abcd@10.77.0.10:30303\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{peer_key}@10.77.0.10:30400\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
                "OUTBE_CHAIN_ID=\"{chain_id}\"\n",
            ),
            peer_key = peer_key,
            chain_id = chain_id
        )
        .as_bytes(),
        0o640,
    );
    write_mode(
        &tree.join("feeder-public.toml"),
        b"[oracle]\nsources = \"local\"\n",
        0o640,
    );
    write_mode(
        &tree.join("hyperlane-public.toml"),
        format!(
            concat!(
                "[hyperlane]\n",
                "poll_interval_secs = 30\n",
                "gasless = true\n\n",
                "[health]\n",
                "enabled = true\n",
                "bind_address = \"127.0.0.1:9003\"\n\n",
                "[storage]\n",
                "db_path = \"/opt/outbe-chain/state/hyperlane/db\"\n\n",
                "[server]\n",
                "bind_address = \"0.0.0.0:9000\"\n",
                "public_url = \"http://10.77.0.10:9000\"\n\n",
                "[[origin]]\n",
                "domain = {chain_id}\n",
                "rpc_urls = [\"http://127.0.0.1:8545\"]\n",
                "rpc_consensus = \"single\"\n",
                "reorg_period = 0\n",
            ),
            chain_id = chain_id
        )
        .as_bytes(),
        0o640,
    );

    fs::create_dir_all(tree.join("keys/radicle/keys")).unwrap();
    for dir in ["keys", "keys/radicle", "keys/radicle/keys"] {
        fs::set_permissions(tree.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (rel, byte) in [
        ("keys/signing-key.hex", "1"),
        ("keys/evm-key.hex", "2"),
        ("keys/feeder-key.hex", "3"),
        ("keys/hyperlane-key.hex", "4"),
        ("keys/reth-p2p-secret.hex", "5"),
    ] {
        write_mode(&tree.join(rel), byte.repeat(64).as_bytes(), 0o600);
    }
    write_mode(
        &tree.join("keys/radicle/keys/radicle"),
        b"fake-radicle-ssh-private-key",
        0o600,
    );
}

fn build_operator_bundle(dir: &Path, chain_id: u64) -> PathBuf {
    let src = dir.join("op-bundle-src");
    let genesis = src.join("genesis.json");
    write_mode(
        &genesis,
        format!(r#"{{"config":{{"chainId":{chain_id}}}}}"#).as_bytes(),
        0o644,
    );
    let crs = src.join("bn254_g1.dat");
    write_mode(&crs, b"crs-bytes", 0o644);
    let keygen = src.join("outbe-keygen");
    let script = r#"#!/bin/sh
set -eu
cmd="$1"
shift
if [ "$cmd" = "verify" ]; then
    key_path=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--key" ]; then
            key_path="$2"
            shift 2
        else
            shift 1
        fi
    done
    val="$(tr -d '\r\n' < "$key_path")"
    [ "${#val}" -eq 64 ] || exit 1
    exit 0
fi
exit 1
"#;
    write_mode(&keygen, script.as_bytes(), 0o755);
    let chain_bin = src.join("outbe-chain");
    write_mode(&chain_bin, b"#!/bin/sh\nexit 0\n", 0o755);

    let bundle_path = dir.join("outbe-validator-operator-bundle.tar.zst");
    bundle::create_archive(
        &bundle_path,
        vec![
            BundleInput {
                path: "genesis.json".into(),
                source: genesis,
                mode: 0o644,
            },
            BundleInput {
                path: ".bb-crs/bn254_g1.dat".into(),
                source: crs,
                mode: 0o644,
            },
            BundleInput {
                path: "outbe-keygen".into(),
                source: keygen,
                mode: 0o755,
            },
            BundleInput {
                path: "outbe-chain".into(),
                source: chain_bin,
                mode: 0o755,
            },
        ],
    )
    .unwrap();
    bundle_path
}

fn tar_dir_bytes(dir: &Path) -> Vec<u8> {
    let out = Command::new("tar")
        .args(["-cf", "-", "-C"])
        .arg(dir)
        .arg(".")
        .output()
        .expect("run tar");
    assert!(
        out.status.success(),
        "tar failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn zstd_compress_bytes(input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("zstd")
        .args(["-1", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn zstd");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "zstd compression failed");
    out.stdout
}

/// Minimal LocalExec harness mirroring how `outbe-net-up` invokes `outbe-up` with args + optional stdin.
struct LocalExec;

impl LocalExec {
    fn run(args: &[&str], stdin_bytes: Option<&[u8]>) -> Output {
        let mut cmd = Command::new(OUTBE_UP_BIN);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        if stdin_bytes.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        let mut child = cmd.spawn().expect("spawn outbe-up");
        if let Some(bytes) = stdin_bytes {
            let mut stdin = child.stdin.take().unwrap();
            let _ = stdin.write_all(bytes);
            drop(stdin);
        }
        child.wait_with_output().expect("wait outbe-up")
    }
}

struct FakeVm {
    name: String,
    image: String,
    vcpu: u32,
    memory: u32,
    app_id: String,
    status: String,
    running: bool,
    stopped_flag: bool,
    annotations: BTreeMap<String, String>,
    remove_polls_left: usize,
}

struct FakeVmmServer {
    sock: PathBuf,
    stop: Arc<AtomicBool>,
    rpc_count: Arc<AtomicUsize>,
    vms: Arc<Mutex<BTreeMap<String, FakeVm>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl FakeVmmServer {
    fn start(sock: PathBuf) -> Self {
        fs::create_dir_all(sock.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        let vms: Arc<Mutex<BTreeMap<String, FakeVm>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let rpc_count = Arc::new(AtomicUsize::new(0));
        let stop_bg = Arc::clone(&stop);
        let rpc_bg = Arc::clone(&rpc_count);
        let vms_bg = Arc::clone(&vms);
        let handle = thread::spawn(move || {
            while !stop_bg.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        rpc_bg.fetch_add(1, Ordering::SeqCst);
                        serve_one(&mut stream, &vms_bg);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            sock,
            stop,
            rpc_count,
            vms,
            handle: Some(handle),
        }
    }
}

impl Drop for FakeVmmServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_file(&self.sock);
    }
}

fn serve_one(
    stream: &mut std::os::unix::net::UnixStream,
    vms: &Arc<Mutex<BTreeMap<String, FakeVm>>>,
) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 2048];
    let mut header_end = None;
    let mut content_len = 0usize;
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if header_end.is_none() {
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                let headers = String::from_utf8_lossy(&raw[..pos]);
                for line in headers.lines() {
                    if let Some((k, v)) = line.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
            }
        }
        if let Some(end) = header_end {
            if raw.len() >= end + content_len {
                break;
            }
        }
    }
    let Some(end) = header_end else { return };
    let head = String::from_utf8_lossy(&raw[..end]);
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");
    let method = target
        .strip_prefix("/prpc/")
        .and_then(|rest| rest.split('?').next())
        .unwrap_or("");
    let req: Value = serde_json::from_slice(&raw[end..end + content_len]).unwrap_or(json!({}));

    let mut guard = vms.lock().unwrap();
    let body = match method {
        "Status" => {
            let keyword = req["keyword"].as_str().unwrap_or("");
            let brief = req["brief"].as_bool().unwrap_or(false);
            let vms_json: Vec<Value> = guard
                .iter()
                .filter(|(_, vm)| keyword.is_empty() || vm.name.contains(keyword))
                .map(|(id, vm)| {
                    let config = if brief {
                        Value::Null
                    } else {
                        json!({
                            "image": vm.image,
                            "vcpu": vm.vcpu,
                            "memory": vm.memory,
                            "app_id": vm.app_id,
                            "stopped": vm.stopped_flag,
                            "annotations": vm.annotations,
                        })
                    };
                    json!({
                        "id": id,
                        "name": vm.name,
                        "status": vm.status,
                        "app_id": vm.app_id,
                        "running": vm.running,
                        "configuration": config,
                        "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                    })
                })
                .collect();
            let total = vms_json.len();
            json!({ "vms": vms_json, "total": total }).to_string()
        }
        "CreateVm" => {
            let annotations: BTreeMap<String, String> = req["annotations"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            guard.insert(
                TEST_UUID.into(),
                FakeVm {
                    name: req["name"].as_str().unwrap().into(),
                    image: req["image"].as_str().unwrap().into(),
                    vcpu: req["vcpu"].as_u64().unwrap() as u32,
                    memory: req["memory"].as_u64().unwrap() as u32,
                    app_id: req["app_id"].as_str().unwrap_or("").into(),
                    status: "stopped".into(),
                    running: false,
                    stopped_flag: true,
                    annotations,
                    remove_polls_left: 0,
                },
            );
            json!({ "id": TEST_UUID }).to_string()
        }
        "GetInfo" => {
            let id = req["id"].as_str().unwrap_or("");
            let mut gone = false;
            let resp = match guard.get_mut(id) {
                Some(vm) if vm.status == "removing" => {
                    if vm.remove_polls_left == 0 {
                        gone = true;
                        json!({ "found": false, "info": null })
                    } else {
                        vm.remove_polls_left -= 1;
                        json!({
                            "found": true,
                            "info": {
                                "id": id,
                                "name": vm.name,
                                "status": "removing",
                                "app_id": vm.app_id,
                                "running": false,
                                "uptime": "",
                                "configuration": {
                                    "image": vm.image,
                                    "vcpu": vm.vcpu,
                                    "memory": vm.memory,
                                    "app_id": vm.app_id,
                                    "stopped": vm.stopped_flag,
                                    "annotations": vm.annotations,
                                },
                                "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                            }
                        })
                    }
                }
                Some(vm) => json!({
                    "found": true,
                    "info": {
                        "id": id,
                        "name": vm.name,
                        "status": vm.status,
                        "app_id": vm.app_id,
                        "running": vm.running,
                        "uptime": if vm.running { "1m 0s" } else { "" },
                        "configuration": {
                            "image": vm.image,
                            "vcpu": vm.vcpu,
                            "memory": vm.memory,
                            "app_id": vm.app_id,
                            "stopped": vm.stopped_flag,
                            "annotations": vm.annotations,
                        },
                        "interfaces": [{ "mode": "bridge", "mac": TEST_MAC }]
                    }
                }),
                None => json!({ "found": false, "info": null }),
            };
            if gone {
                guard.remove(id);
            }
            resp.to_string()
        }
        "StartVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = true;
            vm.stopped_flag = false;
            vm.status = "running".into();
            "{}".into()
        }
        "ShutdownVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.status = "exited".into();
            "{}".into()
        }
        "UpdateVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            if let Some(img) = req.get("image").and_then(Value::as_str) {
                vm.image = img.to_owned();
            }
            "{}".into()
        }
        "StopVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.stopped_flag = true;
            vm.status = "stopped".into();
            "{}".into()
        }
        "RemoveVm" => {
            let vm = guard.get_mut(req["id"].as_str().unwrap()).unwrap();
            vm.running = false;
            vm.status = "removing".into();
            "{}".into()
        }
        _ => "{}".into(),
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

#[test]
fn c1_local_exec_full_staged_disk_owner_tag_and_expect_vm_id_flow() {
    if !has_disk_and_vm_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs/qemu-img not all available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let chain_id = 424_242_u64;
    let founder_tree = dir.path().join("founder-0");
    build_founder_tree(&founder_tree, chain_id);
    let bundle = build_operator_bundle(dir.path(), chain_id);
    let tar_bytes = tar_dir_bytes(&founder_tree);

    let vmm_home = dir.path().join("vmm");
    let sock = vmm_home.join("run/vmm.sock");
    let state_dir = dir.path().join("state");
    let leases = dir.path().join("dhcp-leases");
    fs::write(
        &leases,
        format!("1795598734 {TEST_MAC} 10.77.0.91 validator-0 *\n"),
    )
    .unwrap();

    let image_dir = dir.path().join("img");
    fs::create_dir_all(&image_dir).unwrap();
    let digest = "abcdef0123456789".repeat(4);
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();

    let server = FakeVmmServer::start(sock.clone());

    // 1. Stdin tar without --bundle or --skip-key-verify fails closed.
    let out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--staging-name",
            "validator-0",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&tar_bytes),
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--bundle"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. Plain tar on stdin with verified bundle stages atomically to <state>/staging/validator-0/data.img.
    let out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--bundle",
            bundle.to_str().unwrap(),
            "--staging-name",
            "validator-0",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&tar_bytes),
    );
    assert!(
        out.status.success(),
        "disk prepare stdin failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let prep: Value = serde_json::from_slice(&out.stdout).unwrap();
    let disk_sha = prep["sha256"].as_str().unwrap().to_owned();
    assert_eq!(disk_sha.len(), 64);
    assert_eq!(prep["staging_name"], json!("validator-0"));
    assert_eq!(prep["key_verified"], json!(true));

    let staged_dir = state_dir.join("staging/validator-0");
    let staged_img = staged_dir.join("data.img");
    assert_eq!(
        fs::metadata(&staged_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&staged_img).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Zero leftover temporary staging dirs under <state>/staging.
    for entry in fs::read_dir(state_dir.join("staging")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(".outbe-"),
            "unexpected leftover temp staging dir: {name}"
        );
    }

    // 3. `disk clean --list --json` reports exact staged disk metadata.
    let out = LocalExec::run(
        &[
            "disk",
            "clean",
            "--list",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out.status.success());
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = listed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["staging_name"], json!("validator-0"));
    assert_eq!(arr[0]["sha256"], json!(disk_sha));
    assert_eq!(arr[0]["size_bytes"], json!(1024_u64 * 1024 * 1024));

    // 4. Re-running `disk prepare --staging-name validator-0` with zstd stdin cleanly replaces it.
    let zstd_bytes = zstd_compress_bytes(&tar_bytes);
    let out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--bundle",
            bundle.to_str().unwrap(),
            "--staging-name",
            "validator-0",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&zstd_bytes),
    );
    assert!(
        out.status.success(),
        "second disk prepare replacement failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let prep2: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(prep2["sha256"], json!(disk_sha));

    // 5. `vm create --owner-tag ... --disk-staging validator-0 --consume-staging` creates VM,
    //    annotates VMM with outbe-up.owner-tag, and removes staged disk AFTER Ready.
    let owner_tag = "outbe-net:devnet:01234567:validator-0";
    let out = LocalExec::run(
        &[
            "vm",
            "create",
            "--name",
            "validator-0",
            "--owner-tag",
            owner_tag,
            "--image",
            image_dir.to_str().unwrap(),
            "--disk-staging",
            "validator-0",
            "--consume-staging",
            "--bridge",
            "obr0",
            "--ip",
            "10.77.0.10",
            "--disk-gib",
            "1",
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "vm create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let created: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(created["vm_id"], json!(TEST_UUID));
    assert_eq!(created["owner_tag"], json!(owner_tag));
    assert_eq!(created["staging_consumed"], json!(true));
    assert!(
        !staged_dir.exists(),
        "--consume-staging must remove staged dir after Ready"
    );

    // Verify VMM received OWNER_TAG_ANNOTATION on CreateVm.
    {
        let g = server.vms.lock().unwrap();
        let vm = g.get(TEST_UUID).unwrap();
        assert_eq!(
            vm.annotations.get(OWNER_TAG_ANNOTATION).map(String::as_str),
            Some(owner_tag)
        );
    }

    // `disk clean --list --json` is now empty.
    let out = LocalExec::run(
        &[
            "disk",
            "clean",
            "--list",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!([])
    );

    // 6. `vm status --json` (both single and all-VM) exposes all C1 fields.
    let out = LocalExec::run(
        &[
            "vm",
            "status",
            "--name",
            "validator-0",
            "--dhcp-leases",
            leases.to_str().unwrap(),
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out.status.success());
    let st: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(st["owner_tag"], json!(owner_tag));
    assert_eq!(st["disk_source_sha256"], json!(disk_sha));
    assert!(st["app_id"].as_str().is_some_and(|s| s.len() == 40));
    assert_eq!(st["no_tee"], json!(false));
    assert_eq!(st["vcpu"], json!(8));
    assert_eq!(st["memory_gib"], json!(16));
    assert_eq!(st["disk_gib"], json!(1));
    assert_eq!(st["service_ip"], json!("10.77.0.10"));
    assert_eq!(st["dhcp_ip"], json!("10.77.0.91"));

    // Seed serial.log so `vm logs` succeeds on matching UUID.
    let serial_log = vmm_home.join(format!("vm/{TEST_UUID}/serial.log"));
    fs::create_dir_all(serial_log.parent().unwrap()).unwrap();
    fs::write(&serial_log, "boot ok\n").unwrap();

    // 7. `--expect-vm-id` mismatch fails closed with ZERO VMM RPCs across all 5 verbs.
    let rpc_before = server.rpc_count.load(Ordering::SeqCst);
    for sub in ["status", "start", "logs", "stop", "remove"] {
        let out = LocalExec::run(
            &[
                "vm",
                sub,
                "--name",
                "validator-0",
                "--expect-vm-id",
                WRONG_UUID,
                "--vmm-socket",
                sock.to_str().unwrap(),
                "--state-dir",
                state_dir.to_str().unwrap(),
                "--json",
            ],
            None,
        );
        assert!(
            !out.status.success(),
            "vm {sub} with wrong --expect-vm-id must fail"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("vm_id_mismatch"),
            "vm {sub} stderr missing vm_id_mismatch: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert_eq!(
        server.rpc_count.load(Ordering::SeqCst),
        rpc_before,
        "mismatched --expect-vm-id must issue ZERO VMM RPCs"
    );

    // 8. `--expect-vm-id` matching UUID succeeds across status -> start -> update -> logs -> stop -> remove.
    for sub in ["status", "start", "logs"] {
        let out = LocalExec::run(
            &[
                "vm",
                sub,
                "--name",
                "validator-0",
                "--expect-vm-id",
                TEST_UUID,
                "--vmm-socket",
                sock.to_str().unwrap(),
                "--state-dir",
                state_dir.to_str().unwrap(),
                "--json",
            ],
            None,
        );
        assert!(
            out.status.success(),
            "vm {sub} with matching --expect-vm-id failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // S7: `vm update` switches guest image gracefully (never StopVm) and preserves vm_id/hda.img.
    let image_v2 = dir.path().join("image-v2");
    fs::create_dir_all(&image_v2).unwrap();
    let v2_digest = "2".repeat(64);
    fs::write(image_v2.join("metadata.json"), "{}").unwrap();
    fs::write(image_v2.join("digest.txt"), format!("{v2_digest}\n")).unwrap();
    let out = LocalExec::run(
        &[
            "vm",
            "update",
            "--name",
            "validator-0",
            "--expect-vm-id",
            TEST_UUID,
            "--image",
            image_v2.to_str().unwrap(),
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "vm update failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let updated: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(updated["vm_id"], json!(TEST_UUID));
    assert_eq!(updated["os_image_hash"], json!(v2_digest));
    assert_eq!(updated["already_updated"], json!(false));

    // Idempotent second `vm update` returns already_updated=true.
    let out = LocalExec::run(
        &[
            "vm",
            "update",
            "--name",
            "validator-0",
            "--expect-vm-id",
            TEST_UUID,
            "--image",
            updated["image_name"].as_str().unwrap(),
            "--vmm-socket",
            sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "idempotent vm update failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rerun: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rerun["already_updated"], json!(true));

    for sub in ["stop", "remove"] {
        let out = LocalExec::run(
            &[
                "vm",
                sub,
                "--name",
                "validator-0",
                "--expect-vm-id",
                TEST_UUID,
                "--vmm-socket",
                sock.to_str().unwrap(),
                "--state-dir",
                state_dir.to_str().unwrap(),
                "--json",
            ],
            None,
        );
        assert!(
            out.status.success(),
            "vm {sub} with matching --expect-vm-id failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn c1_stdin_tar_rejections_failed_create_preserves_staging_and_disk_clean_modes() {
    if !has_disk_and_vm_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs/qemu-img not all available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");

    // 1. Stdin tar with `..` traversal is rejected and leaves no staged data.img.
    let mut evil_tar = Vec::new();
    {
        let mut header = [0u8; 512];
        header[..9].copy_from_slice(b"../escape");
        header[100..108].copy_from_slice(b"0000600\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000004\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        let cksum_str = format!("{cksum:06o}\0 ");
        header[148..156].copy_from_slice(cksum_str.as_bytes());
        evil_tar.extend_from_slice(&header);
        let mut block = [0u8; 512];
        block[..4].copy_from_slice(b"evil");
        evil_tar.extend_from_slice(&block);
        evil_tar.extend_from_slice(&[0u8; 1024]);
    }
    let out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--skip-key-verify",
            "--staging-name",
            "evil-vm",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&evil_tar),
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unsafe path"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!state_dir.join("staging/evil-vm/data.img").exists());

    // 2. Stage valid disk, then run a failing `vm create --consume-staging` -> staged disk preserved!
    let founder_tree = dir.path().join("founder-1");
    build_founder_tree(&founder_tree, 424_242);
    let tar_bytes = tar_dir_bytes(&founder_tree);
    let out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--skip-key-verify",
            "--staging-name",
            "validator-1",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&tar_bytes),
    );
    assert!(
        out.status.success(),
        "disk prepare failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let image_dir = dir.path().join("img");
    fs::create_dir_all(&image_dir).unwrap();
    fs::write(image_dir.join("metadata.json"), "{}").unwrap();
    fs::write(
        image_dir.join("digest.txt"),
        format!("{}\n", "0123456789abcdef".repeat(4)),
    )
    .unwrap();
    let dead_sock = dir.path().join("dead/run/vmm.sock");
    let out = LocalExec::run(
        &[
            "vm",
            "create",
            "--name",
            "validator-1",
            "--image",
            image_dir.to_str().unwrap(),
            "--disk-staging",
            "validator-1",
            "--consume-staging",
            "--bridge",
            "obr0",
            "--disk-gib",
            "1",
            "--vmm-socket",
            dead_sock.to_str().unwrap(),
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(
        !out.status.success(),
        "vm create against dead socket must fail"
    );
    assert!(
        state_dir.join("staging/validator-1/data.img").is_file(),
        "failed vm create must preserve staged disk for retry"
    );

    // 3. `disk clean --staging-name validator-1` removes it and is idempotent on second run.
    let out = LocalExec::run(
        &[
            "disk",
            "clean",
            "--staging-name",
            "validator-1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out.status.success());
    let cleaned: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(cleaned["staging_name"], json!("validator-1"));
    assert_eq!(cleaned["removed"], json!(true));
    assert!(!state_dir.join("staging/validator-1").exists());

    let out2 = LocalExec::run(
        &[
            "disk",
            "clean",
            "--staging-name",
            "validator-1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out2.status.success());
    let cleaned2: Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(cleaned2["removed"], json!(false));

    // 4. `disk clean --stale` removes unlocked `.outbe-prepare-*` dirs while sparing live locked ones.
    let stale_dir = state_dir.join("staging/.outbe-prepare-stale");
    let live_dir = state_dir.join("staging/.outbe-disk-live");
    fs::create_dir_all(&stale_dir).unwrap();
    fs::create_dir_all(&live_dir).unwrap();
    let live_fd = openat(
        CWD,
        &live_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    flock(&live_fd, FlockOperation::LockExclusive).unwrap();

    let out = LocalExec::run(
        &[
            "disk",
            "clean",
            "--stale",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(out.status.success());
    let stale_out: Value = serde_json::from_slice(&out.stdout).unwrap();
    let removed = stale_out["removed"].as_array().unwrap();
    assert_eq!(removed.len(), 1);
    assert!(!stale_dir.exists());
    assert!(
        live_dir.is_dir(),
        "live locked staging dir must not be deleted"
    );
    drop(live_fd);
}

#[test]
fn c1_f1_concurrent_disk_clean_stale_never_disrupts_live_staged_prepare_and_f5_list_isolates_bad_entries(
) {
    if !has_disk_and_vm_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs/qemu-img not all available");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let founder_tree = dir.path().join("founder-f1");
    build_founder_tree(&founder_tree, 424_242);
    let tar_bytes = tar_dir_bytes(&founder_tree);

    // Seed an unlocked stale dir alongside a live concurrent `disk prepare --staging-name`.
    let staging_root = state_dir.join("staging");
    fs::create_dir_all(&staging_root).unwrap();
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&staging_root, fs::Permissions::from_mode(0o700)).unwrap();
    let stale_dir = staging_root.join(".outbe-prepare-leftover");
    fs::create_dir_all(&stale_dir).unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let done_sweeper = Arc::clone(&done);
    let state_dir_sweeper = state_dir.clone();
    let sweep_count = Arc::new(AtomicUsize::new(0));
    let sweep_count_bg = Arc::clone(&sweep_count);

    let sweeper = thread::spawn(move || {
        while !done_sweeper.load(Ordering::Relaxed) {
            let out = LocalExec::run(
                &[
                    "disk",
                    "clean",
                    "--stale",
                    "--state-dir",
                    state_dir_sweeper.to_str().unwrap(),
                    "--json",
                ],
                None,
            );
            assert!(
                out.status.success(),
                "disk clean --stale failed during live prepare: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            sweep_count_bg.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(10));
        }
    });

    let prep_out = LocalExec::run(
        &[
            "disk",
            "prepare",
            "--role",
            "founder",
            "--source",
            "-",
            "--skip-key-verify",
            "--staging-name",
            "validator-live",
            "--size-gib",
            "1",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        Some(&tar_bytes),
    );
    done.store(true, Ordering::SeqCst);
    sweeper.join().unwrap();

    assert!(
        prep_out.status.success(),
        "F1 regression: concurrent `disk clean --stale` broke live `disk prepare`: {}",
        String::from_utf8_lossy(&prep_out.stderr)
    );
    assert!(
        sweep_count.load(Ordering::SeqCst) > 0,
        "sweeper thread must have executed at least one `disk clean --stale`"
    );
    assert!(
        !stale_dir.exists(),
        "unlocked stale dir must have been cleaned"
    );
    let prep_json: Value = serde_json::from_slice(&prep_out.stdout).unwrap();
    let live_sha = prep_json["sha256"].as_str().unwrap().to_owned();

    // F5 CLI check: add a bad-mode dir and a stray file next to `validator-live` and verify
    // `disk clean --list --json` exits 0, lists `validator-live` with valid sha256, and reports
    // the bad entries with `error`.
    let bad_dir = staging_root.join("bad-perms");
    fs::create_dir_all(&bad_dir).unwrap();
    fs::set_permissions(&bad_dir, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(staging_root.join("stray-file"), b"junk").unwrap();

    let list_out = LocalExec::run(
        &[
            "disk",
            "clean",
            "--list",
            "--state-dir",
            state_dir.to_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert!(
        list_out.status.success(),
        "F5 regression: disk clean --list aborted on bad entry: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let entries = serde_json::from_slice::<Value>(&list_out.stdout)
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    let valid = entries
        .iter()
        .find(|e| e["staging_name"] == "validator-live")
        .unwrap();
    assert_eq!(valid["sha256"], json!(live_sha));
    assert!(valid.get("error").is_none());
    for bad in ["bad-perms", "stray-file"] {
        let entry = entries.iter().find(|e| e["staging_name"] == bad).unwrap();
        assert!(
            entry["error"].as_str().is_some_and(|s| !s.is_empty()),
            "expected error for {bad}: {entry}"
        );
        assert!(entry.get("sha256").is_none());
    }
}
