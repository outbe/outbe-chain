use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use eyre::{bail, Result};
use outbe_operator_node_role::bundle::{self, BundleInput};
use outbe_operator_remote::{HostExec, Ssh};
use outbe_software_bundle::{sha256_file, SoftwareFile, SoftwareManifest, MANIFEST_NAME};
use serde_json::{json, Value};

use super::*;
use crate::network::vm_state::{MemberRole, NetworkVmState};

// ---------------------------------------------------------------------------
// Test fixture helpers (artifacts, verified software bundle, operator bundle, image archive)
// ---------------------------------------------------------------------------

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn build_software_bundle_with_outbe_up(bundle_path: &Path, outbe_up_bytes: &[u8]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let bin_file = tmp.path().join("outbe-up");
    write_mode(&bin_file, outbe_up_bytes, 0o755);
    let bin_sha = sha256_file(&bin_file).unwrap();

    let manifest = SoftwareManifest {
        version: 1,
        commit: "a".repeat(40),
        release_tag: "v0.1.0-test".to_owned(),
        target: "x86_64-unknown-linux-gnu".to_owned(),
        files: vec![SoftwareFile {
            path: "bin/outbe-up".to_owned(),
            size: outbe_up_bytes.len() as u64,
            sha256: bin_sha.clone(),
            mode: 0o755,
        }],
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

    let out_file = fs::File::create(bundle_path).unwrap();
    let encoder = zstd::Encoder::new(out_file, 1).unwrap();
    let mut builder = tar::Builder::new(encoder);

    let mut mh = tar::Header::new_gnu();
    mh.set_entry_type(tar::EntryType::Regular);
    mh.set_size(manifest_bytes.len() as u64);
    mh.set_mode(0o644);
    mh.set_cksum();
    builder
        .append_data(&mut mh, MANIFEST_NAME, manifest_bytes.as_slice())
        .unwrap();

    let mut bh = tar::Header::new_gnu();
    bh.set_entry_type(tar::EntryType::Regular);
    bh.set_size(outbe_up_bytes.len() as u64);
    bh.set_mode(0o755);
    bh.set_cksum();
    builder
        .append_data(&mut bh, "bin/outbe-up", outbe_up_bytes)
        .unwrap();

    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
    bin_sha
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
        if [ "$1" = "--key" ]; then key_path="$2"; shift 2; else shift 1; fi
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
    let _ = fs::remove_dir_all(&src);
    bundle_path
}

fn build_founder_tree(tree: &Path, chain_id: u64, ip: Ipv4Addr) {
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
                "OUTBE_EXTERNAL_IP=\"{ip}\"\n",
                "OUTBE_RPC_BIND_IP=\"{ip}\"\n",
                "OUTBE_VM_PREFIX=\"24\"\n",
                "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
                "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
                "OUTBE_CONSENSUS_PORT=\"30400\"\n",
                "OUTBE_RETH_PORT=\"30303\"\n",
                "OUTBE_DISCV5_PORT=\"31303\"\n",
                "OUTBE_RADICLE_PORT=\"8776\"\n",
                "OUTBE_CERTIFIED_RPC=\"http://{ip}:8545\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"0x1111111111111111111111111111111111111111\"\n",
                "OUTBE_BOOTNODES=\"enode://abcd@{ip}:30303\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{peer_key}@{ip}:30400\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"--consensus.use-local-defaults\"\n",
                "OUTBE_CHAIN_ID=\"{chain_id}\"\n",
            ),
            ip = ip,
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
                "public_url = \"http://{ip}:9000\"\n\n",
                "[[origin]]\n",
                "domain = {chain_id}\n",
                "rpc_urls = [\"http://127.0.0.1:8545\"]\n",
                "rpc_consensus = \"single\"\n",
                "reorg_period = 0\n",
            ),
            ip = ip,
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

struct TestArtifacts {
    root: PathBuf,
    image_archive: PathBuf,
    outbe_up_sha256: String,
    genesis_sha256: String,
}

fn build_test_vm_artifacts(
    dir: &Path,
    network_name: &str,
    vmm_home: &str,
    outbe_up_bytes: &[u8],
) -> TestArtifacts {
    let root = dir.join("artifacts");
    fs::create_dir_all(&root).unwrap();
    let chain_id = 424_242_u64;

    let sw_bundle = root.join("software-bundle.tar.zst");
    let outbe_up_sha256 = build_software_bundle_with_outbe_up(&sw_bundle, outbe_up_bytes);

    let joining_dir = root.join("joining");
    fs::create_dir_all(&joining_dir).unwrap();
    let op_bundle = build_operator_bundle(&joining_dir, chain_id);

    let mut founders_yaml = String::new();
    let mut founders_manifest = Vec::new();
    for idx in 0..4usize {
        let ip = Ipv4Addr::new(10, 77, 0, 10 + idx as u8);
        let rel = format!("vm/validator-{idx}");
        build_founder_tree(&root.join(&rel), chain_id, ip);
        founders_yaml.push_str(&format!(
            "  - name: validator-{idx}\n    advertise_ip: {ip}\n    rpc_bind_ip: {ip}\n    hyperlane_public_url: http://{ip}:9000\n    vm:\n      bridge_ip: {ip}\n"
        ));
        founders_manifest.push(json!({
            "index": idx,
            "name": format!("validator-{idx}"),
            "advertise_ip": ip.to_string(),
            "validator_address": "0x1111111111111111111111111111111111111111",
            "hyperlane_address": "0x2222222222222222222222222222222222222222",
            "consensus_public_key": "aa",
            "vm": {
                "data_tree": rel,
                "vmm_host": "192.0.2.50",
                "vmm_user": "ubuntu",
                "vmm_port": 22,
                "bridge_ip": ip.to_string(),
                "port_offset": 0,
                "vcpu": 3,
                "memory_gib": 5,
                "disk_gib": 1,
                "flavor": "dev",
                "has_authorized_keys": false
            }
        }));
    }

    let network_yaml = format!(
        r#"version: 1
name: {network_name}
network: devnet
chain_id: {chain_id}
certified_rpc: "http://10.77.0.10:8545"
vm:
  vmm_host:
    host: 192.0.2.50
  vmm_home: "{vmm_home}"
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
  disk_gib: 1
founders:
{founders_yaml}hyperlane:
  gasless: true
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#
    );
    fs::write(root.join("network.yaml"), &network_yaml).unwrap();
    fs::copy(
        root.join("vm/validator-0/genesis.json"),
        root.join("genesis.json"),
    )
    .unwrap();
    let genesis_sha256 = sha256_file(&root.join("genesis.json")).unwrap();
    let (genesis_block_hash, _) =
        crate::network::build::genesis_identity(&root.join("genesis.json")).unwrap();

    let manifest = json!({
        "version": 1,
        "state": "ready",
        "deployment": "vm",
        "name": network_name,
        "network": "devnet",
        "chain_id": chain_id,
        "config_sha256": sha256_file(&root.join("network.yaml")).unwrap(),
        "software": {
            "archive_sha256": sha256_file(&sw_bundle).unwrap(),
            "commit": "a".repeat(40),
            "release_tag": "v0.1.0-test",
            "target": "x86_64-unknown-linux-gnu",
        },
        "genesis": {
            "seeded_path": "genesis.seeded.json",
            "seeded_sha256": "00",
            "final_path": "genesis.json",
            "final_sha256": genesis_sha256,
            "genesis_block_hash": genesis_block_hash,
        },
        "founders": founders_manifest,
        "joining": {
            "bundle": "joining/outbe-validator-operator-bundle.tar.zst",
            "bundle_sha256": sha256_file(&op_bundle).unwrap(),
            "config_example": "joining/operator-config.example.yaml",
        }
    });
    fs::write(
        root.join("network-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let image_archive = build_real_guest_image_archive(dir);

    TestArtifacts {
        root,
        image_archive,
        outbe_up_sha256,
        genesis_sha256,
    }
}

// ---------------------------------------------------------------------------
// Faithful fake HostExec (rejects duplicates, enforces owner_tag & expect-vm-id,
// supports fault injection on every step, and tracks RPC calls to service IPs)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct FakeHostVm {
    name: String,
    vm_id: String,
    owner_tag: Option<String>,
    phase: String,
    vmm_status: String,
    running: bool,
    mac: String,
    service_ip: Option<Ipv4Addr>,
    dhcp_ip: Option<Ipv4Addr>,
    image_name: String,
    os_image_hash: String,
    disk_source_sha256: Option<String>,
    app_id: String,
    no_tee: bool,
    vcpu: u32,
    memory_gib: u32,
    disk_gib: u32,
    bridge: String,
}

#[derive(Default)]
struct FakeHostState {
    installed_bins: BTreeMap<String, String>, // path_suffix -> sha256
    version_override_sha: Option<String>,
    host_ready_for_vm: bool,
    host_check_ready: bool,
    image_name: String,
    os_image_hash: String,
    imported_images: BTreeSet<String>,
    image_archive_uploads: usize,
    vms: BTreeMap<String, FakeHostVm>,
    staged_disks: BTreeMap<String, String>, // staging_name -> sha256
    stale_cleaned_count: usize,
    disk_prepare_calls: usize,
    disk_prepare_bundles: Vec<String>,
    vm_create_calls: usize,
    vm_start_calls: Vec<(String, String)>, // (name, expect_vm_id)
    vm_stop_calls: Vec<(String, String, bool)>,
    vm_remove_calls: Vec<(String, Option<String>)>,
    rpc_urls_hit: Vec<String>,
    /// Per-poll finalized heights returned by batched JSON-RPC (advances on each full network poll).
    rpc_height_schedule: Vec<u64>,
    rpc_poll_counter: usize,
    rpc_enclave_ready: bool,
    rpc_vrf_version: u64,
    /// Optional fault injection: fail N-th `vm create` or `disk prepare` or `vm remove` call once.
    fail_on_vm_create_call: Option<usize>,
    fail_on_disk_prepare_call: Option<usize>,
    fail_remove_vm_once: Option<String>,
    next_vm_seq: u32,
    uploaded_op_bundles: BTreeSet<String>,
}

#[derive(Clone)]
struct FakeHostExec {
    state: Arc<Mutex<FakeHostState>>,
}

impl FakeHostExec {
    fn new() -> Self {
        let s = FakeHostState {
            host_ready_for_vm: true,
            host_check_ready: true,
            image_name: "outbe-vm-dev-0123456789abcdef".to_owned(),
            os_image_hash: "0123456789abcdef".repeat(4),
            rpc_height_schedule: vec![100, 110, 111, 112],
            rpc_enclave_ready: true,
            rpc_vrf_version: 1,
            ..Default::default()
        };
        Self {
            state: Arc::new(Mutex::new(s)),
        }
    }
}

fn extract_flag_value(cmd: &str, flag: &str) -> Option<String> {
    let tokens = sh_tokens(cmd);
    for pair in tokens.windows(2) {
        if pair[0] == flag {
            return Some(pair[1].clone());
        }
    }
    None
}

fn sh_tokens(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    for c in cmd.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ' ' | '\t' | '\n' if !in_single && !in_double => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn extract_pinned_bin_name(cmd: &str) -> Option<String> {
    sh_tokens(cmd).into_iter().rev().find_map(|t| {
        let cleaned = t.trim_end_matches(';');
        let last = cleaned.rsplit('/').next()?;
        last.starts_with("outbe-up-").then(|| last.to_owned())
    })
}

impl HostExec for FakeHostExec {
    fn capture(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
        if command.contains("mktemp -d /tmp/") {
            let prefix = command
                .split("mktemp -d /tmp/")
                .nth(1)
                .and_then(|s| s.split('.').next())
                .unwrap_or("outbe-net-up-tmp");
            let mut g = self.state.lock().unwrap();
            g.next_vm_seq += 1;
            return Ok(format!("/tmp/{prefix}.{:06}", g.next_vm_seq));
        }
        Ok(String::new())
    }

    fn capture_raw(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
        let mut g = self.state.lock().unwrap();

        if command.contains(" version --json") {
            let bin = extract_pinned_bin_name(command).unwrap_or_default();
            let Some(sha) = g
                .version_override_sha
                .clone()
                .or_else(|| g.installed_bins.get(&bin).cloned())
            else {
                bail!("binary `{bin}` not installed");
            };
            return Ok(json!({
                "name": "outbe-up",
                "version": "0.1.0",
                "binary_path": format!("/home/ubuntu/.local/state/outbe-up/bin/{bin}"),
                "binary_sha256": sha,
            })
            .to_string());
        }

        if command.contains(" host status ") {
            return Ok(json!({
                "ready_for_vm": g.host_ready_for_vm,
                "notes": if g.host_ready_for_vm { json!([]) } else { json!(["bridge obr0 down"]) },
            })
            .to_string());
        }

        if command.contains(" disk clean --stale ") {
            g.stale_cleaned_count += 1;
            return Ok(json!({
                "staging_root": "/home/ubuntu/.local/state/outbe-up/staging",
                "removed": []
            })
            .to_string());
        }

        if command.contains(" disk clean --list ") {
            let items: Vec<Value> = g
                .staged_disks
                .iter()
                .map(|(name, sha)| {
                    json!({
                        "staging_name": name,
                        "path": format!("/home/ubuntu/.local/state/outbe-up/staging/{name}/data.img"),
                        "sha256": sha,
                        "size_bytes": 1073741824_u64,
                    })
                })
                .collect();
            return Ok(Value::Array(items).to_string());
        }

        if command.contains(" disk clean --staging-name ") {
            let name = extract_flag_value(command, "--staging-name").unwrap();
            let removed = g.staged_disks.remove(&name).is_some();
            return Ok(json!({
                "staging_name": name,
                "path": format!("/home/ubuntu/.local/state/outbe-up/staging/{name}"),
                "removed": removed,
            })
            .to_string());
        }

        if command.contains(" image list ") {
            let list: Vec<Value> = g
                .imported_images
                .iter()
                .map(|hash| {
                    json!({
                        "name": g.image_name,
                        "flavor": "dev",
                        "os_image_hash": hash,
                        "path": format!("/home/ubuntu/.dstack-vmm/image/{}", g.image_name),
                        "has_archive_manifest": true,
                    })
                })
                .collect();
            return Ok(Value::Array(list).to_string());
        }

        if command.contains(" image import ") || command.contains(" image fetch ") {
            let hash = g.os_image_hash.clone();
            g.imported_images.insert(hash);
            return Ok(json!({
                "image_name": g.image_name,
                "os_image_hash": g.os_image_hash,
                "target_dir": format!("/home/ubuntu/.dstack-vmm/image/{}", g.image_name),
                "reused": true,
            })
            .to_string());
        }

        if command.contains(" vm status ") {
            let list: Vec<Value> = g
                .vms
                .values()
                .map(|vm| {
                    json!({
                        "name": vm.name,
                        "vm_id": vm.vm_id,
                        "owner_tag": vm.owner_tag,
                        "phase": vm.phase,
                        "vmm_status": vm.vmm_status,
                        "running": vm.running,
                        "uptime": if vm.running { "1m 0s" } else { "" },
                        "mac": vm.mac,
                        "ip": vm.service_ip,
                        "service_ip": vm.service_ip,
                        "dhcp_ip": vm.dhcp_ip,
                        "intended_ip": vm.service_ip,
                        "image_name": vm.image_name,
                        "os_image_hash": vm.os_image_hash,
                        "disk_source_sha256": vm.disk_source_sha256,
                        "app_id": vm.app_id,
                        "no_tee": vm.no_tee,
                        "vcpu": vm.vcpu,
                        "memory_gib": vm.memory_gib,
                        "disk_gib": vm.disk_gib,
                        "bridge": vm.bridge,
                    })
                })
                .collect();
            return Ok(Value::Array(list).to_string());
        }

        if command.contains(" vm create ") {
            g.vm_create_calls += 1;
            if g.fail_on_vm_create_call == Some(g.vm_create_calls) {
                g.fail_on_vm_create_call = None;
                bail!("injected vm create failure");
            }
            let name = extract_flag_value(command, "--name").unwrap();
            let owner_tag = extract_flag_value(command, "--owner-tag");
            let staging_name = extract_flag_value(command, "--disk-staging").unwrap();
            let consume = command.contains(" --consume-staging");
            let bridge = extract_flag_value(command, "--bridge").unwrap();
            let ip: Ipv4Addr = extract_flag_value(command, "--ip")
                .unwrap()
                .parse()
                .unwrap();
            let vcpu: u32 = extract_flag_value(command, "--vcpu")
                .unwrap()
                .parse()
                .unwrap();
            let mem: u32 = extract_flag_value(command, "--memory-gib")
                .unwrap()
                .parse()
                .unwrap();
            let disk_gib: u32 = extract_flag_value(command, "--disk-gib")
                .unwrap()
                .parse()
                .unwrap();
            let no_tee = command.contains(" --no-tee");

            let Some(disk_sha) = g.staged_disks.get(&staging_name).cloned() else {
                bail!("staged disk `{staging_name}` missing on fake host");
            };

            // Amendment 9: refuse duplicate VM if already Ready (allow resuming Partial with matching tag & disk sha).
            if let Some(prev) = g.vms.get(&name) {
                if prev.phase == "ready" {
                    bail!("VM `{name}` already exists");
                }
                if prev.owner_tag != owner_tag {
                    bail!("partial VM `{name}` owner_tag mismatch");
                }
                if prev.disk_source_sha256.as_deref() != Some(disk_sha.as_str()) {
                    bail!("partial VM `{name}` disk_source_sha256 mismatch");
                }
            }

            g.next_vm_seq += 1;
            let seq = g.next_vm_seq;
            let vm_id = g
                .vms
                .get(&name)
                .filter(|v| !v.vm_id.is_empty())
                .map(|v| v.vm_id.clone())
                .unwrap_or_else(|| format!("550e8400-e29b-41d4-a716-{seq:012}"));
            let mac = format!("52:54:00:00:00:{:02x}", (seq & 0xff) as u8);
            let img_name = g.image_name.clone();
            let img_hash = g.os_image_hash.clone();
            g.vms.insert(
                name.clone(),
                FakeHostVm {
                    name: name.clone(),
                    vm_id: vm_id.clone(),
                    owner_tag: owner_tag.clone(),
                    phase: "ready".to_owned(),
                    vmm_status: "stopped".to_owned(),
                    running: false,
                    mac: mac.clone(),
                    service_ip: Some(ip),
                    dhcp_ip: Some(Ipv4Addr::new(10, 77, 0, 200 + (seq % 40) as u8)),
                    image_name: img_name.clone(),
                    os_image_hash: img_hash.clone(),
                    disk_source_sha256: Some(disk_sha),
                    app_id: "b".repeat(40),
                    no_tee,
                    vcpu,
                    memory_gib: mem,
                    disk_gib,
                    bridge: bridge.clone(),
                },
            );
            let consumed = if consume {
                g.staged_disks.remove(&staging_name).is_some()
            } else {
                false
            };
            return Ok(json!({
                "name": name,
                "vm_id": vm_id,
                "owner_tag": owner_tag,
                "image_name": img_name,
                "os_image_hash": img_hash,
                "image_reused": true,
                "mac": mac,
                "bridge": bridge,
                "intended_ip": ip,
                "vcpu": vcpu,
                "memory_gib": mem,
                "disk_gib": disk_gib,
                "phase": "ready",
                "staging_consumed": consumed,
                "state_file": format!("/state/vms/{name}.json"),
            })
            .to_string());
        }

        if command.contains(" host check ") {
            let vm_name = extract_flag_value(command, "--vm").unwrap();
            let vm = g
                .vms
                .get(&vm_name)
                .cloned()
                .ok_or_else(|| eyre::eyre!("host check: VM `{vm_name}` missing"))?;
            let ready = g.host_check_ready;
            let payload = json!({
                "vm": vm.name,
                "vm_id": vm.vm_id,
                "mac": vm.mac,
                "ip": vm.service_ip.unwrap(),
                "ready": ready,
                "host_network_ok": ready,
                "service_ip_ok": ready,
                "published_ports_ok": ready,
                "missing": if ready { json!([]) } else { json!(["bridge obr0 NAT missing"]) },
                "conflicts": [],
                "recommended_firewall_commands": if ready { json!([]) } else { json!(["sudo nft -f /etc/host-firewall.nft"]) },
            });
            let rc = if ready { 0 } else { 1 };
            return Ok(format!("{payload}\n{HOST_CHECK_EXIT_SENTINEL}{rc}\n"));
        }

        if command.contains(" vm start ") {
            let name = extract_flag_value(command, "--name").unwrap();
            let expect = extract_flag_value(command, "--expect-vm-id")
                .ok_or_else(|| eyre::eyre!("vm start missing --expect-vm-id"))?;
            let vm = g
                .vms
                .get_mut(&name)
                .ok_or_else(|| eyre::eyre!("vm start: `{name}` missing"))?;
            if vm.vm_id != expect {
                bail!(
                    "vm_id_mismatch for `{name}`: expected {expect}, got {}",
                    vm.vm_id
                );
            }
            vm.running = true;
            vm.vmm_status = "running".to_owned();
            let vm_id = vm.vm_id.clone();
            g.vm_start_calls.push((name.clone(), expect));
            return Ok(json!({
                "name": name,
                "vm_id": vm_id,
                "already_running": false,
                "vmm_status": "running",
            })
            .to_string());
        }

        if command.contains(" vm stop ") {
            let name = extract_flag_value(command, "--name").unwrap();
            let expect = extract_flag_value(command, "--expect-vm-id")
                .ok_or_else(|| eyre::eyre!("vm stop missing --expect-vm-id"))?;
            let force = command.contains(" --force");
            let vm = g
                .vms
                .get_mut(&name)
                .ok_or_else(|| eyre::eyre!("vm stop: `{name}` missing"))?;
            if vm.vm_id != expect {
                bail!(
                    "vm_id_mismatch for `{name}`: expected {expect}, got {}",
                    vm.vm_id
                );
            }
            vm.running = false;
            vm.vmm_status = if force { "stopped" } else { "exited" }.to_owned();
            let vm_id = vm.vm_id.clone();
            let status = vm.vmm_status.clone();
            g.vm_stop_calls.push((name.clone(), expect, force));
            return Ok(json!({
                "name": name,
                "vm_id": vm_id,
                "already_stopped": false,
                "forced": force,
                "vmm_status": status,
            })
            .to_string());
        }

        if command.contains(" vm remove ") {
            let name = extract_flag_value(command, "--name").unwrap();
            let expect = extract_flag_value(command, "--expect-vm-id");
            if g.fail_remove_vm_once.as_deref() == Some(name.as_str()) {
                g.fail_remove_vm_once = None;
                bail!("injected vm remove failure for `{name}`");
            }
            let vm = g
                .vms
                .get(&name)
                .cloned()
                .ok_or_else(|| eyre::eyre!("vm remove: `{name}` missing"))?;
            if let Some(ref exp) = expect {
                if vm.vm_id != *exp {
                    bail!(
                        "vm_id_mismatch for `{name}`: expected {exp}, got {}",
                        vm.vm_id
                    );
                }
            }
            g.vms.remove(&name);
            g.vm_remove_calls.push((name.clone(), expect));
            return Ok(json!({
                "name": name,
                "vm_id": vm.vm_id,
                "forgot_intent": vm.vm_id.is_empty(),
            })
            .to_string());
        }

        if command.contains(" vm logs ") {
            let name = extract_flag_value(command, "--name").unwrap();
            let vm_id = extract_flag_value(command, "--expect-vm-id").unwrap_or_default();
            return Ok(json!({
                "name": name,
                "vm_id": vm_id,
                "lines": 40,
                "log": "boot log line\n"
            })
            .to_string());
        }

        bail!("unhandled fake host command: {command}");
    }

    fn capture_stdin(
        &self,
        _ssh: &Ssh,
        _label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        let mut g = self.state.lock().unwrap();

        // 1. Pinned outbe-up binary install.
        if command.contains(".outbe-up-") && command.contains("cat >") {
            let bin = extract_pinned_bin_name(command).unwrap();
            let tmp = tempfile::NamedTempFile::new().unwrap();
            fs::write(tmp.path(), stdin_bytes).unwrap();
            let sha = sha256_file(tmp.path()).unwrap();
            g.installed_bins.insert(bin, sha);
            return Ok(String::new());
        }

        // 2. `disk prepare --role founder --source - --staging-name <vm>`.
        if command.contains(" disk prepare ") {
            g.disk_prepare_calls += 1;
            if g.fail_on_disk_prepare_call == Some(g.disk_prepare_calls) {
                g.fail_on_disk_prepare_call = None;
                bail!("injected disk prepare failure");
            }
            assert!(
                command.contains("--source -"),
                "disk prepare must stream stdin tar via --source -: {command}"
            );
            let bundle_arg = extract_flag_value(command, "--bundle")
                .expect("disk prepare must pass operator --bundle");
            assert!(
                bundle_arg.contains("outbe-validator-operator-bundle.tar.zst"),
                "disk prepare must pass operator bundle, not software bundle: {bundle_arg}"
            );
            g.disk_prepare_bundles.push(bundle_arg);
            let staging_name = extract_flag_value(command, "--staging-name").unwrap();
            assert!(!stdin_bytes.is_empty(), "tar stdin must be non-empty");
            // Compute deterministic hash from streamed tar bytes.
            let tmp = tempfile::NamedTempFile::new().unwrap();
            fs::write(tmp.path(), stdin_bytes).unwrap();
            let sha = sha256_file(tmp.path()).unwrap();
            g.staged_disks.insert(staging_name.clone(), sha.clone());
            return Ok(json!({
                "role": "founder",
                "output": format!("/home/ubuntu/.local/state/outbe-up/staging/{staging_name}/data.img"),
                "staging_name": staging_name,
                "size_gib": 1,
                "sha256": sha,
                "key_verified": true,
            })
            .to_string());
        }

        // 3. Batched JSON-RPC via curl.
        if command.contains("curl -fsS") {
            let url = sh_tokens(command).last().cloned().unwrap_or_default();
            g.rpc_urls_hit.push(url);
            let batch: Vec<Value> = serde_json::from_slice(stdin_bytes)?;
            assert_eq!(batch.len(), 4, "expected 4-call JSON-RPC batch");
            let vm_count = g.vms.len().max(1);
            let poll_idx = g.rpc_poll_counter / vm_count;
            g.rpc_poll_counter += 1;
            let height = *g
                .rpc_height_schedule
                .get(poll_idx)
                .or_else(|| g.rpc_height_schedule.last())
                .unwrap_or(&100);
            let peers = vm_count.saturating_sub(1);
            let enclave_ready = g.rpc_enclave_ready;
            let vrf_ver = g.rpc_vrf_version;
            let resp = json!([
                {"jsonrpc": "2.0", "id": 1, "result": format!("0x{:x}", 424_242_u64)},
                {"jsonrpc": "2.0", "id": 2, "result": {"number": format!("0x{height:x}"), "hash": "0x01"}},
                {"jsonrpc": "2.0", "id": 3, "result": format!("0x{peers:x}")},
                {"jsonrpc": "2.0", "id": 4, "result": {
                    "connectedPeers": peers,
                    "hasThresholdShares": true,
                    "lastFinalizedBlock": height,
                    "vrfMaterialVersion": vrf_ver,
                    "nextPlannedActivationHeight": 1200,
                    "isValidator": true,
                    "mode": "signer",
                    "enclave": {
                        "state": if enclave_ready { "ready" } else { "booting" },
                        "ready": enclave_ready,
                        "offerKeyReady": enclave_ready,
                    }
                }}
            ]);
            return Ok(resp.to_string());
        }

        bail!("unhandled capture_stdin command: {command}");
    }

    fn run(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
        Ok(())
    }

    fn run_quiet(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
        Ok(())
    }

    fn copy_to(
        &self,
        _ssh: &Ssh,
        _label: &str,
        sources: &[&Path],
        destination: &str,
        _recursive: bool,
    ) -> Result<()> {
        let mut g = self.state.lock().unwrap();
        for src in sources {
            if let Some(name) = src.file_name().and_then(|s| s.to_str()) {
                if name.contains("operator-bundle") {
                    g.uploaded_op_bundles.insert(destination.to_owned());
                } else if name.ends_with(".tar.zst") {
                    g.image_archive_uploads += 1;
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit + Fault-Injection Matrix Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c2_deploy_start_status_stop_remove_happy_path_and_exit_code_2_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture =
        build_test_vm_artifacts(tmp.path(), "devnet", "~/.dstack-vmm", b"fake-outbe-up-v1");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

    // 1. Deploy when host check fails -> MUST return ExitCodeError(2), zero wait loop, VMs still created.
    fake.state.lock().unwrap().host_check_ready = false;
    let err = deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: false,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    let exit_err = err
        .downcast_ref::<ExitCodeError>()
        .expect("deploy with failed host check must return ExitCodeError");
    assert_eq!(exit_err.code, EXIT_CODE_HOST_CONFIG_REQUIRED);

    // Verify pinned outbe-up installed, stale swept, 4 VMs created with owner_tag, 0 staged disks left.
    {
        let g = fake.state.lock().unwrap();
        assert_eq!(g.vms.len(), 4);
        assert!(g.staged_disks.is_empty());
        assert_eq!(g.disk_prepare_calls, 4);
        assert_eq!(g.stale_cleaned_count, 1);
        assert_eq!(g.image_archive_uploads, 1);
        assert_eq!(g.installed_bins.len(), 1);
        assert_eq!(
            g.installed_bins.values().next().map(String::as_str),
            Some(fixture.outbe_up_sha256.as_str())
        );
        let expected_tag0 = member_owner_tag("devnet", &fixture.genesis_sha256, "validator-0");
        assert_eq!(
            g.vms["devnet-validator-0"].owner_tag.as_deref(),
            Some(expected_tag0.as_str())
        );
    }

    // 2. Start while host check still fails -> MUST refuse without calling `vm start`.
    let start_err = start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 10,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(start_err.to_string().contains("host check"), "{start_err}");
    assert!(fake.state.lock().unwrap().vm_start_calls.is_empty());

    // 3. Operator configures host -> re-running deploy is an idempotent no-op returning Ok(()).
    fake.state.lock().unwrap().host_check_ready = true;
    deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: false,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert_eq!(fake.state.lock().unwrap().disk_prepare_calls, 4);
    assert_eq!(fake.state.lock().unwrap().vm_create_calls, 4);
    // F7: idempotent second deploy checks `image list --json` and skips re-uploading via `scp`.
    assert_eq!(fake.state.lock().unwrap().image_archive_uploads, 1);

    // 4. Start succeeds after growth >= +10 in TWO consecutive samples; RPC hits service IPs ONLY (10.77.0.10..13, never 10.77.0.200+).
    start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    {
        let g = fake.state.lock().unwrap();
        assert_eq!(g.vm_start_calls.len(), 4);
        assert!(
            g.rpc_urls_hit
                .iter()
                .all(|u| u.starts_with("http://10.77.0.1") && u.ends_with(":8545")),
            "RPC must target service IPs only, got: {:?}",
            g.rpc_urls_hit
        );
    }

    // 5. Status succeeds on running network.
    status_with_exec(
        &StatusOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            watch: false,
            max_lag: 20,
            json: true,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    // 6. Stop passes --expect-vm-id gracefully.
    stop_with_exec(
        &StopOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: false,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert_eq!(fake.state.lock().unwrap().vm_stop_calls.len(), 4);

    // 7. Remove cleans all tagged VMs + staging + verifies 0 left + deletes network-vm-state.json.
    remove_with_exec(
        &RemoveOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: false,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert!(fake.state.lock().unwrap().vms.is_empty());
    assert!(!NetworkVmState::path(&fixture.root).exists());
}

#[tokio::test]
async fn c2_fault_matrix_resume_reuses_staged_disk_rejects_foreign_stale_and_handles_no_tee() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture =
        build_test_vm_artifacts(tmp.path(), "devnet", "~/.dstack-vmm", b"fake-outbe-up-v1");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

    // 1. Fault injection: fail on 2nd `vm create` (staged disk #2 remains on host, state recorded its sha256).
    fake.state.lock().unwrap().fail_on_vm_create_call = Some(2);
    assert!(deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: true,
        },
        Arc::clone(&exec),
    )
    .await
    .is_err());

    // Status works on partial deployment (reports 1 ours, 3 absent) without crashing.
    let partial_status = status_with_exec(
        &StatusOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            watch: false,
            max_lag: 20,
            json: true,
        },
        Arc::clone(&exec),
    )
    .await;
    assert!(
        partial_status.is_err(),
        "partial network status reports unhealthy"
    );

    // Resume deploy: MUST reuse staged disk #2 (total disk_prepare_calls == 4, NOT 5!).
    deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: true,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert_eq!(
        fake.state.lock().unwrap().disk_prepare_calls,
        4,
        "retry must reuse staged disk #2 instead of re-preparing it"
    );

    // 2. Amendment 5: `no_tee` skips enclave TEE readiness check during `start`.
    fake.state.lock().unwrap().rpc_enclave_ready = false;
    start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    // 3. Partial remove failure MUST keep network-vm-state.json; retry deletes it cleanly AND
    //    leaves any foreign VM on the same host completely untouched!
    fake.state.lock().unwrap().vms.insert(
        "foreign-vm".to_owned(),
        FakeHostVm {
            name: "foreign-vm".to_owned(),
            vm_id: "550e8400-e29b-41d4-a716-999999999999".to_owned(),
            owner_tag: None,
            phase: "ready".to_owned(),
            vmm_status: "running".to_owned(),
            running: true,
            mac: "52:54:00:ff:ff:ff".to_owned(),
            service_ip: Some(Ipv4Addr::new(10, 77, 0, 99)),
            dhcp_ip: None,
            image_name: "other-img".to_owned(),
            os_image_hash: "9".repeat(64),
            disk_source_sha256: None,
            app_id: "9".repeat(40),
            no_tee: false,
            vcpu: 2,
            memory_gib: 4,
            disk_gib: 10,
            bridge: "obr0".to_owned(),
        },
    );
    fake.state.lock().unwrap().fail_remove_vm_once = Some("devnet-validator-2".to_owned());
    assert!(remove_with_exec(
        &RemoveOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: true,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .is_err());
    assert!(
        NetworkVmState::path(&fixture.root).exists(),
        "network-vm-state.json must be preserved while tagged VMs remain"
    );

    // Retry remove succeeds, deletes state, and preserves foreign-vm!
    remove_with_exec(
        &RemoveOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: true,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert!(!NetworkVmState::path(&fixture.root).exists());
    assert!(
        fake.state.lock().unwrap().vms.contains_key("foreign-vm"),
        "remove must never touch foreign VMs"
    );

    // 4. Foreign VM with colliding name blocks deploy cleanly.
    fake.state.lock().unwrap().vms.insert(
        "devnet-validator-0".to_owned(),
        FakeHostVm {
            name: "devnet-validator-0".to_owned(),
            vm_id: "550e8400-e29b-41d4-a716-888888888888".to_owned(),
            owner_tag: Some("outbe-net:devnet:deadbeef:validator-0".to_owned()),
            phase: "ready".to_owned(),
            vmm_status: "stopped".to_owned(),
            running: false,
            mac: "52:54:00:aa:aa:aa".to_owned(),
            service_ip: Some(Ipv4Addr::new(10, 77, 0, 10)),
            dhcp_ip: None,
            image_name: "outbe-vm-dev-0123456789abcdef".to_owned(),
            os_image_hash: "0123456789abcdef".repeat(4),
            disk_source_sha256: None,
            app_id: "b".repeat(40),
            no_tee: false,
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 1,
            bridge: "obr0".to_owned(),
        },
    );
    let foreign_err = deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: false,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(foreign_err.to_string().contains("FOREIGN"), "{foreign_err}");
    assert!(
        foreign_err.to_string().contains("not ours"),
        "foreign hint must identify VM as not ours: {foreign_err}"
    );
    assert!(
        !foreign_err.to_string().contains("vm remove"),
        "foreign hint must NOT suggest deleting someone else's VM: {foreign_err}"
    );

    // 4b. `status` must NOT install `outbe-up` when missing (read-only; instructs user to run deploy).
    {
        let fresh_fake = FakeHostExec::new();
        let fresh_exec: Arc<dyn HostExec> = Arc::new(fresh_fake.clone());
        let status_err = status_with_exec(
            &StatusOptions {
                artifacts: &fixture.root,
                ssh_key: None,
                ssh_config: None,
                watch: false,
                max_lag: 20,
                json: true,
            },
            fresh_exec,
        )
        .await
        .unwrap_err();
        let msg = format!("{status_err:#}");
        assert!(
            msg.contains("network deploy"),
            "status without pinned outbe-up must instruct running deploy: {msg}"
        );
        assert!(
            fresh_fake.state.lock().unwrap().installed_bins.is_empty(),
            "status must be read-only and never install outbe-up"
        );
    }

    // 5. Tampered outbe-up binary hash fails closed.
    fake.state.lock().unwrap().version_override_sha = Some("f".repeat(64));
    let ver_err = deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: false,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(
        ver_err.to_string().contains("SHA-256 verification"),
        "{ver_err}"
    );
}

#[test]
fn c2_classification_distinguishes_stale_foreign_partial_and_ours() {
    let base_desired = DesiredMember {
        index: 0,
        role: MemberRole::Founder,
        name: "validator-0".to_owned(),
        vm_name: "devnet-validator-0".to_owned(),
        owner_tag: "outbe-net:devnet:01234567:validator-0".to_owned(),
        vmm_host: Ssh {
            host: "192.0.2.50".to_owned(),
            user: "ubuntu".to_owned(),
            port: 22,
        },
        vmm_home: "~/.dstack-vmm".to_owned(),
        bridge: "obr0".to_owned(),
        service_ip: Ipv4Addr::new(10, 77, 0, 10),
        advertise_ip: Ipv4Addr::new(10, 77, 0, 10),
        port_offset: 0,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 40,
        flavor: VmFlavor::Dev,
        no_tee: false,
        image_name: Some("outbe-vm-dev-new".to_owned()),
        image_hash: Some("2".repeat(64)),
        recorded_disk_sha256: Some("a".repeat(64)),
    };

    let mut vm = RemoteVmStatusOutput {
        name: "devnet-validator-0".to_owned(),
        vm_id: "550e8400-e29b-41d4-a716-446655440010".to_owned(),
        owner_tag: Some("outbe-net:devnet:01234567:validator-0".to_owned()),
        phase: "ready".to_owned(),
        vmm_status: "running".to_owned(),
        running: true,
        uptime: "1m".to_owned(),
        mac: "52:54:00:aa:bb:cc".to_owned(),
        ip: Some(Ipv4Addr::new(10, 77, 0, 10)),
        service_ip: Some(Ipv4Addr::new(10, 77, 0, 10)),
        dhcp_ip: None,
        intended_ip: Some(Ipv4Addr::new(10, 77, 0, 10)),
        image_name: "outbe-vm-dev-old".to_owned(),
        os_image_hash: "1".repeat(64),
        disk_source_sha256: Some("a".repeat(64)),
        app_id: "c".repeat(40),
        no_tee: false,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 40,
        bridge: "obr0".to_owned(),
        boot_error: None,
        error: None,
    };

    // Image hash mismatch -> Stale (S7 rolling updates deferred).
    assert_eq!(
        classify_host_vm(&base_desired, Some(&vm)).kind(),
        VmClassKind::Stale
    );

    // Matching target image -> Ours.
    vm.image_name = "outbe-vm-dev-new".to_owned();
    vm.os_image_hash = "2".repeat(64);
    assert_eq!(
        classify_host_vm(&base_desired, Some(&vm)).kind(),
        VmClassKind::Ours
    );

    // Partial phase -> Partial.
    vm.phase = "creating".to_owned();
    assert_eq!(
        classify_host_vm(&base_desired, Some(&vm)).kind(),
        VmClassKind::Partial
    );

    // Drifted vcpu -> Stale.
    vm.phase = "ready".to_owned();
    vm.vcpu = 8;
    assert_eq!(
        classify_host_vm(&base_desired, Some(&vm)).kind(),
        VmClassKind::Stale
    );

    // Untagged or different genesis tag -> Foreign.
    vm.vcpu = 3;
    vm.owner_tag = None;
    assert_eq!(
        classify_host_vm(&base_desired, Some(&vm)).kind(),
        VmClassKind::Foreign
    );
    assert_eq!(
        classify_host_vm(&base_desired, None).kind(),
        VmClassKind::Absent
    );
}

#[test]
fn c2_readiness_requires_growth_common_vrf_and_respects_no_tee() {
    let make_status = |idx: u8, finalized: u64, vrf: u64, no_tee: bool, tee_ok: bool| {
        let consensus = ConsensusStatus {
            connected_peers: 3,
            has_threshold_shares: true,
            last_finalized_block: finalized,
            vrf_material_version: vrf,
            next_planned_activation_height: 1200,
            is_validator: true,
            mode: "signer".to_owned(),
            enclave: outbe_operator_node_role::rpc::EnclaveHealth {
                state: if tee_ok { "ready" } else { "booting" }.to_owned(),
                ready: tee_ok,
                offer_key_ready: tee_ok,
            },
        };
        VmMemberStatus {
            name: format!("validator-{idx}"),
            role: MemberRole::Founder,
            vm_name: format!("devnet-validator-{idx}"),
            vm_id: format!("id-{idx}"),
            host: "192.0.2.50".to_owned(),
            classification: VmClassKind::Ours,
            classification_detail: None,
            service_ip: Ipv4Addr::new(10, 77, 0, 10 + idx),
            dhcp_ip: None,
            mac: "52:54:00:00:00:01".to_owned(),
            vmm_status: "running".to_owned(),
            vmm_running: true,
            no_tee,
            chain_id: Some(424_242),
            finalized: Some(finalized),
            execution_peers: Some(3),
            consensus_mode: Some("signer".to_owned()),
            consensus_peers: Some(3),
            has_threshold_shares: Some(true),
            vrf_material_version: Some(vrf),
            tee_ready: if no_tee { None } else { Some(tee_ok) },
            consensus: Some(consensus),
            error: None,
        }
    };

    // Growth below B + 10 -> false.
    let s_low: Vec<_> = (0..4)
        .map(|i| make_status(i, 109, 1, false, true))
        .collect();
    let a_low = assess_members_lag(&s_low, 20);
    assert!(!evaluate_network_readiness(
        &s_low,
        &a_low,
        424_242,
        3,
        Some(100),
        10
    ));

    // Growth >= B + 10 with matching VRF and TEE ready -> true.
    let s_ok: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, false, true))
        .collect();
    let a_ok = assess_members_lag(&s_ok, 20);
    assert!(evaluate_network_readiness(
        &s_ok,
        &a_ok,
        424_242,
        3,
        Some(100),
        10
    ));

    // Divergent VRF version -> false.
    let mut s_vrf = s_ok.clone();
    s_vrf[3].vrf_material_version = Some(2);
    let a_vrf = assess_members_lag(&s_vrf, 20);
    assert!(!evaluate_network_readiness(
        &s_vrf,
        &a_vrf,
        424_242,
        3,
        Some(100),
        10
    ));

    // TEE not ready with no_tee=false -> false; with no_tee=true -> true!
    let s_notee_fail: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, false, false))
        .collect();
    let a_nf = assess_members_lag(&s_notee_fail, 20);
    assert!(!evaluate_network_readiness(
        &s_notee_fail,
        &a_nf,
        424_242,
        3,
        Some(100),
        10
    ));

    let s_notee_ok: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, true, false))
        .collect();
    let a_no = assess_members_lag(&s_notee_ok, 20);
    assert!(evaluate_network_readiness(
        &s_notee_ok,
        &a_no,
        424_242,
        3,
        Some(100),
        10
    ));

    // F1 regression: baseline B MUST be max(finalized) of the first full sample so a lagging node
    // catching up on a stalled chain (5000, 5000, 5000, 4000) -> (5000 x 4) is NOT ready.
    let first_sample = vec![
        make_status(0, 5000, 1, false, true),
        make_status(1, 5000, 1, false, true),
        make_status(2, 5000, 1, false, true),
        make_status(3, 4000, 1, false, true),
    ];
    let baseline = compute_readiness_baseline(&first_sample);
    assert_eq!(baseline, Some(5000));

    let stalled_catchup: Vec<_> = (0..4)
        .map(|i| make_status(i, 5000, 1, false, true))
        .collect();
    let a_stalled = assess_members_lag(&stalled_catchup, 20);
    assert!(!evaluate_network_readiness(
        &stalled_catchup,
        &a_stalled,
        424_242,
        3,
        baseline,
        10
    ));

    let grown_after_catchup: Vec<_> = (0..4)
        .map(|i| make_status(i, 5010, 1, false, true))
        .collect();
    let a_grown = assess_members_lag(&grown_after_catchup, 20);
    assert!(evaluate_network_readiness(
        &grown_after_catchup,
        &a_grown,
        424_242,
        3,
        baseline,
        10
    ));
}

// ---------------------------------------------------------------------------
// LocalExec Integration Tier: executes the REAL compiled `outbe-up` binary
// locally against a fake Unix-socket VMM server + local HTTP JSON-RPC server.
// ---------------------------------------------------------------------------

struct LocalShellExec {
    rpc_port: u16,
    xdg_state_home: PathBuf,
    fake_host_check_ready: Arc<AtomicBool>,
}

impl LocalShellExec {
    fn shell_cmd(&self, command: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command])
            .env("XDG_STATE_HOME", &self.xdg_state_home);
        cmd
    }
}

impl HostExec for LocalShellExec {
    fn capture(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
        let out = self.shell_cmd(command).output()?;
        ensure!(
            out.status.success(),
            "LocalShellExec capture failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    fn capture_raw(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
        // Intercept only `host status` and `host check` (which inspect root Linux bridge/nftables);
        // EVERY other `outbe-up` command (`version`, `image import`, `image list`, `disk prepare`,
        // `disk clean`, `vm create`, `vm status`, `vm start`, `vm stop`, `vm remove`, `vm logs`)
        // executes the REAL pinned `outbe-up-<sha16>` binary!
        if command.contains(" host status ") {
            return Ok(json!({"ready_for_vm": true, "notes": []}).to_string());
        }
        if command.contains(" host check ") {
            let vm_name = extract_flag_value(command, "--vm").unwrap_or_default();
            let ip = extract_flag_value(command, "--ip").unwrap_or_else(|| "10.77.0.10".to_owned());
            let ready = self.fake_host_check_ready.load(Ordering::SeqCst);
            let rc = if ready { 0 } else { 1 };
            let payload = json!({
                "vm": vm_name,
                "vm_id": "550e8400-e29b-41d4-a716-446655440001",
                "mac": "52:54:00:aa:bb:cc",
                "ip": ip,
                "ready": ready,
                "host_network_ok": ready,
                "service_ip_ok": ready,
                "published_ports_ok": ready,
                "missing": if ready { json!([]) } else { json!(["host NAT missing"]) },
                "conflicts": [],
            });
            return Ok(format!("{payload}\n{HOST_CHECK_EXIT_SENTINEL}{rc}\n"));
        }
        let out = self.shell_cmd(command).output()?;
        ensure!(
            out.status.success(),
            "LocalShellExec command failed ({command}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn capture_stdin(
        &self,
        _ssh: &Ssh,
        _label: &str,
        command: &str,
        stdin_bytes: &[u8],
    ) -> Result<String> {
        // Redirect guest bridge-IP curl RPC calls to our local fake HTTP JSON-RPC server.
        let rewritten = if command.contains("curl -fsS") && command.contains(":8545") {
            format!(
                "curl -fsS --connect-timeout 5 --max-time 30 -H 'content-type: application/json' --data-binary @- http://127.0.0.1:{}",
                self.rpc_port
            )
        } else {
            command.to_owned()
        };
        let mut child = self
            .shell_cmd(&rewritten)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(stdin_bytes)?;
        let out = child.wait_with_output()?;
        ensure!(
            out.status.success(),
            "LocalShellExec capture_stdin failed ({rewritten}):\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<()> {
        let status = self.shell_cmd(command).status()?;
        ensure!(status.success(), "LocalShellExec run failed: {command}");
        Ok(())
    }

    fn run_quiet(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<()> {
        let out = self.shell_cmd(command).output()?;
        ensure!(
            out.status.success(),
            "LocalShellExec run_quiet failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(())
    }

    fn copy_to(
        &self,
        _ssh: &Ssh,
        _label: &str,
        sources: &[&Path],
        destination: &str,
        _recursive: bool,
    ) -> Result<()> {
        let dst_dir = Path::new(destination);
        fs::create_dir_all(dst_dir)?;
        for src in sources {
            let fname = src.file_name().unwrap();
            fs::copy(src, dst_dir.join(fname))?;
        }
        Ok(())
    }
}

fn find_compiled_outbe_up_bin() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("outbe-up");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/outbe-up");
    fallback.is_file().then_some(fallback)
}

fn build_real_guest_image_archive(dir: &Path) -> PathBuf {
    let digest = "0123456789abcdef".repeat(4);
    let src = dir.join("img-src");
    fs::create_dir_all(&src).unwrap();
    let dfile = src.join("digest.txt");
    let mfile = src.join("metadata.json");
    fs::write(&dfile, format!("{digest}\n")).unwrap();
    fs::write(&mfile, "{}").unwrap();
    let dsha = sha256_file(&dfile).unwrap();
    let msha = sha256_file(&mfile).unwrap();
    let manifest = json!({
        "version": 1,
        "flavor": "dev",
        "os_image_hash": digest,
        "source_commit": "a".repeat(40),
        "dstack_rev": "b".repeat(40),
        "overlay_commit": "c".repeat(40),
        "files": [
            {"path": "digest.txt", "size": fs::metadata(&dfile).unwrap().len(), "sha256": dsha, "mode": 0o644},
            {"path": "metadata.json", "size": fs::metadata(&mfile).unwrap().len(), "sha256": msha, "mode": 0o644}
        ]
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let archive_path = dir.join("outbe-vm-dev-test.tar.zst");
    let out = fs::File::create(&archive_path).unwrap();
    let enc = zstd::Encoder::new(out, 1).unwrap();
    let mut builder = tar::Builder::new(enc);
    for (name, bytes) in [
        ("image-archive-manifest.json", manifest_bytes),
        ("digest.txt", fs::read(&dfile).unwrap()),
        ("metadata.json", fs::read(&mfile).unwrap()),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Regular);
        h.set_size(bytes.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        builder.append_data(&mut h, name, bytes.as_slice()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    archive_path
}

fn spawn_fake_vmm_unix_server(sock: PathBuf) -> (Arc<AtomicBool>, thread::JoinHandle<()>) {
    fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let vms: Arc<Mutex<BTreeMap<String, Value>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let seq = Arc::new(Mutex::new(1u32));
    let handle = thread::spawn(move || {
        while !stop_bg.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
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
                                for line in String::from_utf8_lossy(&raw[..pos]).lines() {
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
                    let Some(end) = header_end else { continue };
                    let target = String::from_utf8_lossy(&raw[..end])
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("")
                        .to_owned();
                    let method = target
                        .strip_prefix("/prpc/")
                        .and_then(|r| r.split('?').next())
                        .unwrap_or("");
                    let req: Value =
                        serde_json::from_slice(&raw[end..end + content_len]).unwrap_or(json!({}));
                    let mut g = vms.lock().unwrap();
                    let body = match method {
                        "Status" => {
                            let kw = req["keyword"].as_str().unwrap_or("");
                            let list: Vec<Value> = g
                                .values()
                                .filter(|v| {
                                    kw.is_empty() || v["name"].as_str().unwrap_or("").contains(kw)
                                })
                                .cloned()
                                .collect();
                            json!({"vms": list, "total": list.len()}).to_string()
                        }
                        "CreateVm" => {
                            let mut s = seq.lock().unwrap();
                            let id = format!("550e8400-e29b-41d4-a716-{:012}", *s);
                            let mac = format!("52:54:00:aa:bb:{:02x}", *s as u8);
                            *s += 1;
                            g.insert(
                                id.clone(),
                                json!({
                                    "id": id,
                                    "name": req["name"],
                                    "status": "stopped",
                                    "app_id": req["app_id"],
                                    "running": false,
                                    "uptime": "",
                                    "configuration": {
                                        "image": req["image"],
                                        "vcpu": req["vcpu"],
                                        "memory": req["memory"],
                                        "app_id": req["app_id"],
                                        "stopped": true,
                                        "annotations": req["annotations"],
                                    },
                                    "interfaces": [{"mode": "bridge", "mac": mac}]
                                }),
                            );
                            json!({"id": id}).to_string()
                        }
                        "GetInfo" => {
                            let id = req["id"].as_str().unwrap_or("");
                            match g.get(id) {
                                Some(v) => json!({"found": true, "info": v}).to_string(),
                                None => json!({"found": false, "info": null}).to_string(),
                            }
                        }
                        "StartVm" => {
                            let id = req["id"].as_str().unwrap();
                            if let Some(v) = g.get_mut(id) {
                                v["running"] = json!(true);
                                v["status"] = json!("running");
                            }
                            "{}".to_owned()
                        }
                        "ShutdownVm" | "StopVm" => {
                            let id = req["id"].as_str().unwrap();
                            if let Some(v) = g.get_mut(id) {
                                v["running"] = json!(false);
                                v["status"] = json!("stopped");
                            }
                            "{}".to_owned()
                        }
                        "RemoveVm" => {
                            let id = req["id"].as_str().unwrap();
                            g.remove(id);
                            "{}".to_owned()
                        }
                        _ => "{}".to_owned(),
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (stop, handle)
}

fn spawn_fake_rpc_http_server() -> (u16, Arc<AtomicBool>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let call_count = Arc::new(Mutex::new(0usize));
    let handle = thread::spawn(move || {
        while !stop_bg.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let mut cnt = call_count.lock().unwrap();
                    let poll = *cnt / 4;
                    *cnt += 1;
                    let height = match poll {
                        0 => 10_u64,
                        1 => 20_u64,
                        _ => 21_u64,
                    };
                    let body = json!([
                        {"jsonrpc": "2.0", "id": 1, "result": format!("0x{:x}", 424_242_u64)},
                        {"jsonrpc": "2.0", "id": 2, "result": {"number": format!("0x{height:x}"), "hash": "0x01"}},
                        {"jsonrpc": "2.0", "id": 3, "result": "0x3"},
                        {"jsonrpc": "2.0", "id": 4, "result": {
                            "connectedPeers": 3,
                            "hasThresholdShares": true,
                            "lastFinalizedBlock": height,
                            "vrfMaterialVersion": 1,
                            "nextPlannedActivationHeight": 1200,
                            "isValidator": true,
                            "mode": "signer",
                            "enclave": {"state": "ready", "ready": true, "offerKeyReady": true}
                        }}
                    ])
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    (port, stop, handle)
}

#[tokio::test]
async fn c2_local_exec_real_outbe_up_binary_full_lifecycle() {
    let Some(real_outbe_up) = find_compiled_outbe_up_bin() else {
        eprintln!("SKIPPED: compiled target/debug/outbe-up not found (build outbe-up first)");
        return;
    };
    let has_tools = ["sfdisk", "mke2fs", "debugfs", "qemu-img", "curl"]
        .into_iter()
        .all(|tool| {
            Command::new("sh")
                .args([
                    "-c",
                    &format!("command -v {tool} >/dev/null 2>&1 || [ -x /usr/sbin/{tool} ] || [ -x /sbin/{tool} ]"),
                ])
                .status()
                .is_ok_and(|s| s.success())
        });
    if !has_tools {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs/qemu-img/curl not all available");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let vmm_home = tmp.path().join("vmm");
    let xdg_state_home = tmp.path().join("xdg-state");
    let state_dir = xdg_state_home.join("outbe-up");
    let sock = vmm_home.join("run/vmm.sock");
    let (vmm_stop, vmm_handle) = spawn_fake_vmm_unix_server(sock);
    let (rpc_port, rpc_stop, rpc_handle) = spawn_fake_rpc_http_server();

    let outbe_up_bytes = fs::read(&real_outbe_up).unwrap();
    let fixture = build_test_vm_artifacts(
        tmp.path(),
        "localnet",
        vmm_home.to_str().unwrap(),
        &outbe_up_bytes,
    );

    let host_check_ready = Arc::new(AtomicBool::new(true));
    let exec: Arc<dyn HostExec> = Arc::new(LocalShellExec {
        rpc_port,
        xdg_state_home,
        fake_host_check_ready: Arc::clone(&host_check_ready),
    });

    // 1. Real deploy -> pins real `outbe-up-<sha16>`, streams real founder tar over stdin to
    //    `disk prepare`, creates 4 real VMs with `--owner-tag --disk-staging --consume-staging`.
    deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fixture.image_archive.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: false,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    // Verify pinned binary exists with 0700 and staging is empty after consume.
    let pinned = state_dir
        .join("bin")
        .join(format!("outbe-up-{}", &fixture.outbe_up_sha256[..16]));
    assert!(pinned.is_file());
    assert_eq!(
        fs::metadata(&pinned).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(!state_dir.join("staging/localnet-validator-0").exists());

    // 2. Real start (parallel `--expect-vm-id` + batched RPC growth +10 across 2 consecutive samples).
    start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    // 3. Real status + stop + remove -> state deleted cleanly.
    status_with_exec(
        &StatusOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            watch: false,
            max_lag: 20,
            json: true,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    stop_with_exec(
        &StopOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: false,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    remove_with_exec(
        &RemoveOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            force: false,
            timeout: Duration::from_secs(30),
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    assert!(!NetworkVmState::path(&fixture.root).exists());

    vmm_stop.store(true, Ordering::Relaxed);
    rpc_stop.store(true, Ordering::Relaxed);
    let _ = vmm_handle.join();
    let _ = rpc_handle.join();
}
