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
                "OUTBE_NODE_ROLE=\"founder\"\n",
                "OUTBE_EXTERNAL_IP=\"{ip}\"\n",
                "OUTBE_RPC_BIND_IP=\"{ip}\"\n",
                "OUTBE_VM_IP=\"{ip}\"\n",
                "OUTBE_VM_PREFIX=\"24\"\n",
                "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
                "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
                "OUTBE_RPC_PORT=\"8545\"\n",
                "OUTBE_CONSENSUS_PORT=\"30400\"\n",
                "OUTBE_RETH_P2P_PORT=\"30303\"\n",
                "OUTBE_RETH_DISCOVERY_PORT=\"31303\"\n",
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
    let peer_key = "a".repeat(96);
    write_mode(
        &joining_dir.join("operator-config.example.yaml"),
        format!(
            concat!(
                "version: 1\n",
                "network:\n",
                "  chain_id: {chain_id}\n",
                "  certified_rpc: \"http://10.77.0.10:8545\"\n",
                "  advertise_ip: 192.0.2.10\n",
                "  rpc_bind_ip: 127.0.0.1\n",
                "  vm_prefix: 24\n",
                "  vm_gateway: 10.77.0.1\n",
                "  vm_dns: [1.1.1.1, 8.8.8.8]\n",
                "  bootnodes:\n",
                "    - \"enode://abcd@10.77.0.10:30303\"\n",
                "  consensus_peers:\n",
                "    - \"{peer_key}@10.77.0.10:30400\"\n",
                "  use_local_defaults: true\n",
                "feeder:\n",
                "  public_toml: |\n",
                "    [oracle]\n",
                "    sources = \"local\"\n",
                "hyperlane:\n",
                "  public_url: \"http://192.0.2.10:9000\"\n",
                "  poll_interval_secs: 30\n",
                "  gasless: true\n",
                "  origins:\n",
                "    - domain: {chain_id}\n",
                "      rpc_urls: [\"http://127.0.0.1:8545\"]\n",
                "      rpc_consensus: single\n",
                "      reorg_period: 0\n",
            ),
            chain_id = chain_id,
            peer_key = peer_key,
        )
        .as_bytes(),
        0o644,
    );
    write_mode(
        &root.join("secrets/founder-keys/validator-0/evm-key.hex"),
        "2".repeat(64).as_bytes(),
        0o600,
    );

    let mut founders_yaml = String::new();
    let mut founders_manifest = Vec::new();
    for idx in 0..4usize {
        let ip = Ipv4Addr::new(10, 77, 0, 10 + idx as u8);
        let rel = format!("vm/validator-{idx}");
        build_founder_tree(&root.join(&rel), chain_id, ip);
        founders_yaml.push_str(&format!(
            "  - name: validator-{idx}\n    advertise_ip: {ip}\n    hyperlane_public_url: http://{ip}:9000\n    vm:\n      vm_ip: {ip}\n"
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
                "vm_ip": ip.to_string(),
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
    disk_prepare_roles: Vec<String>,
    disk_prepare_bundles: Vec<String>,
    host_check_commands: Vec<String>,
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
    full_node_ips: BTreeSet<String>,
    joiner_mode_override: Option<(String, String)>, // (service_ip, mode)
    joiner_has_shares: bool,
    joiner_enclave_ready: Option<bool>,
    vm_update_calls: Vec<(String, String, String, u64)>, // (vm_name, expect_vm_id, image_name, stop_timeout)
    fail_rpc_for_ip_after_update: Option<String>,
    drop_vm_update_response_once: Option<String>,
    rpc_dead_ips: BTreeSet<String>,
    rpc_dynamic_epoch_len: Option<u64>,
    rpc_next_activation_override: Option<u64>,
    rpc_randomness_preparing_until_height: Option<u64>,
    rpc_omit_randomness_status: bool,
    rpc_height_base: u64,
    rpc_height_step: u64,
    rpc_step_round: u64,
    rpc_batch_seen_since_last_status: bool,
    update_height_at_call: Vec<(String, u64)>,
    kill_ip_after_vm_update_rounds: Option<(String, usize, String)>,
    rounds_since_target_update: Option<usize>,
    rpc_connected_peers_override: BTreeMap<String, u64>,
    /// Optional per-call `missedVotes` schedule for validator-1 (`0x...0002`) returned by
    /// `IValidatorSet.validatorByAddress` `eth_call` batches (pops front or repeats last).
    rpc_participation_missed_schedule: Vec<u64>,
    /// If non-zero, validator-1's `missedVotes` increments by this amount on every participation call.
    rpc_participation_persistent_step: u64,
    rpc_participation_calls: usize,
    rpc_fail_participation: bool,
    certified_rpc_chain_id_override: Option<std::result::Result<u64, String>>,
    version_bins_queried: Vec<String>,
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

        if command.contains(":9003/status") {
            return Ok(json!({
                "healthy": true,
                "controller": "waiting_for_controller",
                "active": false,
            })
            .to_string());
        }

        if command.contains(" version --json") {
            let bin = extract_pinned_bin_name(command).unwrap_or_default();
            g.version_bins_queried.push(bin.clone());
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
            if g.rpc_height_step > 0 && g.rpc_batch_seen_since_last_status {
                g.rpc_step_round += 1;
                g.rpc_batch_seen_since_last_status = false;
            }
            if let Some(count) = g.rounds_since_target_update.as_mut() {
                *count += 1;
                let current = *count;
                if let Some((_, threshold, ip)) = g.kill_ip_after_vm_update_rounds.clone() {
                    if current >= threshold {
                        g.kill_ip_after_vm_update_rounds = None;
                        g.rounds_since_target_update = None;
                        g.rpc_dead_ips.insert(ip);
                    }
                }
            }
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

        if command.contains(" disk prepare ") && command.contains("--role full-node") {
            g.disk_prepare_calls += 1;
            g.disk_prepare_roles.push("full-node".to_owned());
            let bundle_arg = extract_flag_value(command, "--bundle")
                .expect("full-node disk prepare must pass --bundle");
            let _cfg_arg = extract_flag_value(command, "--config")
                .expect("full-node disk prepare must pass --config");
            assert!(
                !command.contains("--source") && !command.contains("--keys-from"),
                "full-node disk prepare must be keyless: {command}"
            );
            g.disk_prepare_bundles.push(bundle_arg);
            let staging_name = extract_flag_value(command, "--staging-name").unwrap();
            let sha = "f".repeat(64);
            g.staged_disks.insert(staging_name.clone(), sha.clone());
            return Ok(json!({
                "role": "full-node",
                "output": format!("/home/ubuntu/.local/state/outbe-up/staging/{staging_name}/data.img"),
                "staging_name": staging_name,
                "size_gib": 1,
                "sha256": sha,
            })
            .to_string());
        }

        if command.contains(" host check ") {
            g.host_check_commands.push(command.to_owned());
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

        if command.contains(" vm update ") {
            assert!(
                !command.contains("--force"),
                "vm update must never pass --force: {command}"
            );
            let name = extract_flag_value(command, "--name").unwrap();
            let expect = extract_flag_value(command, "--expect-vm-id")
                .ok_or_else(|| eyre::eyre!("vm update missing --expect-vm-id"))?;
            let target_image_name =
                extract_flag_value(command, "--image-name").unwrap_or_else(|| g.image_name.clone());
            let stop_timeout: u64 = extract_flag_value(command, "--stop-timeout")
                .unwrap_or_else(|| "120".to_owned())
                .parse()
                .unwrap();
            let target_hash = g.os_image_hash.clone();
            let cur_height = if g.rpc_height_step > 0 {
                g.rpc_height_base + g.rpc_step_round * g.rpc_height_step
            } else {
                let vm_count = g.vms.len().max(1);
                let poll_idx = g.rpc_poll_counter / vm_count;
                *g.rpc_height_schedule
                    .get(poll_idx)
                    .or_else(|| g.rpc_height_schedule.last())
                    .unwrap_or(&100)
            };
            let fail_ip = g.fail_rpc_for_ip_after_update.clone();
            let vm = g
                .vms
                .get_mut(&name)
                .ok_or_else(|| eyre::eyre!("vm update: `{name}` missing"))?;
            if vm.vm_id != expect {
                bail!(
                    "vm_id_mismatch for `{name}`: expected {expect}, got {}",
                    vm.vm_id
                );
            }
            let prev_img = vm.image_name.clone();
            let prev_hash = vm.os_image_hash.clone();
            let already = prev_img == target_image_name && prev_hash == target_hash && vm.running;
            vm.image_name = target_image_name.clone();
            vm.os_image_hash = target_hash.clone();
            vm.running = true;
            vm.vmm_status = "running".to_owned();
            let vm_id = vm.vm_id.clone();
            let vm_ip = vm.service_ip.map(|ip| ip.to_string());
            if fail_ip.is_some() && fail_ip == vm_ip {
                if let Some(ip) = vm_ip {
                    g.rpc_dead_ips.insert(ip);
                }
            }
            g.vm_update_calls.push((
                name.clone(),
                expect,
                target_image_name.clone(),
                stop_timeout,
            ));
            g.update_height_at_call.push((name.clone(), cur_height));
            if let Some((ref target_vm, _, _)) = g.kill_ip_after_vm_update_rounds {
                if target_vm == &name {
                    g.rounds_since_target_update = Some(0);
                }
            }
            if g.drop_vm_update_response_once.as_deref() == Some(name.as_str()) {
                g.drop_vm_update_response_once = None;
                bail!("simulated SSH drop after host vm update succeeded for `{name}`");
            }
            return Ok(json!({
                "name": name,
                "vm_id": vm_id,
                "previous_image_name": prev_img,
                "previous_os_image_hash": prev_hash,
                "image_name": target_image_name,
                "os_image_hash": target_hash,
                "image_reused": true,
                "already_updated": already,
                "vmm_status": "running",
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

        // 2. `disk prepare --role founder|joining-validator --source - --staging-name <vm>`.
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
            let role =
                extract_flag_value(command, "--role").unwrap_or_else(|| "founder".to_owned());
            g.disk_prepare_roles.push(role.clone());
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
                "role": role,
                "output": format!("/home/ubuntu/.local/state/outbe-up/staging/{staging_name}/data.img"),
                "staging_name": staging_name,
                "size_gib": 1,
                "sha256": sha,
                "key_verified": true,
            })
            .to_string());
        }

        // 3. Batched or single JSON-RPC via curl.
        if command.contains("curl -fsS") {
            let url = sh_tokens(command).last().cloned().unwrap_or_default();
            g.rpc_urls_hit.push(url.clone());
            if g.rpc_dead_ips.iter().any(|ip| url.contains(ip)) {
                bail!("simulated dead RPC on {url}");
            }
            let parsed: Value = serde_json::from_slice(stdin_bytes)?;
            if parsed.is_object() {
                let method = parsed.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "eth_chainId" {
                    let cid = match &g.certified_rpc_chain_id_override {
                        Some(Ok(cid)) => *cid,
                        Some(Err(msg)) => return Err(eyre::eyre!("{msg}")),
                        None => 424_242,
                    };
                    return Ok(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": format!("0x{cid:x}"),
                    })
                    .to_string());
                }
                if method == "eth_call" {
                    let addrs: Vec<alloy_primitives::Address> = g
                        .vms
                        .values()
                        .filter(|v| {
                            !v.service_ip
                                .is_some_and(|ip| g.full_node_ips.contains(&ip.to_string()))
                        })
                        .enumerate()
                        .map(|(i, _)| {
                            format!("0x{:040x}", i + 1)
                                .parse::<alloy_primitives::Address>()
                                .unwrap()
                        })
                        .collect();
                    let encoded =
                        outbe_operator_node_role::rpc::encode_active_validators_return(&addrs);
                    return Ok(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": format!("0x{}", hex::encode(encoded)),
                    })
                    .to_string());
                }
                let height_hex = parsed
                    .pointer("/params/0")
                    .and_then(Value::as_str)
                    .unwrap_or("0x64");
                return Ok(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "number": height_hex,
                        "hash": format!("0xhash-{height_hex}"),
                    }
                })
                .to_string());
            }
            let batch = parsed
                .as_array()
                .ok_or_else(|| eyre::eyre!("expected JSON-RPC batch array"))?;
            if batch
                .first()
                .and_then(|item| item.get("method"))
                .and_then(Value::as_str)
                == Some("eth_call")
            {
                let call_idx = g.rpc_participation_calls;
                g.rpc_participation_calls += 1;
                if g.rpc_fail_participation {
                    let err_items: Vec<Value> = batch
                        .iter()
                        .map(|item| {
                            json!({
                                "jsonrpc": "2.0",
                                "id": item["id"],
                                "error": { "code": -32000, "message": "simulated validatorByAddress eth_call error" }
                            })
                        })
                        .collect();
                    return Ok(Value::Array(err_items).to_string());
                }
                let val1_addr: alloy_primitives::Address =
                    format!("0x{:040x}", 2usize).parse().unwrap();
                let val1_missed = if g.rpc_participation_persistent_step > 0 {
                    (call_idx as u64) * g.rpc_participation_persistent_step
                } else {
                    g.rpc_participation_missed_schedule
                        .get(call_idx)
                        .or_else(|| g.rpc_participation_missed_schedule.last())
                        .copied()
                        .unwrap_or(0)
                };
                let resp_items: Vec<Value> = batch
                    .iter()
                    .map(|item| {
                        let data_hex = item
                            .pointer("/params/0/data")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let calldata = hex::decode(data_hex.strip_prefix("0x").unwrap_or(data_hex))
                            .expect("valid eth_call calldata hex");
                        let addr = outbe_operator_node_role::rpc::decode_validator_by_address_arg(
                            &calldata,
                        )
                        .expect("valid validatorByAddress calldata");
                        let missed = if addr == val1_addr { val1_missed } else { 0 };
                        let encoded =
                            outbe_operator_node_role::rpc::encode_validator_missed_votes_return(
                                addr, missed,
                            );
                        json!({
                            "jsonrpc": "2.0",
                            "id": item["id"],
                            "result": format!("0x{}", hex::encode(encoded)),
                        })
                    })
                    .collect();
                return Ok(Value::Array(resp_items).to_string());
            }
            assert_eq!(batch.len(), 4, "expected 4-call JSON-RPC batch");
            let vm_count = g.vms.values().filter(|v| v.running).count().max(1);
            let poll_idx = g.rpc_poll_counter / vm_count;
            g.rpc_poll_counter += 1;
            let height = if g.rpc_height_step > 0 {
                g.rpc_batch_seen_since_last_status = true;
                g.rpc_height_base + g.rpc_step_round * g.rpc_height_step
            } else {
                *g.rpc_height_schedule
                    .get(poll_idx)
                    .or_else(|| g.rpc_height_schedule.last())
                    .unwrap_or(&100)
            };
            let running_val_count = g
                .vms
                .values()
                .filter(|v| {
                    v.running
                        && !v
                            .service_ip
                            .is_some_and(|ip| g.full_node_ips.contains(&ip.to_string()))
                })
                .count();
            let peers = running_val_count.saturating_sub(1);
            let enclave_ready = g.rpc_enclave_ready;
            let (vrf_ver, computed_next_act, last_dkg_act) = match g.rpc_dynamic_epoch_len {
                Some(epoch_len) => (
                    height / epoch_len,
                    ((height / epoch_len) + 1) * epoch_len,
                    Some((height / epoch_len) * epoch_len),
                ),
                None => (g.rpc_vrf_version, 1200, Some(0)),
            };
            let next_activation = g.rpc_next_activation_override.unwrap_or(computed_next_act);
            let randomness_status = match g.rpc_randomness_preparing_until_height {
                Some(until) if height <= until => "preparing",
                _ => "healthy",
            };
            let is_full_node = g.full_node_ips.iter().any(|ip| url.contains(ip));
            let consensus_result = if is_full_node {
                json!({
                    "connectedPeers": 0,
                    "hasThresholdShares": false,
                    "lastFinalizedBlock": height,
                    "vrfMaterialVersion": 0,
                    "nextPlannedActivationHeight": 0,
                    "isValidator": false,
                    "mode": "follower",
                    "enclave": { "state": "disabled", "ready": false, "offerKeyReady": false }
                })
            } else {
                let (mode, shares, node_enclave_ready) = match &g.joiner_mode_override {
                    Some((ip, m)) if url.contains(ip) => (
                        m.as_str(),
                        g.joiner_has_shares,
                        g.joiner_enclave_ready.unwrap_or(enclave_ready),
                    ),
                    _ => ("signer", true, enclave_ready),
                };
                let consensus_peers = g
                    .rpc_connected_peers_override
                    .iter()
                    .find(|(ip, _)| url.contains(ip.as_str()))
                    .map(|(_, &p)| p)
                    .unwrap_or(peers as u64);
                let mut obj = json!({
                    "connectedPeers": consensus_peers,
                    "hasThresholdShares": shares,
                    "lastFinalizedBlock": height,
                    "vrfMaterialVersion": vrf_ver,
                    "lastDkgActivationHeight": last_dkg_act,
                    "nextPlannedActivationHeight": next_activation,
                    "isValidator": true,
                    "mode": mode,
                    "enclave": {
                        "state": if node_enclave_ready { "ready" } else { "booting" },
                        "ready": node_enclave_ready,
                        "offerKeyReady": node_enclave_ready,
                    }
                });
                if !g.rpc_omit_randomness_status {
                    obj["randomnessStatus"] = json!(randomness_status);
                }
                obj
            };
            let resp = json!([
                {"jsonrpc": "2.0", "id": 1, "result": format!("0x{:x}", 424_242_u64)},
                {"jsonrpc": "2.0", "id": 2, "result": {"number": format!("0x{height:x}"), "hash": format!("0xhash-0x{height:x}")}},
                {"jsonrpc": "2.0", "id": 3, "result": format!("0x{peers:x}")},
                {"jsonrpc": "2.0", "id": 4, "result": consensus_result}
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
        rpc_port: 8545,
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
            last_dkg_activation_height: None,
            next_planned_activation_height: 1200,
            randomness_status: None,
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
            finalized_hash: Some(format!("0x{:064x}", finalized)),
            execution_peers: Some(3),
            consensus_mode: Some("signer".to_owned()),
            consensus_peers: Some(3),
            has_threshold_shares: Some(true),
            vrf_material_version: Some(vrf),
            tee_ready: if no_tee { None } else { Some(tee_ok) },
            hyperlane_healthy: Some(true),
            hyperlane_controller: Some("waiting_for_controller".to_owned()),
            hyperlane_active: Some(false),
            consensus: Some(consensus),
            boot_warning: None,
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
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: Some(100),
            min_finalized_blocks: 10,
        },
    ));

    // Growth >= B + 10 with matching VRF and TEE ready -> true.
    let s_ok: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, false, true))
        .collect();
    let a_ok = assess_members_lag(&s_ok, 20);
    assert!(evaluate_network_readiness(
        &s_ok,
        &a_ok,
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: Some(100),
            min_finalized_blocks: 10,
        },
    ));

    // Divergent VRF version -> false.
    let mut s_vrf = s_ok.clone();
    s_vrf[3].vrf_material_version = Some(2);
    let a_vrf = assess_members_lag(&s_vrf, 20);
    assert!(!evaluate_network_readiness(
        &s_vrf,
        &a_vrf,
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: Some(100),
            min_finalized_blocks: 10,
        },
    ));

    // TEE not ready with no_tee=false -> false; with no_tee=true -> true!
    let s_notee_fail: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, false, false))
        .collect();
    let a_nf = assess_members_lag(&s_notee_fail, 20);
    assert!(!evaluate_network_readiness(
        &s_notee_fail,
        &a_nf,
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: Some(100),
            min_finalized_blocks: 10,
        },
    ));

    let s_notee_ok: Vec<_> = (0..4)
        .map(|i| make_status(i, 110, 1, true, false))
        .collect();
    let a_no = assess_members_lag(&s_notee_ok, 20);
    assert!(evaluate_network_readiness(
        &s_notee_ok,
        &a_no,
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: Some(100),
            min_finalized_blocks: 10,
        },
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
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: baseline,
            min_finalized_blocks: 10,
        },
    ));

    let grown_after_catchup: Vec<_> = (0..4)
        .map(|i| make_status(i, 5010, 1, false, true))
        .collect();
    let a_grown = assess_members_lag(&grown_after_catchup, 20);
    assert!(evaluate_network_readiness(
        &grown_after_catchup,
        &a_grown,
        ReadinessRequirements {
            chain_id: 424_242,
            required_peers: 3,
            baseline_b: baseline,
            min_finalized_blocks: 10,
        },
    ));
}

#[test]
fn host_check_port_specs_include_p2p_and_radicle_1to1_ports() {
    let make_member =
        |idx: usize, host: &str, pub_ip: Ipv4Addr, bridge_ip: Ipv4Addr, offset: u16| {
            DesiredMember {
                index: idx,
                role: MemberRole::Founder,
                name: format!("validator-{idx}"),
                vm_name: format!("s6r1-validator-{idx}"),
                owner_tag: format!("outbe-net:s6r1:01234567:validator-{idx}"),
                vmm_host: Ssh {
                    host: host.to_owned(),
                    user: "ubuntu".to_owned(),
                    port: 22,
                },
                vmm_home: "~/.dstack-vmm".to_owned(),
                bridge: "obr0".to_owned(),
                service_ip: bridge_ip,
                advertise_ip: pub_ip,
                rpc_port: 8545,
                port_offset: offset,
                vcpu: 3,
                memory_gib: 5,
                disk_gib: 10,
                flavor: VmFlavor::Dev,
                no_tee: false,
                image_name: None,
                image_hash: None,
                recorded_disk_sha256: None,
            }
        };
    let members = vec![
        make_member(
            0,
            "198.51.100.10",
            Ipv4Addr::new(198, 51, 100, 10),
            Ipv4Addr::new(10, 77, 0, 11),
            0,
        ),
        make_member(
            1,
            "198.51.100.10",
            Ipv4Addr::new(198, 51, 100, 10),
            Ipv4Addr::new(10, 77, 0, 12),
            1,
        ),
        make_member(
            2,
            "203.0.113.20",
            Ipv4Addr::new(203, 0, 113, 20),
            Ipv4Addr::new(10, 77, 0, 11),
            0,
        ),
        make_member(
            3,
            "203.0.113.20",
            Ipv4Addr::new(203, 0, 113, 20),
            Ipv4Addr::new(10, 77, 0, 12),
            1,
        ),
    ];
    let ctx = PublishContext::from_members(&members);
    assert_eq!(
        ctx.p2p_port_specs(&members[0]).unwrap(),
        vec![
            "30303/tcp",
            "30303/udp",
            "30400/tcp",
            "31303/udp",
            "8776/tcp",
        ]
    );
    assert_eq!(
        ctx.p2p_port_specs(&members[1]).unwrap(),
        vec![
            "30304/tcp",
            "30304/udp",
            "30401/tcp",
            "31304/udp",
            "8777/tcp",
        ]
    );
    assert_eq!(
        ctx.hyperlane_port_specs(&members[0]).unwrap(),
        vec!["9000/tcp"]
    );
    assert_eq!(
        ctx.hyperlane_port_specs(&members[1]).unwrap(),
        vec!["9000:9001/tcp"]
    );
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
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let req_text = String::from_utf8_lossy(&buf[..n]);
                    let req_body = req_text
                        .split_once("\r\n\r\n")
                        .map_or("", |(_, b)| b.trim());
                    let body = if req_body.starts_with('{') && req_body.contains("eth_chainId") {
                        json!({"jsonrpc": "2.0", "id": 1, "result": format!("0x{:x}", 424_242_u64)})
                            .to_string()
                    } else {
                        let mut cnt = call_count.lock().unwrap();
                        let poll = *cnt / 4;
                        *cnt += 1;
                        let height = match poll {
                            0 => 10_u64,
                            1 => 20_u64,
                            _ => 21_u64,
                        };
                        json!([
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
                        .to_string()
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

// ---------------------------------------------------------------------------
// S4 Tests: Joining Validator VM + Keyless Full Node VM + Mixed-Role Lifecycle
// ---------------------------------------------------------------------------

struct FakeJoinDriverState {
    min_stake: alloy_primitives::U256,
    joiner_addr: alloy_primitives::Address,
    balances: BTreeMap<alloy_primitives::Address, alloy_primitives::U256>,
    record: outbe_operator_node_role::rpc::ValidatorRecord,
    oracle_delegate: alloy_primitives::Address,
    staked_symmetric: Option<String>,
    confirm_ready_called: bool,
    funded_calls: Vec<(alloy_primitives::Address, alloy_primitives::U256)>,
    funded_before_vm_start: bool,
    host_state: Arc<Mutex<FakeHostState>>,
    joiner_ip: String,
    joiner_vm_name: String,
}

struct FakeJoinDriver {
    inner: Mutex<FakeJoinDriverState>,
}

impl JoinChainDriver for FakeJoinDriver {
    fn min_stake(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<alloy_primitives::U256>> + Send + '_>,
    > {
        let val = self.inner.lock().unwrap().min_stake;
        Box::pin(async move { Ok(val) })
    }

    fn balance(
        &self,
        addr: alloy_primitives::Address,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<alloy_primitives::U256>> + Send + '_>,
    > {
        let val = self
            .inner
            .lock()
            .unwrap()
            .balances
            .get(&addr)
            .copied()
            .unwrap_or_default();
        Box::pin(async move { Ok(val) })
    }

    fn validator(
        &self,
        _addr: alloy_primitives::Address,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<outbe_operator_node_role::rpc::ValidatorRecord>>
                + Send
                + '_,
        >,
    > {
        let rec = self.inner.lock().unwrap().record.clone();
        Box::pin(async move { Ok(rec) })
    }

    fn validator_delegate(
        &self,
        _validator: alloy_primitives::Address,
        _role: u8,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<alloy_primitives::Address>> + Send + '_>,
    > {
        let addr = self.inner.lock().unwrap().oracle_delegate;
        Box::pin(async move { Ok(addr) })
    }

    fn finalized_block(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<outbe_operator_node_role::rpc::Block>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            Ok(outbe_operator_node_role::rpc::Block {
                number: 100,
                hash: "0xhash-0x64".to_owned(),
            })
        })
    }

    fn fund_address(
        &self,
        _from_key: &Path,
        to: alloy_primitives::Address,
        amount: alloy_primitives::U256,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async move {
            let mut g = self.inner.lock().unwrap();
            let entry = g.balances.entry(to).or_default();
            *entry = entry.saturating_add(amount);
            g.funded_calls.push((to, amount));
            // F1: resident TEE (`registerEnclave`) requires a non-zero balance and becomes ready only
            // once funded; record whether funding occurred BEFORE the joiner VM was started.
            if to == g.joiner_addr && !g.balances[&to].is_zero() {
                let already_started = {
                    let mut h = g.host_state.lock().unwrap();
                    let started = h.vm_start_calls.iter().any(|(n, _)| n == &g.joiner_vm_name);
                    h.joiner_enclave_ready = Some(true);
                    started
                };
                g.funded_before_vm_start = !already_started;
            }
            Ok(format!("0x{}", "1".repeat(64)))
        })
    }

    fn stake(
        &self,
        _joiner_keys_dir: &Path,
        _validator: alloy_primitives::Address,
        _amount: alloy_primitives::U256,
        symmetric_endpoint: &str,
    ) -> Result<String> {
        let mut g = self.inner.lock().unwrap();
        g.staked_symmetric = Some(symmetric_endpoint.to_owned());
        g.record.registered = true;
        g.record.status = 1; // PENDING
        {
            let mut h = g.host_state.lock().unwrap();
            h.joiner_mode_override = Some((g.joiner_ip.clone(), "verifier".to_owned()));
            h.joiner_has_shares = false;
        }
        Ok(format!("0x{}", "2".repeat(64)))
    }

    fn delegate_oracle(
        &self,
        _joiner_evm_key: &Path,
        feeder: alloy_primitives::Address,
    ) -> Result<String> {
        self.inner.lock().unwrap().oracle_delegate = feeder;
        Ok(format!("0x{}", "3".repeat(64)))
    }

    fn confirm_ready(&self, _joiner_evm_key: &Path) -> Result<String> {
        let mut g = self.inner.lock().unwrap();
        g.confirm_ready_called = true;
        // Simulate DKG reshare completing -> ACTIVE + BLS share + signer mode.
        g.record.status = 2;
        g.record.has_bls_share = true;
        {
            let mut h = g.host_state.lock().unwrap();
            h.joiner_mode_override = Some((g.joiner_ip.clone(), "signer".to_owned()));
            h.joiner_has_shares = true;
            h.rpc_poll_counter = 0;
            h.rpc_height_schedule = vec![200, 210, 212, 215];
        }
        Ok(format!("0x{}", "4".repeat(64)))
    }

    fn wait_tx_finalized(
        &self,
        _hash: &str,
        _deadline: Instant,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move { Ok(()) })
    }
}

fn seed_pregenerated_member_keys(artifacts: &Path, member_name: &str) {
    let dir = artifacts.join("secrets/members").join(member_name);
    fs::create_dir_all(dir.join("radicle/keys")).unwrap();
    for (rel, byte) in [
        ("signing-key.hex", "a"),
        ("evm-key.hex", "b"),
        ("feeder-key.hex", "c"),
        ("hyperlane-key.hex", "d"),
        ("reth-p2p-secret.hex", "e"),
    ] {
        write_mode(&dir.join(rel), byte.repeat(64).as_bytes(), 0o600);
    }
    write_mode(
        &dir.join("radicle/keys/radicle"),
        b"fake-radicle-key",
        0o600,
    );
}

#[tokio::test]
async fn s4_add_validator_and_add_full_node_full_lifecycle_and_exit2_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(tmp.path(), "s4net", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

    // 1. Deploy and start the 4 founding VMs.
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

    // 2. Add joining validator: first with host_check_ready=false -> exit code 2 BEFORE vm start.
    //    F1: model hardware behavior where resident TEE stays not-ready until balance > 0!
    seed_pregenerated_member_keys(&fixture.root, "validator-joiner-0");
    let joiner_addr = outbe_operator_node_role::validator_address(
        &fixture
            .root
            .join("secrets/members/validator-joiner-0/evm-key.hex"),
    )
    .unwrap();
    {
        let mut g = fake.state.lock().unwrap();
        g.host_check_ready = false;
        g.host_check_commands.clear();
        g.joiner_mode_override = Some(("10.77.0.20".to_owned(), "upstreamBootstrap".to_owned()));
        g.joiner_has_shares = false;
        g.joiner_enclave_ready = Some(false);
        g.rpc_poll_counter = 0;
        g.rpc_height_schedule = vec![100, 100, 100];
    }
    let min_stake = alloy_primitives::U256::from(10_000_000_000_000_000_000_u128);
    let driver = FakeJoinDriver {
        inner: Mutex::new(FakeJoinDriverState {
            min_stake,
            joiner_addr,
            balances: BTreeMap::new(),
            record: outbe_operator_node_role::rpc::ValidatorRecord::default(),
            oracle_delegate: alloy_primitives::Address::ZERO,
            staked_symmetric: None,
            confirm_ready_called: false,
            funded_calls: Vec::new(),
            funded_before_vm_start: false,
            host_state: Arc::clone(&fake.state),
            joiner_ip: "10.77.0.20".to_owned(),
            joiner_vm_name: "s4net-validator-joiner-0".to_owned(),
        }),
    };
    let join_opts = AddValidatorOptions {
        artifacts: &fixture.root,
        name: "validator-joiner-0",
        host: "192.0.2.50",
        user: None,
        port: 22,
        vmm_home: None,
        bridge: None,
        vm_ip: Ipv4Addr::new(10, 77, 0, 20),
        rpc_bind_ip: None,
        advertise_ip: Ipv4Addr::new(198, 51, 100, 20),
        rpc_port: 8545,
        port_offset: 2,
        image_source: None,
        vcpu: None,
        memory_gib: None,
        disk_gib: None,
        no_tee: false,
        ssh_key: None,
        ssh_config: None,
        timeout: Duration::from_secs(30),
        max_lag: 20,
        min_finalized_blocks: 10,
    };
    let err = add_validator_with_exec(&join_opts, Arc::clone(&exec), &driver)
        .await
        .expect_err("failed host check must exit with code 2 before starting joiner");
    let exit_err = err
        .downcast_ref::<ExitCodeError>()
        .unwrap_or_else(|| panic!("expected ExitCodeError, got: {err:#}"));
    assert_eq!(exit_err.code, EXIT_CODE_HOST_CONFIG_REQUIRED);
    {
        let g = fake.state.lock().unwrap();
        assert!(g
            .disk_prepare_roles
            .contains(&"joining-validator".to_owned()));
        let check_cmd = g.host_check_commands[..g.host_check_commands.len() - 1].join("\n");
        for port in [
            "30305/tcp",
            "30305/udp",
            "30402/tcp",
            "31305/udp",
            "8778/tcp",
        ] {
            assert!(
                check_cmd.contains(port),
                "joiner host check must verify 1:1 port {port}: {check_cmd}"
            );
        }
        let hyperlane_check = g.host_check_commands.last().unwrap();
        assert!(
            hyperlane_check.contains("--port '9000:9002/tcp'")
                || hyperlane_check.contains("--port 9000:9002/tcp"),
            "joiner host check must verify Hyperlane 9002 -> VM:9000: {hyperlane_check}"
        );
        assert!(
            !hyperlane_check.contains("--source"),
            "Hyperlane host check must accept the firewall's dedicated infra source: {hyperlane_check}"
        );
        assert!(
            !g.vm_start_calls
                .iter()
                .any(|(n, _)| n == "s4net-validator-joiner-0"),
            "joiner VM must NOT be started when host check fails"
        );
    }

    // 3. Re-run add-validator with host_check_ready=true -> reuses VM, funds joiner BEFORE vm start
    //    (unblocking resident TEE), then drives stake -> confirm-ready -> ACTIVE lockstep!
    fake.state.lock().unwrap().host_check_ready = true;
    add_validator_with_exec(&join_opts, Arc::clone(&exec), &driver)
        .await
        .unwrap();
    {
        let d = driver.inner.lock().unwrap();
        assert!(
            d.funded_before_vm_start,
            "F1: joiner must be funded BEFORE vm start so registerEnclave has gas"
        );
        let expected_min_funding = min_stake
            + alloy_primitives::U256::from(outbe_operator_node_role::FEEDER_FUNDING_WEI)
            + alloy_primitives::U256::from(1_000_000_000_000_000_000_u128)
            + alloy_primitives::U256::from(2_000_000_000_000_000_000_u128);
        assert_eq!(
            d.funded_calls[0],
            (joiner_addr, expected_min_funding),
            "F7: pre-start funding must cover min_stake + feeder + Hyperlane + gas reserve"
        );
        assert_eq!(d.staked_symmetric.as_deref(), Some("198.51.100.20:30402"));
        assert!(d.confirm_ready_called);
        assert_eq!(d.record.status, 2);
        assert!(d.record.has_bls_share);
        assert_eq!(d.funded_calls.len(), 2, "must fund joiner + oracle feeder");
    }

    // 4. Add keyless full-node VM -> zero keys, zero inbound --port in host check, tracks finalized hashes.
    {
        let mut g = fake.state.lock().unwrap();
        g.full_node_ips.insert("10.77.0.30".to_owned());
        g.host_check_commands.clear();
        g.rpc_poll_counter = 0;
        g.rpc_height_schedule = vec![300, 310, 312, 315];
    }
    add_full_node_with_exec(
        &AddFullNodeOptions {
            artifacts: &fixture.root,
            name: "fullnode-0",
            host: "192.0.2.50",
            user: None,
            port: 22,
            vmm_home: None,
            bridge: None,
            vm_ip: Ipv4Addr::new(10, 77, 0, 30),
            rpc_bind_ip: None,
            advertise_ip: None,
            rpc_port: 8545,
            image_source: None,
            vcpu: None,
            memory_gib: None,
            disk_gib: None,
            no_tee: false,
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
    assert!(
        !fixture.root.join("secrets/members/fullnode-0").exists(),
        "full-node must never generate or store keys"
    );
    {
        let g = fake.state.lock().unwrap();
        assert!(g.disk_prepare_roles.contains(&"full-node".to_owned()));
        let check_cmd = g.host_check_commands.last().unwrap();
        assert!(
            !check_cmd.contains(" --port "),
            "full-node host check must be outbound-only with ZERO --port args: {check_cmd}"
        );
    }

    // 5. Verify network-vm-state.json records all 6 members across all 3 roles, and `status`, `stop`, `remove` handle all 6!
    {
        let lock = NetworkVmState::lock(&fixture.root).unwrap();
        let st = NetworkVmState::load_optional_locked(&lock, &fixture.root)
            .unwrap()
            .unwrap();
        assert_eq!(st.members.len(), 6);
        assert_eq!(
            st.member("s4net-validator-joiner-0").unwrap().role,
            MemberRole::JoiningValidator
        );
        assert_eq!(
            st.member("s4net-fullnode-0").unwrap().role,
            MemberRole::FullNode
        );
    }

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
    assert!(fake.state.lock().unwrap().vms.is_empty());
    assert!(!NetworkVmState::path(&fixture.root).exists());
}

#[test]
fn s4_partial_member_keys_refused_without_deleting_existing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let artifacts = tmp.path().join("artifacts");
    let member_dir = artifacts.join("secrets/members/validator-joiner-partial");
    fs::create_dir_all(&member_dir).unwrap();
    let evm_key = member_dir.join("evm-key.hex");
    let signing_key = member_dir.join("signing-key.hex");
    write_mode(&evm_key, "b".repeat(64).as_bytes(), 0o600);
    write_mode(&signing_key, "a".repeat(64).as_bytes(), 0o600);

    let err = ensure_local_member_keys(&artifacts, "validator-joiner-partial", 424_242)
        .expect_err("partial member keys dir must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("partial member keys directory")
            && msg.contains("feeder-key.hex")
            && msg.contains("radicle/keys/radicle"),
        "error must explain refusal and name missing key files: {msg}"
    );
    // Existing keys MUST NOT have been deleted or modified.
    assert_eq!(fs::read_to_string(&evm_key).unwrap(), "b".repeat(64));
    assert_eq!(fs::read_to_string(&signing_key).unwrap(), "a".repeat(64));
}

#[test]
fn s4_boot_error_warning_vs_live_unhealthy() {
    struct StaticRpcExec {
        rpc_response: Option<String>,
    }
    impl HostExec for StaticRpcExec {
        fn run(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
            unreachable!()
        }
        fn run_quiet(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<()> {
            unreachable!()
        }
        fn capture(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<String> {
            unreachable!()
        }
        fn capture_raw(&self, _ssh: &Ssh, _label: &str, command: &str) -> Result<String> {
            if command.contains(":9003/status") {
                return Ok(json!({
                    "healthy": true,
                    "controller": "waiting_for_controller",
                    "active": false,
                })
                .to_string());
            }
            unreachable!()
        }
        fn capture_stdin(
            &self,
            _ssh: &Ssh,
            _label: &str,
            _command: &str,
            _stdin: &[u8],
        ) -> Result<String> {
            self.rpc_response
                .clone()
                .ok_or_else(|| eyre::eyre!("connection refused"))
        }
        fn copy_to(
            &self,
            _ssh: &Ssh,
            _label: &str,
            _sources: &[&Path],
            _destination: &str,
            _recursive: bool,
        ) -> Result<()> {
            unreachable!()
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(tmp.path(), "devnet", "~/.dstack-vmm", b"fake-outbe-up");
    let build = NetworkArtifacts::load(&fixture.root).unwrap();
    let member = DesiredMember {
        index: 1,
        role: MemberRole::Founder,
        name: "validator-1".to_owned(),
        vm_name: "devnet-validator-1".to_owned(),
        owner_tag: "outbe-net:devnet:01234567:validator-1".to_owned(),
        vmm_host: Ssh {
            host: "192.0.2.50".to_owned(),
            user: "ubuntu".to_owned(),
            port: 22,
        },
        vmm_home: "~/.dstack-vmm".to_owned(),
        bridge: "obr0".to_owned(),
        service_ip: Ipv4Addr::new(10, 77, 0, 12),
        advertise_ip: Ipv4Addr::new(192, 0, 2, 50),
        rpc_port: 8545,
        port_offset: 1,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 10,
        flavor: VmFlavor::Dev,
        no_tee: false,
        image_name: None,
        image_hash: None,
        recorded_disk_sha256: None,
    };
    let observed_running = RemoteVmStatusOutput {
        name: member.vm_name.clone(),
        vm_id: "550e8400-e29b-41d4-a716-446655440012".to_owned(),
        owner_tag: Some(member.owner_tag.clone()),
        phase: "ready".to_owned(),
        vmm_status: "running".to_owned(),
        running: true,
        uptime: "18m".to_owned(),
        mac: "7e:64:73:fe:93:be".to_owned(),
        ip: Some(member.service_ip),
        service_ip: Some(member.service_ip),
        dhcp_ip: None,
        intended_ip: Some(member.service_ip),
        image_name: "outbe-vm-dev-93721673e3cd9a46".to_owned(),
        os_image_hash: "1".repeat(64),
        disk_source_sha256: Some("a".repeat(64)),
        app_id: "c".repeat(40),
        no_tee: false,
        vcpu: 3,
        memory_gib: 5,
        disk_gib: 10,
        bridge: "obr0".to_owned(),
        boot_error: Some("dstack-guest-agent.service failed".to_owned()),
        error: None,
    };

    let healthy_rpc = json!([
        {"jsonrpc": "2.0", "id": 1, "result": format!("0x{:x}", build.config.chain_id)},
        {"jsonrpc": "2.0", "id": 2, "result": {"number": "0xe94", "hash": "0xf03ee63eb69aa22203420b9bac433e92a8c95b28fe96178e1556b4caf1ceea0d"}},
        {"jsonrpc": "2.0", "id": 3, "result": "0x4"},
        {"jsonrpc": "2.0", "id": 4, "result": {
            "connectedPeers": 4,
            "hasThresholdShares": true,
            "lastFinalizedBlock": 3732,
            "vrfMaterialVersion": 12,
            "nextPlannedActivationHeight": 4000,
            "isValidator": true,
            "mode": "signer",
            "enclave": {"state": "ready", "ready": true, "offerKeyReady": true}
        }}
    ])
    .to_string();

    // Case 1: boot_error present, live checks healthy -> ready with warning in DETAIL & JSON.
    let healthy_exec = StaticRpcExec {
        rpc_response: Some(healthy_rpc),
    };
    let status_ok =
        build_member_status(member.clone(), Some(&observed_running), &healthy_exec, true);
    assert!(status_ok.error.is_none());
    assert_eq!(
        status_ok.boot_warning.as_deref(),
        Some("dstack-guest-agent.service failed")
    );
    assert!(member_node_ready(&status_ok, build.config.chain_id, 4));
    let assessments_ok = assess_members_lag(std::slice::from_ref(&status_ok), 20);
    assert!(
        check_unhealthy_members(std::slice::from_ref(&status_ok), &assessments_ok, &[], 4,).is_ok()
    );
    let rendered = render_vm_status(&build, std::slice::from_ref(&status_ok), &[], 20);
    assert!(
        rendered.contains("ok (warn: dstack-guest-agent.service failed)"),
        "DETAIL must surface boot_error warning on healthy VM: {rendered}"
    );
    let json_val = serde_json::to_value(&status_ok).unwrap();
    assert_eq!(
        json_val["boot_warning"],
        json!("dstack-guest-agent.service failed")
    );
    assert!(json_val["error"].is_null());

    // Case 2a: boot_error present + live RPC unreachable -> not ready, boot_error surfaced in error.
    let dead_rpc_exec = StaticRpcExec { rpc_response: None };
    let status_rpc_dead = build_member_status(
        member.clone(),
        Some(&observed_running),
        &dead_rpc_exec,
        true,
    );
    assert!(!member_node_ready(
        &status_rpc_dead,
        build.config.chain_id,
        4
    ));
    let rpc_err = status_rpc_dead.error.as_deref().unwrap_or("");
    assert!(
        rpc_err.contains("RPC unreachable")
            && rpc_err.contains("dstack-guest-agent.service failed"),
        "RPC failure must surface boot_error in error: {rpc_err}"
    );
    let assessments_dead = assess_members_lag(std::slice::from_ref(&status_rpc_dead), 20);
    assert!(check_unhealthy_members(
        std::slice::from_ref(&status_rpc_dead),
        &assessments_dead,
        &[],
        4,
    )
    .is_err());

    // Case 2b: boot_error present + VM not running -> not ready, boot_error surfaced in error.
    let mut observed_stopped = observed_running;
    observed_stopped.running = false;
    observed_stopped.vmm_status = "stopped".to_owned();
    let status_stopped = build_member_status(member, Some(&observed_stopped), &dead_rpc_exec, true);
    assert!(!member_node_ready(
        &status_stopped,
        build.config.chain_id,
        4
    ));
    assert_eq!(
        status_stopped.error.as_deref(),
        Some("dstack-guest-agent.service failed")
    );
}

#[tokio::test]
async fn s7_rolling_update_one_by_one_dkg_guard_software_bundle_and_boundary_crossing() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        build_test_vm_artifacts(dir.path(), "s7net", "~/.dstack-vmm", b"fake-outbe-up-v1");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

    // 1. Deploy and start 4 founders + 1 keyless full node on image v1.
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
        let mut g = fake.state.lock().unwrap();
        g.full_node_ips.insert("10.77.0.30".to_owned());
        g.rpc_poll_counter = 0;
        g.rpc_height_schedule = vec![200, 210, 211, 212];
    }
    add_full_node_with_exec(
        &AddFullNodeOptions {
            artifacts: &fixture.root,
            name: "fullnode-0",
            host: "192.0.2.50",
            user: Some("ubuntu"),
            port: 22,
            vmm_home: Some("~/.dstack-vmm"),
            bridge: Some("obr0"),
            vm_ip: Ipv4Addr::new(10, 77, 0, 30),
            rpc_bind_ip: None,
            advertise_ip: None,
            rpc_port: 8545,
            image_source: None,
            vcpu: None,
            memory_gib: None,
            disk_gib: None,
            no_tee: false,
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

    // Record pre-update VM UUIDs and MACs to verify every VM keeps its identity.
    let pre_identities: BTreeMap<String, (String, String)> = {
        let g = fake.state.lock().unwrap();
        g.vms
            .iter()
            .map(|(k, v)| (k.clone(), (v.vm_id.clone(), v.mac.clone())))
            .collect()
    };
    assert_eq!(pre_identities.len(), 5);

    // 2. Prepare image v2 archive + newer override software bundle (different outbe-up SHA-256).
    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-image-v2-bytes").unwrap();
    let v2_image_hash = "2222222222222222".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    let v2_bundle = dir.path().join("software-bundle-v2.tar.zst");
    let v2_outbe_up_sha = build_software_bundle_with_outbe_up(&v2_bundle, b"fake-outbe-up-v2");
    assert_ne!(v2_outbe_up_sha, fixture.outbe_up_sha256);

    // Start near a 300-block DKG boundary (height=1185, next_activation=1200, guard=30):
    // network update MUST wait past 1200 before stopping validator-0, then roll all 5 VMs one by
    // one, then wait until validators cross the next 300-block DKG boundary in lockstep.
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 1185;
        g.rpc_height_step = 15;
        g.rpc_dynamic_epoch_len = Some(300);
        g.vm_stop_calls.clear();
    }

    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: Some(&v2_bundle),
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(90),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    {
        let g = fake.state.lock().unwrap();
        // Override outbe-up v2 was installed on host.
        assert!(g.installed_bins.values().any(|sha| sha == &v2_outbe_up_sha));
        // Zero `vm stop` / forced calls.
        assert!(g.vm_stop_calls.is_empty());
        // Exactly 5 VMs updated one-by-one in deterministic order with stop_timeout=90.
        let updated_names: Vec<&str> = g
            .vm_update_calls
            .iter()
            .map(|(n, _, _, _)| n.as_str())
            .collect();
        assert_eq!(
            updated_names,
            vec![
                "s7net-validator-0",
                "s7net-validator-1",
                "s7net-validator-2",
                "s7net-validator-3",
                "s7net-fullnode-0",
            ]
        );
        for (_, _, img, stop_to) in &g.vm_update_calls {
            assert_eq!(img, &v2_image_name);
            assert_eq!(*stop_to, 90);
        }
        // DKG guard deferred validator-0 update until finalized > 1200!
        assert!(
            g.update_height_at_call[0].1 > 1200,
            "validator-0 must not be stopped inside DKG guard window: {:?}",
            g.update_height_at_call
        );
        // Every VM preserved its exact vm_id & MAC and now runs image v2.
        for (vm_name, (orig_id, orig_mac)) in &pre_identities {
            let vm = &g.vms[vm_name];
            assert_eq!(&vm.vm_id, orig_id);
            assert_eq!(&vm.mac, orig_mac);
            assert_eq!(vm.image_name, v2_image_name);
            assert_eq!(vm.os_image_hash, v2_image_hash);
            assert!(vm.running);
        }
    }

    // State persisted top-level, per-member v2 image hash, AND newly pinned `outbe_up_sha256`.
    {
        let lock = NetworkVmState::lock(&fixture.root).unwrap();
        let st = NetworkVmState::load_optional_locked(&lock, &fixture.root)
            .unwrap()
            .unwrap();
        assert_eq!(st.image_name, v2_image_name);
        assert_eq!(st.image_hash, v2_image_hash);
        assert_eq!(st.outbe_up_sha256, v2_outbe_up_sha);
        assert_eq!(st.members.len(), 5);
        for m in &st.members {
            assert_eq!(m.image_hash, v2_image_hash);
        }
    }

    // Subsequent `status` uses the newly recorded `v2_outbe_up_sha` pinned binary (NOT v1).
    {
        fake.state.lock().unwrap().version_bins_queried.clear();
    }
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
    {
        let g = fake.state.lock().unwrap();
        let expected_v2_bin = format!("outbe-up-{}", &v2_outbe_up_sha[..16]);
        assert!(
            g.version_bins_queried.iter().all(|b| b == &expected_v2_bin)
                && !g.version_bins_queried.is_empty(),
            "status after --software-bundle update must use {expected_v2_bin}, got {:?}",
            g.version_bins_queried
        );
    }

    // 3. Idempotent re-run performs ZERO additional `vm update` calls.
    {
        let mut g = fake.state.lock().unwrap();
        g.vm_update_calls.clear();
    }
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: Some(&v2_bundle),
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(90),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());
}

#[tokio::test]
async fn s7_rolling_update_aborts_on_member_failure_and_resumes_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7res", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-resume-v2-bytes").unwrap();
    let v2_image_hash = "3333333333333333".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // Inject dead RPC on validator-1 (`10.77.0.11`) as soon as validator-1 is updated.
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
        g.fail_rpc_for_ip_after_update = Some("10.77.0.11".to_owned());
    }

    let err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_millis(250),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("rollout must abort when validator-1 fails recovery");
    assert!(
        format!("{err:#}").contains("stopping rollout before remaining VMs"),
        "{err:#}"
    );

    // validator-2 and validator-3 MUST NOT have been touched yet!
    {
        let g = fake.state.lock().unwrap();
        let called: Vec<&str> = g
            .vm_update_calls
            .iter()
            .map(|(n, _, _, _)| n.as_str())
            .collect();
        assert_eq!(called, vec!["s7res-validator-0", "s7res-validator-1"]);
        assert_eq!(g.vms["s7res-validator-0"].os_image_hash, v2_image_hash);
        assert_eq!(g.vms["s7res-validator-1"].os_image_hash, v2_image_hash);
        assert_ne!(g.vms["s7res-validator-2"].os_image_hash, v2_image_hash);
        assert_ne!(g.vms["s7res-validator-3"].os_image_hash, v2_image_hash);
    }

    // Clear the RPC failure on validator-1 and resume `network update`!
    {
        let mut g = fake.state.lock().unwrap();
        g.fail_rpc_for_ip_after_update = None;
        g.rpc_dead_ips.clear();
        g.vm_update_calls.clear();
    }
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    // Resume MUST have skipped validator-0 & validator-1 and updated ONLY validator-2 & validator-3!
    {
        let g = fake.state.lock().unwrap();
        let resumed: Vec<&str> = g
            .vm_update_calls
            .iter()
            .map(|(n, _, _, _)| n.as_str())
            .collect();
        assert_eq!(resumed, vec!["s7res-validator-2", "s7res-validator-3"]);
        for vm in g.vms.values() {
            assert_eq!(vm.os_image_hash, v2_image_hash);
        }
    }
}

#[tokio::test]
async fn s7_f1_scenario_a_resumes_after_lost_vm_update_response() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7fa", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-f1a-v2-bytes").unwrap();
    let v2_image_hash = "4444444444444444".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // Simulate SSH drop right AFTER `outbe-up vm update` succeeded on host for validator-1:
    // host VM is running v2, but `network-vm-state.json` still records v1 for validator-1.
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
        g.drop_vm_update_response_once = Some("s7fa-validator-1".to_owned());
    }

    let err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("simulated SSH drop must abort first run");
    assert!(format!("{err:#}").contains("simulated SSH drop"), "{err:#}");

    // Confirm Scenario A state: host validator-1 is on v2, state still records v1!
    {
        let g = fake.state.lock().unwrap();
        assert_eq!(g.vms["s7fa-validator-1"].os_image_hash, v2_image_hash);
        let lock = NetworkVmState::lock(&fixture.root).unwrap();
        let st = NetworkVmState::load_optional_locked(&lock, &fixture.root)
            .unwrap()
            .unwrap();
        assert_eq!(
            st.member("s7fa-validator-0").unwrap().image_hash,
            v2_image_hash
        );
        assert_ne!(
            st.member("s7fa-validator-1").unwrap().image_hash,
            v2_image_hash
        );
    }

    // Resume MUST NOT reject validator-1 as Stale; must sync state and roll ONLY validator-2 & 3!
    fake.state.lock().unwrap().vm_update_calls.clear();
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    let g = fake.state.lock().unwrap();
    let resumed: Vec<&str> = g
        .vm_update_calls
        .iter()
        .map(|(n, _, _, _)| n.as_str())
        .collect();
    assert_eq!(resumed, vec!["s7fa-validator-2", "s7fa-validator-3"]);
    let lock = NetworkVmState::lock(&fixture.root).unwrap();
    let st = NetworkVmState::load_optional_locked(&lock, &fixture.root)
        .unwrap()
        .unwrap();
    for m in &st.members {
        assert_eq!(m.image_hash, v2_image_hash);
    }
}

#[tokio::test]
async fn s7_f1_scenario_b_resumes_single_stopped_vm_and_refuses_two_unhealthy() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7fb", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-f1b-v2-bytes").unwrap();
    let v2_image_hash = "5555555555555555".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // Case 1: TWO VMs stopped/unhealthy -> MUST refuse with ZERO vm update calls.
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
        for name in ["s7fb-validator-1", "s7fb-validator-2"] {
            let vm = g.vms.get_mut(name).unwrap();
            vm.running = false;
            vm.vmm_status = "exited".to_owned();
        }
    }
    let two_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("must refuse when 2+ members are unhealthy");
    assert!(
        format!("{two_err:#}").contains("unhealthy member(s)"),
        "{two_err:#}"
    );
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());

    // Case 2: Scenario B — exactly ONE VM (`validator-1`) is stopped on old image (e.g. after
    // graceful stop timeout), while the other 3 validators are healthy at N-1 peers.
    // Resume MUST finish `validator-1` FIRST, recover to full lockstep, then roll the remaining 3!
    {
        let mut g = fake.state.lock().unwrap();
        let vm2 = g.vms.get_mut("s7fb-validator-2").unwrap();
        vm2.running = true;
        vm2.vmm_status = "running".to_owned();
    }
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    let g = fake.state.lock().unwrap();
    let order: Vec<&str> = g
        .vm_update_calls
        .iter()
        .map(|(n, _, _, _)| n.as_str())
        .collect();
    assert_eq!(
        order,
        vec![
            "s7fb-validator-1",
            "s7fb-validator-0",
            "s7fb-validator-2",
            "s7fb-validator-3",
        ]
    );
    for vm in g.vms.values() {
        assert_eq!(vm.os_image_hash, v2_image_hash);
        assert!(vm.running);
    }
}

#[tokio::test]
async fn s7_f2_dkg_guard_time_based_ceremony_window_and_fail_closed() {
    // 1. Verify time-based effective guard calculation & floor.
    assert_eq!(
        compute_effective_dkg_guard_blocks(Duration::from_secs(120), 30, 4.0),
        735
    );
    assert_eq!(
        compute_effective_dkg_guard_blocks(Duration::from_secs(10), 500, 1.0),
        500
    );
    assert_eq!(
        compute_effective_dkg_guard_blocks(Duration::from_secs(120), 30, 0.0),
        195
    );

    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7f2", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-f2-v2-bytes").unwrap();
    let v2_image_hash = "6666666666666666".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // 2. Refuse `--dkg-guard-blocks 0`.
    let zero_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 0,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("dkg_guard_blocks=0 must be refused");
    assert!(
        format!("{zero_err:#}").contains("--dkg-guard-blocks 0"),
        "{zero_err:#}"
    );

    // 3. Fail closed when `next_planned_activation_height == 0` (must NOT stop any VM).
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_next_activation_override = Some(0);
    }
    let fail_closed_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("next_planned_activation_height==0 must fail closed");
    assert!(
        format!("{fail_closed_err:#}").contains("next_planned_activation_height"),
        "{fail_closed_err:#}"
    );
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());

    // 3b. Missing `randomnessStatus` in `outbe_consensusStatus` must be treated as NOT healthy
    //     (fail closed without stopping any VM).
    {
        let mk_val = |rs: Option<&str>| VmMemberStatus {
            name: "validator-0".to_owned(),
            role: MemberRole::Founder,
            vm_name: "s7f2-validator-0".to_owned(),
            vm_id: "id-0".to_owned(),
            host: "192.0.2.50".to_owned(),
            classification: VmClassKind::Ours,
            classification_detail: None,
            service_ip: Ipv4Addr::new(10, 77, 0, 10),
            dhcp_ip: None,
            mac: "52:54:00:00:00:01".to_owned(),
            vmm_status: "running".to_owned(),
            vmm_running: true,
            no_tee: false,
            chain_id: Some(424_242),
            finalized: Some(500),
            finalized_hash: Some("0xabc".to_owned()),
            execution_peers: Some(3),
            consensus_mode: Some("signer".to_owned()),
            consensus_peers: Some(3),
            has_threshold_shares: Some(true),
            vrf_material_version: Some(1),
            tee_ready: Some(true),
            hyperlane_healthy: Some(true),
            hyperlane_controller: Some("waiting_for_controller".to_owned()),
            hyperlane_active: Some(false),
            consensus: Some(ConsensusStatus {
                randomness_status: rs.map(ToOwned::to_owned),
                ..Default::default()
            }),
            boot_warning: None,
            error: None,
        };
        assert!(!all_validators_randomness_healthy(&[mk_val(None)]));
        assert!(!all_validators_randomness_healthy(&[mk_val(Some(
            "preparing"
        ))]));
        assert!(all_validators_randomness_healthy(&[mk_val(Some(
            "healthy"
        ))]));
    }
    {
        let mut g = fake.state.lock().unwrap();
        g.rpc_next_activation_override = None;
        g.rpc_omit_randomness_status = true;
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
    }
    let missing_rs_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_millis(120),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("missing randomnessStatus must fail closed without stopping any VM");
    assert!(
        format!("{missing_rs_err:#}")
            .contains("timed out waiting to clear DKG activation boundary"),
        "{missing_rs_err:#}"
    );
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());

    // 4. Active DKG ceremony (`randomnessStatus == "preparing"`) AND remaining=150 (> floor 30)
    //    MUST defer stopping validator-0 until AFTER boundary 1200 when randomness is healthy!
    {
        let mut g = fake.state.lock().unwrap();
        g.rpc_next_activation_override = None;
        g.rpc_omit_randomness_status = false;
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_height_base = 1050;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
        g.rpc_randomness_preparing_until_height = Some(1200);
    }
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(90),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    let g = fake.state.lock().unwrap();
    assert!(
        g.update_height_at_call[0].1 > 1200,
        "validator-0 must defer past boundary 1200 during ceremony/time-guard: {:?}",
        g.update_height_at_call
    );
}

#[tokio::test]
async fn s7_f4_fresh_health_recheck_aborts_before_stopping_second_validator() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7f4", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-f4-v2-bytes").unwrap();
    let v2_image_hash = "7777777777777777".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // Kill validator-3 (`10.77.0.13`) immediately AFTER validator-0 finishes its 3 post-update
    // lockstep polling rounds (`baseline`, `+30` streak 1, `+60` streak 2), right when validator-1's
    // pre-stop gate takes its fresh sample (round 4)!
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(300);
        g.kill_ip_after_vm_update_rounds =
            Some(("s7f4-validator-0".to_owned(), 4, "10.77.0.13".to_owned()));
    }

    let err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("F4: must abort before stopping validator-1 when validator-3 fell over");
    assert!(
        format!("{err:#}").contains("pre-stop health check before updating `validator-1`"),
        "{err:#}"
    );
    let g = fake.state.lock().unwrap();
    let called: Vec<&str> = g
        .vm_update_calls
        .iter()
        .map(|(n, _, _, _)| n.as_str())
        .collect();
    assert_eq!(
        called,
        vec!["s7f4-validator-0"],
        "must NEVER stop validator-1 while validator-3 is unhealthy"
    );
}

#[tokio::test]
async fn s7_pre_stop_requires_every_validator_connected_peers_at_least_n_minus_1() {
    // 1. Unit check on `check_validators_connected_peers` (fail closed on None or < n-1).
    let mk_val = |name: &str, peers: Option<u64>| VmMemberStatus {
        name: name.to_owned(),
        role: MemberRole::Founder,
        vm_name: format!("s7p-{name}"),
        vm_id: format!("id-{name}"),
        host: "192.0.2.50".to_owned(),
        classification: VmClassKind::Ours,
        classification_detail: None,
        service_ip: Ipv4Addr::new(10, 77, 0, 10),
        dhcp_ip: None,
        mac: "52:54:00:00:00:01".to_owned(),
        vmm_status: "running".to_owned(),
        vmm_running: true,
        no_tee: false,
        chain_id: Some(424_242),
        finalized: Some(500),
        finalized_hash: Some("0xabc".to_owned()),
        execution_peers: Some(3),
        consensus_mode: Some("signer".to_owned()),
        consensus_peers: peers,
        has_threshold_shares: Some(true),
        vrf_material_version: Some(1),
        tee_ready: Some(true),
        hyperlane_healthy: Some(true),
        hyperlane_controller: Some("waiting_for_controller".to_owned()),
        hyperlane_active: Some(false),
        consensus: peers.map(|p| ConsensusStatus {
            connected_peers: p,
            is_validator: true,
            mode: "signer".to_owned(),
            has_threshold_shares: true,
            randomness_status: Some("healthy".to_owned()),
            ..Default::default()
        }),
        boot_warning: None,
        error: None,
    };
    assert!(check_validators_connected_peers(&[mk_val("validator-0", None)], 3).is_err());
    assert!(check_validators_connected_peers(&[mk_val("validator-0", Some(0))], 3).is_err());
    let degraded_err = check_validators_connected_peers(
        &[
            mk_val("validator-0", Some(3)),
            mk_val("validator-1", Some(2)),
        ],
        3,
    )
    .unwrap_err()
    .to_string();
    assert!(
        degraded_err.contains("validator-1")
            && degraded_err.contains("connectedPeers=2")
            && degraded_err.contains("n-1=3"),
        "{degraded_err}"
    );
    check_validators_connected_peers(
        &[
            mk_val("validator-0", Some(3)),
            mk_val("validator-1", Some(4)),
        ],
        3,
    )
    .unwrap();

    // 2. End-to-end `update_with_exec`: validator-2 reports `connectedPeers = 2` (> 0, < n-1 = 3)
    //    while all 4 VMs are running and finalizing. Must fail closed BEFORE stopping any VM!
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7p", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-peers-v2-bytes").unwrap();
    let v2_image_hash = "8888888888888888".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name;
        g.os_image_hash = v2_image_hash;
        g.rpc_poll_counter = 0;
        g.rpc_height_base = 500;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(1200);
        // Degrade validator-2's consensus connectedPeers to 2 (> 0, but < n-1 = 3).
        g.rpc_connected_peers_override
            .insert("10.77.0.12".to_owned(), 2);
    }

    let err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("must refuse update when any validator has connectedPeers < n-1");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("validator-2") && msg.contains("connectedPeers=2") && msg.contains("n-1=3"),
        "{msg}"
    );
    assert!(
        fake.state.lock().unwrap().vm_update_calls.is_empty(),
        "must NOT stop or update any VM when connectedPeers < n-1"
    );
}

#[tokio::test]
async fn s7_pre_stop_missed_votes_fails_closed_and_retries_across_epoch_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s7mv", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    let v2_image_archive = dir.path().join("image-v2.tar.zst");
    fs::write(&v2_image_archive, b"fake-s7-mv-v2-bytes").unwrap();
    let v2_image_hash = "9999999999999999".repeat(4);
    let v2_image_name = format!("outbe-vm-dev-{}", &v2_image_hash[..16]);

    // 1. Persistent missed votes (+2 every window) -> retries until per-VM deadline, then fails
    //    closed with the last offending delta and 0 VM stops!
    {
        let mut g = fake.state.lock().unwrap();
        g.image_name = v2_image_name.clone();
        g.os_image_hash = v2_image_hash.clone();
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_height_base = 100;
        g.rpc_height_step = 30;
        // Large epoch so Phase 1 passes immediately and Phase 2 retries persistent +2 until deadline.
        g.rpc_dynamic_epoch_len = Some(10_000);
        g.rpc_participation_calls = 0;
        g.rpc_participation_persistent_step = 2;
    }
    let delta_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_millis(250),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("must fail closed on deadline when missed votes persist across retries");
    let delta_msg = format!("{delta_err:#}");
    assert!(
        delta_msg.contains("missed") && delta_msg.contains("+2"),
        "{delta_msg}"
    );
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());

    // 2. Participation RPC error -> fail closed immediately with 0 stops!
    {
        let mut g = fake.state.lock().unwrap();
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_participation_calls = 0;
        g.rpc_participation_persistent_step = 0;
        g.rpc_fail_participation = true;
    }
    let rpc_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive.clone(),
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("must fail closed when IValidatorSet.validatorByAddress returns an RPC error");
    assert!(
        format!("{rpc_err:#}").contains("validatorByAddress"),
        "{rpc_err:#}"
    );
    assert!(fake.state.lock().unwrap().vm_update_calls.is_empty());

    // 3. Transient missed votes (`0 -> 2 -> 2`) AND epoch-boundary counter reset (`5 -> 0 -> 0`)
    //    MUST retry the 60-block window, re-check the DKG guard, and succeed once 60 clean blocks
    //    pass with delta == 0!
    {
        let mut g = fake.state.lock().unwrap();
        g.rpc_fail_participation = false;
        g.rpc_poll_counter = 0;
        g.rpc_step_round = 0;
        g.rpc_batch_seen_since_last_status = false;
        g.rpc_height_base = 100;
        g.rpc_height_step = 30;
        g.rpc_dynamic_epoch_len = Some(1200);
        g.rpc_participation_calls = 0;
        // Validator-0: call 0=0, call 1=2 (transient +2 -> retries!), call 2=2 (delta==0 -> passes!).
        // Validator-1: call 3=5, call 4=0 (epoch counter reset -> retries!), call 5=0 (passes!).
        // Validator-2 & 3: 0 -> 0 (2 calls each). Total >= 10 participation batches!
        g.rpc_participation_missed_schedule = vec![0, 2, 2, 5, 0, 0];
    }
    update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: v2_image_archive,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(30),
            stop_timeout: Duration::from_secs(30),
            max_lag: 20,
            min_finalized_blocks: 10,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap();

    let g = fake.state.lock().unwrap();
    assert_eq!(g.vm_update_calls.len(), 4);
    assert!(
        g.rpc_participation_calls >= 10,
        "expected >= 10 participation batches (transient retry + boundary retry), got {}",
        g.rpc_participation_calls
    );
    for vm in g.vms.values() {
        assert_eq!(vm.os_image_hash, v2_image_hash);
    }
}

#[tokio::test]
async fn s10_status_fails_when_validator_consensus_peers_below_n_minus_1_or_missing() {
    let mk_member = |name: &str, role: MemberRole, peers: Option<u64>| VmMemberStatus {
        name: name.to_owned(),
        role,
        vm_name: format!("s10-{name}"),
        vm_id: format!("id-{name}"),
        host: "192.0.2.50".to_owned(),
        classification: VmClassKind::Ours,
        classification_detail: None,
        service_ip: Ipv4Addr::new(10, 77, 0, 10),
        dhcp_ip: None,
        mac: "52:54:00:00:00:01".to_owned(),
        vmm_status: "running".to_owned(),
        vmm_running: true,
        no_tee: false,
        chain_id: Some(424_242),
        finalized: Some(500),
        finalized_hash: Some("0xabc".to_owned()),
        execution_peers: Some(3),
        consensus_mode: Some(if role == MemberRole::FullNode {
            "follower".to_owned()
        } else {
            "signer".to_owned()
        }),
        consensus_peers: peers,
        has_threshold_shares: Some(role != MemberRole::FullNode),
        vrf_material_version: Some(if role == MemberRole::FullNode { 0 } else { 1 }),
        tee_ready: Some(role != MemberRole::FullNode),
        hyperlane_healthy: (role != MemberRole::FullNode).then_some(true),
        hyperlane_controller: (role != MemberRole::FullNode)
            .then(|| "waiting_for_controller".to_owned()),
        hyperlane_active: (role != MemberRole::FullNode).then_some(false),
        consensus: peers.map(|p| ConsensusStatus {
            connected_peers: p,
            is_validator: role != MemberRole::FullNode,
            mode: if role == MemberRole::FullNode {
                "follower".to_owned()
            } else {
                "signer".to_owned()
            },
            has_threshold_shares: role != MemberRole::FullNode,
            randomness_status: Some("healthy".to_owned()),
            ..Default::default()
        }),
        boot_warning: None,
        error: None,
    };
    let ok_assess = || MemberLagAssessment {
        lag: Some(0),
        lagging: false,
        reasons: Vec::new(),
    };

    // 1. Missing `outbe_consensusStatus` on a validator -> unhealthy (Err).
    let missing_err = check_unhealthy_members(
        &[mk_member("validator-0", MemberRole::Founder, None)],
        &[ok_assess()],
        &[],
        3,
    )
    .unwrap_err()
    .to_string();
    assert!(
        missing_err.contains("validator-0")
            && missing_err.contains("missing outbe_consensusStatus"),
        "{missing_err}"
    );

    // 2. Validator reporting `connectedPeers < n - 1` (0 or 2 in a 4-validator network) -> unhealthy.
    for low in [0u64, 2u64] {
        let low_err = check_unhealthy_members(
            &[
                mk_member("validator-0", MemberRole::Founder, Some(3)),
                mk_member("validator-1", MemberRole::Founder, Some(low)),
            ],
            &[ok_assess(), ok_assess()],
            &[],
            3,
        )
        .unwrap_err()
        .to_string();
        assert!(
            low_err.contains("validator-1")
                && low_err.contains(&format!("connectedPeers={low}"))
                && low_err.contains("n-1=3"),
            "{low_err}"
        );
    }

    // 3. Validators with `connectedPeers >= n - 1` (3 or 4) + full-node with 0 -> healthy (Ok).
    check_unhealthy_members(
        &[
            mk_member("validator-0", MemberRole::Founder, Some(3)),
            mk_member("validator-1", MemberRole::Founder, Some(4)),
            mk_member("full-node-0", MemberRole::FullNode, Some(0)),
        ],
        &[ok_assess(), ok_assess(), ok_assess()],
        &[],
        3,
    )
    .unwrap();

    // 4. End-to-end `status_with_exec` (table & --json) returns Err when validator-2 drops to 2 peers.
    let dir = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(dir.path(), "s10st", "~/.dstack-vmm", b"fake-outbe-up");
    let fake = FakeHostExec::new();
    let exec: Arc<dyn HostExec> = Arc::new(fake.clone());

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

    // Degrade validator-2 (`10.77.0.12`) to `connectedPeers = 2` (< n-1 = 3).
    fake.state
        .lock()
        .unwrap()
        .rpc_connected_peers_override
        .insert("10.77.0.12".to_owned(), 2);

    for json_mode in [false, true] {
        let err = status_with_exec(
            &StatusOptions {
                artifacts: &fixture.root,
                ssh_key: None,
                ssh_config: None,
                watch: false,
                max_lag: 20,
                json: json_mode,
            },
            Arc::clone(&exec),
        )
        .await
        .expect_err("network status must fail when validator connectedPeers < n-1");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("validator-2")
                && msg.contains("connectedPeers=2")
                && msg.contains("n-1=3"),
            "json={json_mode}: {msg}"
        );
    }
}

#[test]
fn s10_member_node_ready_requires_execution_peers_ge_1_and_consensus_peers_ge_n_minus_1() {
    let mk_val = |exec_peers: Option<u64>, cons_peers: u64| VmMemberStatus {
        name: "validator-0".to_owned(),
        role: MemberRole::Founder,
        vm_name: "s10ep-validator-0".to_owned(),
        vm_id: "id-0".to_owned(),
        host: "192.0.2.50".to_owned(),
        classification: VmClassKind::Ours,
        classification_detail: None,
        service_ip: Ipv4Addr::new(10, 77, 0, 10),
        dhcp_ip: None,
        mac: "52:54:00:00:00:01".to_owned(),
        vmm_status: "running".to_owned(),
        vmm_running: true,
        no_tee: false,
        chain_id: Some(424_242),
        finalized: Some(500),
        finalized_hash: Some("0xabc".to_owned()),
        execution_peers: exec_peers,
        consensus_mode: Some("signer".to_owned()),
        consensus_peers: Some(cons_peers),
        has_threshold_shares: Some(true),
        vrf_material_version: Some(1),
        tee_ready: Some(true),
        hyperlane_healthy: Some(true),
        hyperlane_controller: Some("waiting_for_controller".to_owned()),
        hyperlane_active: Some(false),
        consensus: Some(ConsensusStatus {
            connected_peers: cons_peers,
            is_validator: true,
            mode: "signer".to_owned(),
            has_threshold_shares: true,
            randomness_status: Some("healthy".to_owned()),
            enclave: outbe_operator_node_role::rpc::EnclaveHealth {
                state: "ready".to_owned(),
                ready: true,
                offer_key_ready: true,
            },
            ..Default::default()
        }),
        boot_warning: None,
        error: None,
    };

    // 5-validator network (required_peers = n - 1 = 4):
    // 0 or missing execution_peers -> NOT ready.
    assert!(!member_node_ready(&mk_val(None, 4), 424_242, 4));
    assert!(!member_node_ready(&mk_val(Some(0), 4), 424_242, 4));
    // execution_peers >= 1 (e.g. 1 or 3) with consensus_peers == 4 -> ready!
    assert!(member_node_ready(&mk_val(Some(1), 4), 424_242, 4));
    assert!(member_node_ready(&mk_val(Some(3), 4), 424_242, 4));
    // consensus_peers < 4 STILL fails closed even with plenty of execution peers!
    assert!(!member_node_ready(&mk_val(Some(5), 3), 424_242, 4));
}

#[test]
fn vm_rpc_queries_use_configured_non_default_rpc_port() {
    struct CapturingExec {
        commands: Mutex<Vec<String>>,
    }

    impl HostExec for CapturingExec {
        fn capture(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<String> {
            Ok(String::new())
        }
        fn capture_raw(&self, _ssh: &Ssh, _label: &str, _command: &str) -> Result<String> {
            Ok(String::new())
        }
        fn capture_stdin(
            &self,
            _ssh: &Ssh,
            _label: &str,
            command: &str,
            stdin: &[u8],
        ) -> Result<String> {
            self.commands.lock().unwrap().push(command.to_owned());
            let body = String::from_utf8_lossy(stdin);
            if body.contains("outbe_consensusStatus") {
                Ok(serde_json::json!([
                    {"jsonrpc": "2.0", "id": 1, "result": "0x67932"},
                    {"jsonrpc": "2.0", "id": 2, "result": {"number": "0x10", "hash": "0xabc"}},
                    {"jsonrpc": "2.0", "id": 3, "result": "0x3"},
                    {"jsonrpc": "2.0", "id": 4, "result": null}
                ])
                .to_string())
            } else {
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"hash": "0xdeadbeef"}
                })
                .to_string())
            }
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
            _sources: &[&Path],
            _destination: &str,
            _recursive: bool,
        ) -> Result<()> {
            Ok(())
        }
    }

    let exec = CapturingExec {
        commands: Mutex::new(Vec::new()),
    };
    let ssh = Ssh {
        host: "198.51.100.10".to_owned(),
        user: "ubuntu".to_owned(),
        port: 22,
    };
    let ip = Ipv4Addr::new(10, 77, 0, 11);

    let batch = query_vm_rpc_batch(&exec, &ssh, "validator-0", ip, 18_545).unwrap();
    assert_eq!(batch.chain_id, Some(0x67932));
    assert_eq!(batch.finalized, Some(0x10));

    let hash = query_vm_block_hash(
        &exec,
        VmRpcTarget {
            ssh: &ssh,
            label: "validator-0",
            service_ip: ip,
            rpc_port: 18_545,
        },
        16,
    )
    .unwrap();
    assert_eq!(hash, "0xdeadbeef");

    let recorded = exec.commands.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    for cmd in recorded.iter() {
        assert!(
            cmd.contains("http://10.77.0.11:18545"),
            "expected custom RPC port 18545 in command: {cmd}"
        );
        assert!(
            !cmd.contains(":8545"),
            "command must never hardcode :8545: {cmd}"
        );
    }
}

#[tokio::test]
async fn deploy_start_and_update_refuse_when_rpc_bind_ip_differs_from_vm_ip() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(tmp.path(), "loopbackrpc", "~/.dstack-vmm", b"outbe-up");

    // Rewrite network.yaml so founder-0 binds RPC to 127.0.0.1 instead of vm.vm_ip (10.77.0.10),
    // and update config_sha256 in network-manifest.json so NetworkArtifacts::load succeeds.
    let yaml_path = fixture.root.join("network.yaml");
    let raw_yaml = fs::read_to_string(&yaml_path).unwrap().replace(
        "  - name: validator-0\n    advertise_ip: 10.77.0.10\n",
        "  - name: validator-0\n    advertise_ip: 10.77.0.10\n    rpc_bind_ip: 127.0.0.1\n",
    );
    fs::write(&yaml_path, &raw_yaml).unwrap();
    let new_sha = sha256_file(&yaml_path).unwrap();
    let manifest_path = fixture.root.join("network-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["config_sha256"] = json!(new_sha);
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let fake = Arc::new(FakeHostExec::new());
    let exec: Arc<dyn HostExec> = fake;
    let fake_image = tmp.path().join("img.tar.zst");
    fs::write(&fake_image, b"img").unwrap();

    let deploy_err = deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: true,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(
        deploy_err
            .to_string()
            .contains("rpc_bind_ip=127.0.0.1 != vm.vm_ip=10.77.0.10"),
        "{deploy_err}"
    );

    let start_err = start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 1,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(
        start_err
            .to_string()
            .contains("rpc_bind_ip=127.0.0.1 != vm.vm_ip=10.77.0.10"),
        "{start_err}"
    );

    let update_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 1,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .unwrap_err();
    assert!(
        update_err
            .to_string()
            .contains("rpc_bind_ip=127.0.0.1 != vm.vm_ip=10.77.0.10"),
        "{update_err}"
    );
}

#[tokio::test]
async fn certified_rpc_preflight_skips_brand_new_deploy_checks_end_of_start_and_gates_existing_ops()
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 1. Verify private vs public URL classification.
    assert!(is_private_rpc_url("http://10.77.0.10:8545").unwrap());
    assert!(is_private_rpc_url("http://192.168.1.5:18545").unwrap());
    assert!(is_private_rpc_url("http://172.16.0.2:8545").unwrap());
    assert!(is_private_rpc_url("http://127.0.0.1:8545").unwrap());
    assert!(is_private_rpc_url("http://localhost:8545").unwrap());
    assert!(is_private_rpc_url("http://169.254.1.2:8545").unwrap());
    assert!(!is_private_rpc_url("https://rpc.testnet.outbe.net").unwrap());
    assert!(!is_private_rpc_url("http://198.51.100.10:8545").unwrap());

    // 2. Verify local direct HTTP eth_chainId helper against a real ephemeral HTTP listener.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).await;
        let body = r#"{"jsonrpc":"2.0","id":1,"result":"0x67932"}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });
    let got_cid = query_certified_rpc_chain_id_local(&format!("http://{local_addr}"))
        .await
        .unwrap();
    assert_eq!(got_cid, 424_242);
    server.await.unwrap();

    // 3. Brand-new founding deploy succeeds even when certified_rpc is unreachable (no chain yet).
    let tmp = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(tmp.path(), "crpc", "~/.dstack-vmm", b"outbe-up");
    let fake_image = tmp.path().join("img.tar.zst");
    fs::write(&fake_image, b"img").unwrap();
    let fake = Arc::new(FakeHostExec::new());
    fake.state.lock().unwrap().certified_rpc_chain_id_override =
        Some(Err("connection refused".to_owned()));
    let exec: Arc<dyn HostExec> = fake.clone();

    deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: true,
        },
        Arc::clone(&exec),
    )
    .await
    .expect("brand-new deploy must skip certified_rpc check when no VMs are running yet");

    // 4. `start` checks certified_rpc at the end and fails on mismatched chain_id.
    fake.state.lock().unwrap().certified_rpc_chain_id_override = Some(Ok(999_999));
    let start_err = start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(15),
            max_lag: 20,
            min_finalized_blocks: 1,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("start must fail at the end when certified_rpc returns wrong chain_id");
    assert!(
        start_err
            .to_string()
            .contains("returned chain_id 999999, expected 424242"),
        "{start_err}"
    );

    // 5. With VMs now running, re-running `deploy`, `add_validator`, `add_full_node`, and `update`
    //    must ALL fail closed before mutations when certified_rpc returns wrong chain_id.
    let redeploy_err = deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image.clone(),
                sha256: None,
            },
            ssh_key: None,
            ssh_config: None,
            no_tee: true,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("deploy against running network must check certified_rpc");
    assert!(
        redeploy_err
            .to_string()
            .contains("returned chain_id 999999, expected 424242"),
        "{redeploy_err}"
    );

    let add_fn_err = add_full_node_with_exec(
        &AddFullNodeOptions {
            artifacts: &fixture.root,
            name: "fullnode-0",
            host: "192.0.2.50",
            user: None,
            port: 22,
            vmm_home: None,
            bridge: None,
            vm_ip: Ipv4Addr::new(10, 77, 0, 30),
            rpc_bind_ip: None,
            advertise_ip: None,
            rpc_port: 8545,
            image_source: None,
            vcpu: None,
            memory_gib: None,
            disk_gib: None,
            no_tee: true,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 1,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("add-full-node must fail before provisioning on wrong certified_rpc chain_id");
    assert!(
        add_fn_err
            .to_string()
            .contains("returned chain_id 999999, expected 424242"),
        "{add_fn_err}"
    );

    let update_err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 1,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("update must fail before rolling on wrong certified_rpc chain_id");
    assert!(
        update_err
            .to_string()
            .contains("returned chain_id 999999, expected 424242"),
        "{update_err}"
    );

    // 6. Once certified_rpc answers with expected chain_id (424242), `start` succeeds.
    fake.state.lock().unwrap().certified_rpc_chain_id_override = Some(Ok(424_242));
    fake.state.lock().unwrap().rpc_poll_counter = 0;
    start_with_exec(
        &StartOptions {
            artifacts: &fixture.root,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(15),
            max_lag: 20,
            min_finalized_blocks: 1,
        },
        Arc::clone(&exec),
    )
    .await
    .expect("start must succeed once certified_rpc answers expected chain_id");
}

#[test]
fn manifest_firewall_mappings_match_host_check_port_specs() {
    let mixed: crate::network::config::NetworkConfig = serde_yaml_ng::from_str(
        r#"
version: 1
name: fw-align
network: testnet
chain_id: 54322345
certified_rpc: "http://10.77.0.10:8545"
vm:
  vmm_host:
    host: 198.51.100.10
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
founders:
  - name: validator-0
    advertise_ip: 198.51.100.10
    hyperlane_public_url: http://198.51.100.10:9000
    vm:
      vm_ip: 10.77.0.10
      port_offset: 0
  - name: validator-1
    advertise_ip: 198.51.100.10
    hyperlane_public_url: http://198.51.100.10:9001
    vm:
      vm_ip: 10.77.0.11
      port_offset: 1
  - name: validator-2
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9000
    vm:
      vm_ip: 10.77.0.10
      port_offset: 0
      vmm_host:
        host: 203.0.113.20
  - name: validator-3
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9001
    vm:
      vm_ip: 10.77.0.11
      port_offset: 1
      vmm_host:
        host: 203.0.113.20
hyperlane:
  gasless: true
topology:
  use_local_defaults: false
genesis:
  prefunded_accounts: {}
"#,
    )
    .unwrap();
    mixed.validate().unwrap();
    let mappings = crate::network::build::build_vm_firewall_mappings(&mixed).unwrap();
    let members: Vec<DesiredMember> = mixed
        .founders
        .iter()
        .enumerate()
        .map(|(index, founder)| {
            let r = mixed.resolve_founder_vm(founder).unwrap();
            DesiredMember {
                index,
                role: MemberRole::Founder,
                name: founder.name.clone(),
                vm_name: format!("fw-align-{}", founder.name),
                owner_tag: format!("outbe-net:fw-align:01234567:{}", founder.name),
                vmm_host: r.vmm_host,
                vmm_home: r.vmm_home,
                bridge: r.bridge,
                service_ip: r.vm_ip,
                advertise_ip: founder.advertise_ip,
                rpc_port: founder.rpc_port(),
                port_offset: r.port_offset,
                vcpu: r.vcpu,
                memory_gib: r.memory_gib,
                disk_gib: r.disk_gib,
                flavor: r.flavor,
                no_tee: false,
                image_name: None,
                image_hash: None,
                recorded_disk_sha256: None,
            }
        })
        .collect();
    let ctx = PublishContext::from_members(&members);
    for member in &members {
        let from_manifest: Vec<String> = mappings
            .iter()
            .filter(|e| e.member == member.name)
            .map(|e| {
                if e.vm_port == e.public_port {
                    format!("{}/{}", e.public_port, e.proto)
                } else {
                    format!("{}:{}/{}", e.vm_port, e.public_port, e.proto)
                }
            })
            .collect();
        let mut from_host_check = ctx.p2p_port_specs(member).unwrap();
        from_host_check.extend(ctx.hyperlane_port_specs(member).unwrap());
        assert_eq!(
            from_manifest, from_host_check,
            "manifest firewall_mappings must match host check port specs for {}",
            member.name
        );
    }
}

#[tokio::test]
async fn update_refuses_legacy_env_schema_disks_before_touching_hosts() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = build_test_vm_artifacts(tmp.path(), "legacyenv", "~/.dstack-vmm", b"outbe-up");
    let fake_image = tmp.path().join("img.tar.zst");
    fs::write(&fake_image, b"img").unwrap();
    let fake = Arc::new(FakeHostExec::new());
    let exec: Arc<dyn HostExec> = fake.clone();

    deploy_with_exec(
        &DeployOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image.clone(),
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

    // Simulate a legacy `network-vm-state.json` written before `env_schema` existed (defaults to 1).
    let state_path = NetworkVmState::path(&fixture.root);
    let mut raw: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    raw.as_object_mut().unwrap().remove("env_schema");
    fs::write(&state_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

    fake.state.lock().unwrap().vm_update_calls.clear();
    let err = update_with_exec(
        &UpdateOptions {
            artifacts: &fixture.root,
            image_source: ImageSource::Archive {
                path: fake_image,
                sha256: None,
            },
            software_bundle: None,
            ssh_key: None,
            ssh_config: None,
            timeout: Duration::from_secs(10),
            stop_timeout: Duration::from_secs(10),
            max_lag: 20,
            min_finalized_blocks: 1,
            dkg_guard_blocks: 30,
        },
        Arc::clone(&exec),
    )
    .await
    .expect_err("network update must refuse legacy env_schema v1 disks");
    assert!(
        err.to_string().contains("validator.env schema v1")
            && err.to_string().contains("requires schema v2"),
        "{err}"
    );
    assert!(
        fake.state.lock().unwrap().vm_update_calls.is_empty(),
        "must refuse before updating any VM"
    );
}
