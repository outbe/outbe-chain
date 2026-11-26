use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use outbe_operator_node_role::{
    bundle::{self, BundleInput},
    NODE_ROLE_FILE,
};
use outbe_operator_vm::disk::sha256_sparse_file;
use outbe_software_bundle::sha256_file;
use serde_json::Value;

const OUTBE_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-up");
const PARTITION_OFFSET_BYTES: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn has_disk_tools() -> bool {
    ["sfdisk", "mke2fs", "debugfs"]
        .into_iter()
        .all(|tool| {
            Command::new("sh")
                .args(["-c", &format!("command -v {tool} >/dev/null 2>&1 || [ -x /usr/sbin/{tool} ] || [ -x /sbin/{tool} ]")])
                .status()
                .is_ok_and(|s| s.success())
        })
}

fn debugfs_bin() -> &'static str {
    for candidate in ["/usr/sbin/debugfs", "/sbin/debugfs", "debugfs"] {
        if Path::new(candidate).is_file()
            || (candidate == "debugfs" && Command::new("debugfs").arg("-V").output().is_ok())
        {
            return candidate;
        }
    }
    "debugfs"
}

fn extract_ext4_partition(disk: &Path, out: &Path) {
    let disk_bytes = fs::metadata(disk).unwrap().len();
    let part_bytes = disk_bytes - 2 * PARTITION_OFFSET_BYTES;
    let mut src = File::open(disk).unwrap();
    src.seek(SeekFrom::Start(PARTITION_OFFSET_BYTES)).unwrap();
    let mut reader = src.take(part_bytes);
    let mut dst = File::create(out).unwrap();
    std::io::copy(&mut reader, &mut dst).unwrap();
}

fn debugfs_stat(part: &Path, entry: &str) -> (bool, String) {
    let out = Command::new(debugfs_bin())
        .args(["-R", &format!("stat {entry}")])
        .arg(part)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let exists =
        out.status.success() && stdout.contains("Inode:") && !stderr.contains("File not found");
    (exists, stdout)
}

fn debugfs_cat(part: &Path, entry: &str) -> String {
    let out = Command::new(debugfs_bin())
        .args(["-R", &format!("cat {entry}")])
        .arg(part)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "debugfs cat {entry} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
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
    let genesis = serde_json::json!({
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
                "OUTBE_VM_PREFIX=\"24\"\n",
                "OUTBE_VM_GATEWAY=\"10.77.0.1\"\n",
                "OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\"\n",
                "OUTBE_CONSENSUS_PORT=\"30400\"\n",
                "OUTBE_RETH_PORT=\"30303\"\n",
                "OUTBE_DISCV5_PORT=\"31303\"\n",
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

fn build_vm_network_artifacts(artifacts: &Path, chain_id: u64) -> PathBuf {
    fs::create_dir_all(artifacts).unwrap();
    let founder_tree = artifacts.join("vm/validator-0");
    build_founder_tree(&founder_tree, chain_id);

    let network_yaml = format!(
        r#"version: 1
name: s3-test
network: devnet
chain_id: {chain_id}
certified_rpc: "http://10.77.0.10:8545"
founders:
  - name: validator-0
    vm:
      bridge_ip: 10.77.0.10
    advertise_ip: 10.77.0.10
    rpc_bind_ip: 10.77.0.10
    hyperlane_public_url: "http://10.77.0.10:9000"
  - name: validator-1
    vm:
      bridge_ip: 10.77.0.11
    advertise_ip: 10.77.0.11
    rpc_bind_ip: 10.77.0.11
    hyperlane_public_url: "http://10.77.0.11:9000"
  - name: validator-2
    vm:
      bridge_ip: 10.77.0.12
    advertise_ip: 10.77.0.12
    rpc_bind_ip: 10.77.0.12
    hyperlane_public_url: "http://10.77.0.12:9000"
  - name: validator-3
    vm:
      bridge_ip: 10.77.0.13
    advertise_ip: 10.77.0.13
    rpc_bind_ip: 10.77.0.13
    hyperlane_public_url: "http://10.77.0.13:9000"
hyperlane:
  gasless: true
  origins:
    - domain: {chain_id}
      rpc_urls: ["http://127.0.0.1:8545"]
      rpc_consensus: single
      reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  epoch_length_blocks: 1200
  dkg_prepare_window_blocks: 300
  dkg_activation_grace_blocks: 300
  prefunded_accounts: {{}}
  overrides: {{}}
vm:
  vmm_host:
    host: 192.0.2.50
    user: ubuntu
    port: 22
  vcpu: 3
  memory_gib: 5
  disk_gib: 1
  flavor: prod
"#
    );
    fs::write(artifacts.join("network.yaml"), network_yaml).unwrap();
    fs::write(artifacts.join("software-bundle.tar.zst"), b"sw").unwrap();
    fs::copy(
        founder_tree.join("genesis.json"),
        artifacts.join("genesis.json"),
    )
    .unwrap();
    let hash = alloy_primitives::B256::repeat_byte(0x42);

    let mut founders = Vec::new();
    for index in 0..4 {
        let rel = format!("vm/validator-{index}");
        if index > 0 {
            build_founder_tree(&artifacts.join(&rel), chain_id);
        }
        founders.push(serde_json::json!({
            "index": index,
            "name": format!("validator-{index}"),
            "public_ip": format!("10.77.0.{}", 10 + index),
            "private_ip": format!("10.77.0.{}", 10 + index),
            "validator_address": "0x1111111111111111111111111111111111111111",
            "hyperlane_address": "0x2222222222222222222222222222222222222222",
            "consensus_public_key": "aa",
            "vm": {
                "data_tree": rel,
                "vmm_host": "192.0.2.50",
                "vmm_user": "ubuntu",
                "vmm_port": 22,
                "bridge_ip": format!("10.77.0.{}", 10 + index),
                "vcpu": 3,
                "memory_gib": 5,
                "disk_gib": 1,
                "flavor": "prod",
                "has_authorized_keys": false
            }
        }));
    }

    let joining_dir = artifacts.join("joining");
    fs::create_dir_all(&joining_dir).unwrap();
    let op_bundle = build_operator_bundle(&joining_dir, chain_id);

    let manifest = serde_json::json!({
        "version": 2,
        "state": "ready",
        "deployment": "vm",
        "name": "s3-test",
        "network": "devnet",
        "chain_id": chain_id,
        "config_sha256": sha256_file(&artifacts.join("network.yaml")).unwrap(),
        "software": {
            "archive_sha256": sha256_file(&artifacts.join("software-bundle.tar.zst")).unwrap(),
            "commit": "abc",
            "release_tag": "v0",
            "target": "x86_64-unknown-linux-gnu",
        },
        "genesis": {
            "seeded_path": "genesis.seeded.json",
            "seeded_sha256": "00",
            "final_path": "genesis.json",
            "final_sha256": sha256_file(&artifacts.join("genesis.json")).unwrap(),
            "genesis_block_hash": format!("{hash:#x}"),
        },
        "founders": founders,
        "joining": {
            "bundle": "joining/outbe-validator-operator-bundle.tar.zst",
            "bundle_sha256": sha256_file(&op_bundle).unwrap(),
            "config_example": "joining/operator-config.example.yaml",
        }
    });
    fs::write(
        artifacts.join("network-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    founder_tree
}

fn build_operator_bundle(dir: &Path, chain_id: u64) -> PathBuf {
    build_operator_bundle_with_verify_and_validator(
        dir,
        chain_id,
        r#"    key_path=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--key" ]; then
            key_path="$2"
            shift 2
        else
            shift 1
        fi
    done
    val="$(tr -d '\r\n' < "$key_path")"
    case "$val" in
        *[!0-9a-fA-F]*|"")
            echo "VERIFY_SECRET_STDOUT_LEAK_deadbeef"
            echo "VERIFY_SECRET_STDERR_LEAK_cafebabe" >&2
            exit 1
            ;;
    esac
    [ "${#val}" -eq 64 ] || exit 1
    exit 0"#,
        "",
    )
}

fn build_operator_bundle_with_verify_and_validator(
    dir: &Path,
    chain_id: u64,
    verify_body: &str,
    extra_validator_hook: &str,
) -> PathBuf {
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
    let script = format!(
        r#"#!/bin/sh
set -eu
cmd="$1"
shift
if [ "$cmd" = "verify" ]; then
{verify_body}
fi
if [ "$cmd" = "validator" ]; then
    out=""
    while [ $# -gt 0 ]; do
        if [ "$1" = "--output-dir" ]; then
            out="$2"
            shift 2
        else
            shift 2
        fi
    done
    umask 077
    mkdir -p "$out/radicle/keys"
    printf '%s' 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' > "$out/signing-key.hex"
    printf '%s' 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' > "$out/evm-key.hex"
    printf '%s' 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc' > "$out/feeder-key.hex"
    printf '%s' 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd' > "$out/hyperlane-key.hex"
    printf '%s\n' 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee' > "$out/reth-p2p-secret.hex"
    printf '%s' 'SECRET_RADICLE_KEY_DO_NOT_LOG' > "$out/radicle/keys/radicle"
    echo "KEYGEN_STDOUT_SHOULD_NEVER_APPEAR_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
{extra_validator_hook}
    exit 0
fi
exit 1
"#
    );
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

fn write_operator_config(path: &Path, chain_id: u64) {
    let peer_key = "a".repeat(96);
    fs::write(
        path,
        format!(
            r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.20
  rpc_bind_ip: 10.77.0.20
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  consensus_peers:
    - "{peer_key}@10.77.0.10:30400"
  use_local_defaults: true
feeder:
  public_toml: |
    [oracle]
    sources = "local"
hyperlane:
  public_url: "http://10.77.0.20:9000"
  gasless: true
  origins:
    - domain: {chain_id}
      rpc_urls: ["http://127.0.0.1:8545"]
      rpc_consensus: single
      reorg_period: 0
"#
        ),
    )
    .unwrap();
}

fn write_full_node_config(path: &Path, chain_id: u64) {
    fs::write(
        path,
        format!(
            r#"version: 1
network:
  chain_id: {chain_id}
  certified_rpc: "http://10.77.0.10:8545"
  advertise_ip: 10.77.0.21
  rpc_bind_ip: 10.77.0.21
  vm_prefix: 24
  vm_gateway: 10.77.0.1
  vm_dns: [1.1.1.1, 8.8.8.8]
  bootnodes:
    - "enode://abcd@10.77.0.10:30303"
  use_local_defaults: true
"#
        ),
    )
    .unwrap();
}

#[test]
fn founder_disk_prepare_is_byte_identical_to_legacy_vm_disk_and_supports_tar() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let artifacts = tmp.path().join("network-artifacts");
    let founder_tree = build_vm_network_artifacts(&artifacts, 424_242);

    let legacy_img = tmp.path().join("legacy.img");
    let legacy_out = Command::new(OUTBE_UP_BIN)
        .args(["vm", "disk", "--artifacts"])
        .arg(&artifacts)
        .args(["--founder", "0", "--size-gib", "1", "--output"])
        .arg(&legacy_img)
        .output()
        .unwrap();
    assert!(
        legacy_out.status.success(),
        "legacy vm disk failed:\n{}",
        String::from_utf8_lossy(&legacy_out.stderr)
    );

    let prepared_img = tmp.path().join("founder-dir.img");
    let prep_out = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "founder", "--source"])
        .arg(&founder_tree)
        .args(["--size-gib", "1", "--output"])
        .arg(&prepared_img)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        prep_out.status.success(),
        "disk prepare --role founder failed:\n{}",
        String::from_utf8_lossy(&prep_out.stderr)
    );

    let legacy_sha = sha256_sparse_file(&legacy_img).unwrap();
    let prepared_sha = sha256_sparse_file(&prepared_img).unwrap();
    assert_eq!(
        legacy_sha, prepared_sha,
        "disk prepare --role founder must be byte-identical to legacy vm disk"
    );
    assert_eq!(
        fs::metadata(&legacy_img).unwrap().permissions().mode() & 0o777,
        0o600,
        "legacy vm disk raw image must be mode 0600"
    );
    assert_eq!(
        fs::metadata(&prepared_img).unwrap().permissions().mode() & 0o777,
        0o600,
        "disk prepare founder raw image must be mode 0600"
    );
    let json: Value = serde_json::from_slice(&prep_out.stdout).unwrap();
    assert_eq!(json["role"], "founder");
    assert_eq!(json["sha256"], prepared_sha);
    assert_eq!(json["key_verified"], true);
    assert!(json["keygen_source"]
        .as_str()
        .unwrap()
        .starts_with("artifacts:"));
    assert_eq!(fs::metadata(&prepared_img).unwrap().len(), GIB);

    // Founder directory input must not have been mutated with runtime dirs.
    assert!(!founder_tree.join("validator-0").exists());
    assert!(!founder_tree.join("state").exists());
    assert!(!founder_tree.join(NODE_ROLE_FILE).exists());

    // Tar archive input (.tar.zst) with explicit --bundle also produces a byte-identical disk image.
    let artifacts_bundle = artifacts.join("joining/outbe-validator-operator-bundle.tar.zst");
    let founder_tar = tmp.path().join("validator-0.tar.zst");
    let tar_out = Command::new("tar")
        .args(["--zstd", "-cf"])
        .arg(&founder_tar)
        .args(["-C"])
        .arg(artifacts.join("vm"))
        .arg("validator-0")
        .output()
        .unwrap();
    assert!(
        tar_out.status.success(),
        "tar --zstd failed: {}",
        String::from_utf8_lossy(&tar_out.stderr)
    );
    let tar_img = tmp.path().join("founder-tar.img");
    let tar_prep = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "founder", "--source"])
        .arg(&founder_tar)
        .arg("--bundle")
        .arg(&artifacts_bundle)
        .args(["--size-gib", "1", "--output"])
        .arg(&tar_img)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        tar_prep.status.success(),
        "disk prepare --role founder (tar.zst) failed:\n{}",
        String::from_utf8_lossy(&tar_prep.stderr)
    );
    assert_eq!(sha256_sparse_file(&tar_img).unwrap(), legacy_sha);
    assert_eq!(
        fs::metadata(&tar_img).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // Gzip tar archive (.tar.gz) with explicit --bundle also produces a byte-identical disk image.
    let founder_tgz = tmp.path().join("validator-0.tar.gz");
    let tgz_out = Command::new("tar")
        .args(["-czf"])
        .arg(&founder_tgz)
        .args(["-C"])
        .arg(artifacts.join("vm"))
        .arg("validator-0")
        .output()
        .unwrap();
    assert!(
        tgz_out.status.success(),
        "tar -czf failed: {}",
        String::from_utf8_lossy(&tgz_out.stderr)
    );
    let tgz_img = tmp.path().join("founder-tgz.img");
    let tgz_prep = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "founder", "--source"])
        .arg(&founder_tgz)
        .arg("--bundle")
        .arg(&artifacts_bundle)
        .args(["--size-gib", "1", "--output"])
        .arg(&tgz_img)
        .output()
        .unwrap();
    assert!(
        tgz_prep.status.success(),
        "disk prepare --role founder (tar.gz) failed:\n{}",
        String::from_utf8_lossy(&tgz_prep.stderr)
    );
    assert_eq!(sha256_sparse_file(&tgz_img).unwrap(), legacy_sha);
    assert_eq!(
        fs::metadata(&tgz_img).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn joining_validator_disk_prepare_generates_keys_never_logs_secrets_and_is_deterministic_with_keys_from(
) {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_operator_bundle(tmp.path(), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_operator_config(&cfg, 707);

    // 1. Fresh local key generation via bundle outbe-keygen.
    let fresh_img = tmp.path().join("joining-fresh.img");
    let out = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&fresh_img)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "joining-validator prepare failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for secret in [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "SECRET_RADICLE_KEY_DO_NOT_LOG",
        "KEYGEN_STDOUT_SHOULD_NEVER_APPEAR",
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
    assert_eq!(json["role"], "joining-validator");
    assert!(json["validator_address"]
        .as_str()
        .unwrap()
        .starts_with("0x"));
    assert!(json["hyperlane_address"]
        .as_str()
        .unwrap()
        .starts_with("0x"));

    // Inspect ext4 contents via debugfs: OUTBE_NODE_ROLE in validator.env, 0600 keys, no bundle binaries.
    let part = tmp.path().join("joining.ext4");
    extract_ext4_partition(&fresh_img, &part);
    assert!(debugfs_cat(&part, "outbe-chain/validator.env")
        .contains("OUTBE_NODE_ROLE=\"joining-validator\"\n"));
    let (has_legacy_role, _) = debugfs_stat(&part, "outbe-chain/node-role");
    assert!(
        !has_legacy_role,
        "joining disk must use validator.env OUTBE_NODE_ROLE instead of separate node-role file"
    );
    for secret_path in [
        "outbe-chain/keys/signing-key.hex",
        "outbe-chain/keys/evm-key.hex",
        "outbe-chain/keys/feeder-key.hex",
        "outbe-chain/keys/hyperlane-key.hex",
        "outbe-chain/keys/reth-p2p-secret.hex",
        "outbe-chain/keys/radicle/keys/radicle",
    ] {
        let (exists, stat) = debugfs_stat(&part, secret_path);
        assert!(exists, "missing {secret_path} in joining disk");
        assert!(
            stat.contains("Mode:  0600"),
            "{secret_path} must be mode 0600:\n{stat}"
        );
    }
    for forbidden_bin in [
        "outbe-chain/outbe-keygen",
        "outbe-chain/outbe-chain",
        "outbe-chain/operator-manifest.json",
    ] {
        let (exists, _) = debugfs_stat(&part, forbidden_bin);
        assert!(
            !exists,
            "bundle artifact {forbidden_bin} must not be on VM disk"
        );
    }

    // 2. Deterministic builds with --keys-from <dir>.
    let keys_dir = tmp.path().join("deterministic-keys");
    fs::create_dir_all(keys_dir.join("radicle/keys")).unwrap();
    for (rel, byte) in [
        ("signing-key.hex", "1"),
        ("evm-key.hex", "2"),
        ("feeder-key.hex", "3"),
        ("hyperlane-key.hex", "4"),
        ("reth-p2p-secret.hex", "5"),
    ] {
        write_mode(&keys_dir.join(rel), byte.repeat(64).as_bytes(), 0o600);
    }
    write_mode(
        &keys_dir.join("radicle/keys/radicle"),
        b"fixed-radicle",
        0o600,
    );

    let det_a = tmp.path().join("joining-det-a.img");
    let det_b = tmp.path().join("joining-det-b.img");
    for out_path in [&det_a, &det_b] {
        let res = Command::new(OUTBE_UP_BIN)
            .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
            .arg(&bundle)
            .arg("--config")
            .arg(&cfg)
            .arg("--keys-from")
            .arg(&keys_dir)
            .args(["--size-gib", "1", "--output"])
            .arg(out_path)
            .output()
            .unwrap();
        assert!(
            res.status.success(),
            "deterministic joining prepare failed:\n{}",
            String::from_utf8_lossy(&res.stderr)
        );
    }
    assert_eq!(
        sha256_sparse_file(&det_a).unwrap(),
        sha256_sparse_file(&det_b).unwrap(),
        "joining-validator disk with --keys-from must be deterministic"
    );
}

#[test]
fn full_node_disk_prepare_has_zero_keys_is_deterministic_and_device_target_refuses() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_operator_bundle(tmp.path(), 707);
    let full_cfg = tmp.path().join("fullnode.yaml");
    write_full_node_config(&full_cfg, 707);

    // 1. Two independent builds of full-node disk are byte-identical with zero keys.
    let img_a = tmp.path().join("fullnode-a.img");
    let img_b = tmp.path().join("fullnode-b.img");
    for out_path in [&img_a, &img_b] {
        let res = Command::new(OUTBE_UP_BIN)
            .args(["disk", "prepare", "--role", "full-node", "--bundle"])
            .arg(&bundle)
            .arg("--config")
            .arg(&full_cfg)
            .args(["--size-gib", "1", "--output"])
            .arg(out_path)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            res.status.success(),
            "full-node prepare failed:\n{}",
            String::from_utf8_lossy(&res.stderr)
        );
        let json: Value = serde_json::from_slice(&res.stdout).unwrap();
        assert_eq!(json["role"], "full-node");
        assert!(json.get("validator_address").is_none());
        assert!(json.get("hyperlane_address").is_none());
    }
    assert_eq!(
        sha256_sparse_file(&img_a).unwrap(),
        sha256_sparse_file(&img_b).unwrap()
    );

    let part = tmp.path().join("fullnode.ext4");
    extract_ext4_partition(&img_a, &part);
    assert!(
        debugfs_cat(&part, "outbe-chain/validator.env").contains("OUTBE_NODE_ROLE=\"full-node\"\n")
    );
    let (has_legacy_role, _) = debugfs_stat(&part, "outbe-chain/node-role");
    assert!(
        !has_legacy_role,
        "full-node disk must use validator.env OUTBE_NODE_ROLE instead of separate node-role file"
    );
    for required in [
        "outbe-chain/genesis.json",
        "outbe-chain/.bb-crs/bn254_g1.dat",
        "outbe-chain/validator.env",
    ] {
        let (exists, _) = debugfs_stat(&part, required);
        assert!(exists, "missing {required} in full-node disk");
    }
    for forbidden in [
        "outbe-chain/keys",
        "outbe-chain/feeder-public.toml",
        "outbe-chain/hyperlane-public.toml",
        "outbe-chain/outbe-chain",
    ] {
        let (exists, _) = debugfs_stat(&part, forbidden);
        assert!(!exists, "full-node disk must not contain {forbidden}");
    }

    // 2. Refuses to overwrite existing output file.
    let overwrite = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&img_a)
        .output()
        .unwrap();
    assert!(!overwrite.status.success());
    assert!(
        String::from_utf8_lossy(&overwrite.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&overwrite.stderr)
    );

    // 3. Refuses --device with clear R-3 / S3 probe message.
    let dev = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--device", "/dev/null"])
        .output()
        .unwrap();
    assert!(!dev.status.success());
    let dev_err = String::from_utf8_lossy(&dev.stderr);
    assert!(
        dev_err.contains("not yet supported, see S3 probe"),
        "unexpected --device error: {dev_err}"
    );

    // 4. Refuses --keys-from on full-node.
    let with_keys = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&full_cfg)
        .arg("--keys-from")
        .arg(tmp.path())
        .args(["--output"])
        .arg(tmp.path().join("never.img"))
        .output()
        .unwrap();
    assert!(!with_keys.status.success());
    assert!(
        String::from_utf8_lossy(&with_keys.stderr).contains("zero keys"),
        "{}",
        String::from_utf8_lossy(&with_keys.stderr)
    );

    // 5. Refuses OperatorConfig (validator sections) on full-node.
    let op_cfg = tmp.path().join("operator.yaml");
    write_operator_config(&op_cfg, 707);
    let bad_cfg = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&op_cfg)
        .args(["--output"])
        .arg(tmp.path().join("never2.img"))
        .output()
        .unwrap();
    assert!(!bad_cfg.status.success());
    assert!(
        String::from_utf8_lossy(&bad_cfg.stderr).contains("unknown field"),
        "{}",
        String::from_utf8_lossy(&bad_cfg.stderr)
    );
    assert_eq!(
        fs::metadata(&img_a).unwrap().permissions().mode() & 0o777,
        0o600,
        "full-node raw image must also be mode 0600"
    );
}

#[test]
fn sigint_cleans_up_staging_dirs_and_cleanup_stale_is_opt_in() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ready_marker = tmp.path().join("keygen.ready");
    let hook = format!(
        "    : > '{}'\n    sleep 30\n",
        ready_marker.to_str().unwrap()
    );
    let slow_bundle =
        build_operator_bundle_with_verify_and_validator(tmp.path(), 707, "    exit 0", &hook);
    let cfg = tmp.path().join("operator.yaml");
    write_operator_config(&cfg, 707);

    let out_dir = tmp.path().join("sigint-out");
    fs::create_dir_all(&out_dir).unwrap();
    let out_img = out_dir.join("canceled.img");

    let child = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&slow_bundle)
        .arg("--config")
        .arg(&cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&out_img)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Wait until fake outbe-keygen has written keys into the staging dir and signaled readiness.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready_marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for fake outbe-keygen to start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Send SIGINT to outbe-up.
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "outbe-up must exit non-zero after SIGINT"
    );
    assert!(
        fs::symlink_metadata(&out_img).is_err(),
        "canceled run must not publish output image"
    );

    // Zero .outbe-prepare-*, .outbe-disk-*, or .outbe-vm-disk-* staging dirs may remain.
    let leftover: Vec<String> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(".outbe-prepare-")
                || name.starts_with(".outbe-disk-")
                || name.starts_with(".outbe-vm-disk-")
        })
        .collect();
    assert!(
        leftover.is_empty(),
        "SIGINT left stale staging directories behind: {leftover:?}"
    );

    // Explicit --cleanup-stale removes planted stale dirs, whereas omitting it leaves them alone.
    let fast_bundle = build_operator_bundle(&tmp.path().join("fast"), 707);
    let full_cfg = tmp.path().join("fullnode.yaml");
    write_full_node_config(&full_cfg, 707);
    let stale_dir = out_dir.join(".outbe-disk-stale-leftover");
    fs::create_dir_all(&stale_dir).unwrap();

    let img_no_clean = out_dir.join("no-clean.img");
    let res1 = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&fast_bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&img_no_clean)
        .output()
        .unwrap();
    assert!(
        res1.status.success(),
        "{}",
        String::from_utf8_lossy(&res1.stderr)
    );
    assert!(
        stale_dir.is_dir(),
        "stale dir must not be deleted unless --cleanup-stale is passed"
    );

    let img_clean = out_dir.join("with-clean.img");
    let res2 = Command::new(OUTBE_UP_BIN)
        .args([
            "disk",
            "prepare",
            "--role",
            "full-node",
            "--cleanup-stale",
            "--bundle",
        ])
        .arg(&fast_bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&img_clean)
        .output()
        .unwrap();
    assert!(
        res2.status.success(),
        "{}",
        String::from_utf8_lossy(&res2.stderr)
    );
    assert!(
        !stale_dir.exists(),
        "--cleanup-stale must remove stale staging directories"
    );
}

#[test]
fn cli_enforces_keys_from_allowlist_redacts_verify_errors_verifies_founder_bls_and_refuses_dangling_symlinks(
) {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bundle = build_operator_bundle(tmp.path(), 707);
    let op_cfg = tmp.path().join("operator.yaml");
    write_operator_config(&op_cfg, 707);
    let full_cfg = tmp.path().join("fullnode.yaml");
    write_full_node_config(&full_cfg, 707);

    // F3: --keys-from rejects unexpected extra file.
    let keys_dir = tmp.path().join("extra-keys");
    fs::create_dir_all(keys_dir.join("radicle/keys")).unwrap();
    for (rel, ch) in [
        ("signing-key.hex", "1"),
        ("evm-key.hex", "2"),
        ("feeder-key.hex", "3"),
        ("hyperlane-key.hex", "4"),
        ("reth-p2p-secret.hex", "5"),
    ] {
        write_mode(&keys_dir.join(rel), ch.repeat(64).as_bytes(), 0o600);
    }
    write_mode(&keys_dir.join("radicle/keys/radicle"), b"rad", 0o600);
    write_mode(&keys_dir.join("extra-backup.env"), b"SECRET=1", 0o600);
    let f3_out = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&op_cfg)
        .arg("--keys-from")
        .arg(&keys_dir)
        .args(["--size-gib", "1", "--output"])
        .arg(tmp.path().join("f3.img"))
        .output()
        .unwrap();
    assert!(!f3_out.status.success());
    assert!(
        String::from_utf8_lossy(&f3_out.stderr)
            .contains("refusing unexpected file in --keys-from: extra-backup.env"),
        "{}",
        String::from_utf8_lossy(&f3_out.stderr)
    );

    // F4: outbe-keygen verify failure never leaks stdout or stderr markers through CLI output.
    fs::remove_file(keys_dir.join("extra-backup.env")).unwrap();
    write_mode(
        &keys_dir.join("signing-key.hex"),
        b"not-a-BLS-private-key",
        0o600,
    );
    let f4_out = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&op_cfg)
        .arg("--keys-from")
        .arg(&keys_dir)
        .args(["--size-gib", "1", "--output"])
        .arg(tmp.path().join("f4.img"))
        .output()
        .unwrap();
    assert!(!f4_out.status.success());
    let f4_stdout = String::from_utf8_lossy(&f4_out.stdout);
    let f4_stderr = String::from_utf8_lossy(&f4_out.stderr);
    assert!(
        f4_stderr.contains("signing-key.hex failed outbe-keygen verify"),
        "{f4_stderr}"
    );
    for marker in [
        "VERIFY_SECRET_STDOUT_LEAK_deadbeef",
        "VERIFY_SECRET_STDERR_LEAK_cafebabe",
    ] {
        assert!(!f4_stdout.contains(marker), "leaked to stdout: {f4_stdout}");
        assert!(!f4_stderr.contains(marker), "leaked to stderr: {f4_stderr}");
    }

    // F6: Founder BLS verification rejects corrupt key via auto-discovered bundle, refuses
    // standalone founder without --bundle/--skip-key-verify, and allows explicit --skip-key-verify.
    let artifacts = tmp.path().join("f6-artifacts");
    let founder_tree = build_vm_network_artifacts(&artifacts, 424_242);
    write_mode(
        &founder_tree.join("keys/signing-key.hex"),
        b"not-a-BLS-private-key",
        0o600,
    );
    let f6_bad = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "founder", "--source"])
        .arg(&founder_tree)
        .args(["--size-gib", "1", "--output"])
        .arg(tmp.path().join("f6-bad.img"))
        .output()
        .unwrap();
    assert!(!f6_bad.status.success());
    let f6_bad_err = String::from_utf8_lossy(&f6_bad.stderr);
    assert!(
        f6_bad_err.contains("signing-key.hex failed outbe-keygen verify"),
        "{f6_bad_err}"
    );
    assert!(!f6_bad_err.contains("VERIFY_SECRET_STDOUT_LEAK_deadbeef"));
    assert!(!f6_bad_err.contains("VERIFY_SECRET_STDERR_LEAK_cafebabe"));

    let standalone_founder = tmp.path().join("standalone-founder");
    build_founder_tree(&standalone_founder, 424_242);
    let f6_nobundle = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "founder", "--source"])
        .arg(&standalone_founder)
        .args(["--size-gib", "1", "--output"])
        .arg(tmp.path().join("f6-nobundle.img"))
        .output()
        .unwrap();
    assert!(!f6_nobundle.status.success());
    assert!(
        String::from_utf8_lossy(&f6_nobundle.stderr).contains("no verified operator bundle found"),
        "{}",
        String::from_utf8_lossy(&f6_nobundle.stderr)
    );

    let f6_skip_img = tmp.path().join("f6-skip.img");
    let f6_skip = Command::new(OUTBE_UP_BIN)
        .args([
            "disk",
            "prepare",
            "--role",
            "founder",
            "--skip-key-verify",
            "--source",
        ])
        .arg(&standalone_founder)
        .args(["--size-gib", "1", "--output"])
        .arg(&f6_skip_img)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        f6_skip.status.success(),
        "{}",
        String::from_utf8_lossy(&f6_skip.stderr)
    );
    let f6_json: Value = serde_json::from_slice(&f6_skip.stdout).unwrap();
    assert_eq!(f6_json["key_verified"], false);
    assert_eq!(
        fs::metadata(&f6_skip_img).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // F7: Refuse existing dangling symlink for both `disk prepare` and legacy `vm disk`, leaving
    // the symlink untouched.
    let dangling = tmp.path().join("dangling.img");
    let nonexistent = tmp.path().join("never-created-target.img");
    std::os::unix::fs::symlink(&nonexistent, &dangling).unwrap();

    let f7_prep = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "full-node", "--bundle"])
        .arg(&bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&dangling)
        .output()
        .unwrap();
    assert!(!f7_prep.status.success());
    assert!(
        String::from_utf8_lossy(&f7_prep.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&f7_prep.stderr)
    );
    assert!(fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!nonexistent.exists());

    let f7_vm = Command::new(OUTBE_UP_BIN)
        .args(["vm", "disk", "--artifacts"])
        .arg(&artifacts)
        .args(["--founder", "0", "--size-gib", "1", "--output"])
        .arg(&dangling)
        .output()
        .unwrap();
    assert!(!f7_vm.status.success());
    assert!(
        String::from_utf8_lossy(&f7_vm.stderr).contains("already exists"),
        "{}",
        String::from_utf8_lossy(&f7_vm.stderr)
    );
    assert!(fs::symlink_metadata(&dangling)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!nonexistent.exists());
}

#[test]
fn sigint_during_outbe_keygen_verify_terminates_child_and_cleans_staging() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ready_marker = tmp.path().join("verify.ready");
    let pid_file = tmp.path().join("verify.pid");
    let verify_body = format!(
        "    echo $$ > '{}'\n    : > '{}'\n    sleep 30\n    exit 0",
        pid_file.to_str().unwrap(),
        ready_marker.to_str().unwrap()
    );
    let slow_verify_bundle =
        build_operator_bundle_with_verify_and_validator(tmp.path(), 707, &verify_body, "");
    let cfg = tmp.path().join("operator.yaml");
    write_operator_config(&cfg, 707);

    let out_dir = tmp.path().join("verify-sigint-out");
    fs::create_dir_all(&out_dir).unwrap();
    let out_img = out_dir.join("canceled-verify.img");

    let child = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&slow_verify_bundle)
        .arg("--config")
        .arg(&cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&out_img)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready_marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for outbe-keygen verify to start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let start = std::time::Instant::now();
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "outbe-up took too long ({:?}) to exit after SIGINT during outbe-keygen verify",
        start.elapsed()
    );
    assert!(
        !output.status.success(),
        "outbe-up must exit non-zero after SIGINT during outbe-keygen verify"
    );
    assert!(fs::symlink_metadata(&out_img).is_err());

    let leftover: Vec<String> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(".outbe-prepare-")
                || name.starts_with(".outbe-disk-")
                || name.starts_with(".outbe-vm-disk-")
        })
        .collect();
    assert!(
        leftover.is_empty(),
        "SIGINT during outbe-keygen verify left staging dirs behind: {leftover:?}"
    );

    if let Ok(verify_pid) = fs::read_to_string(&pid_file) {
        if let Ok(raw) = verify_pid.trim().parse::<i32>() {
            if let Some(pid) = rustix::process::Pid::from_raw(raw) {
                let alive = rustix::process::test_kill_process(pid).is_ok();
                assert!(
                    !alive,
                    "outbe-keygen verify child (pid {raw}) survived SIGINT"
                );
            }
        }
    }
}

#[test]
fn sigint_during_stalled_gzip_extraction_terminates_gzip_and_cleans_staging() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let fake_bin = tmp.path().join("fake-bin");
    let ready_marker = tmp.path().join("gzip.ready");
    let pid_file = tmp.path().join("gzip.pid");
    let fake_gzip = fake_bin.join("gzip");
    write_mode(
        &fake_gzip,
        format!(
            "#!/bin/sh\necho $$ > '{}'\n: > '{}'\nsleep 30\n",
            pid_file.to_str().unwrap(),
            ready_marker.to_str().unwrap()
        )
        .as_bytes(),
        0o755,
    );

    // File with valid gzip magic bytes so extract_founder_tar spawns `gzip -dc`.
    let fake_tgz = tmp.path().join("founder.tar.gz");
    write_mode(&fake_tgz, &[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00], 0o644);

    let out_dir = tmp.path().join("gzip-sigint-out");
    fs::create_dir_all(&out_dir).unwrap();
    let out_img = out_dir.join("canceled-gzip.img");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{orig_path}", fake_bin.display());

    let child = Command::new(OUTBE_UP_BIN)
        .env("PATH", path_env)
        .args([
            "disk",
            "prepare",
            "--role",
            "founder",
            "--skip-key-verify",
            "--source",
        ])
        .arg(&fake_tgz)
        .args(["--size-gib", "1", "--output"])
        .arg(&out_img)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready_marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for stalled gzip child to start"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let start = std::time::Instant::now();
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "outbe-up took too long ({:?}) to exit after SIGINT during stalled gzip",
        start.elapsed()
    );
    assert!(
        !output.status.success(),
        "outbe-up must exit non-zero after SIGINT during gzip extraction"
    );
    assert!(fs::symlink_metadata(&out_img).is_err());

    let leftover: Vec<String> = fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with(".outbe-prepare-")
                || name.starts_with(".outbe-disk-")
                || name.starts_with(".outbe-vm-disk-")
        })
        .collect();
    assert!(
        leftover.is_empty(),
        "SIGINT during stalled gzip left staging dirs behind: {leftover:?}"
    );

    if let Ok(gzip_pid) = fs::read_to_string(&pid_file) {
        if let Ok(raw) = gzip_pid.trim().parse::<i32>() {
            if let Some(pid) = rustix::process::Pid::from_raw(raw) {
                let alive = rustix::process::test_kill_process(pid).is_ok();
                assert!(!alive, "stalled gzip child (pid {raw}) survived SIGINT");
            }
        }
    }
}

#[test]
fn cleanup_stale_preserves_live_concurrent_prepare_staging_dirs() {
    if !has_disk_tools() {
        eprintln!("SKIPPED: sfdisk/mke2fs/debugfs not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ready_marker = tmp.path().join("first.ready");
    let release_marker = tmp.path().join("first.release");
    let hook = format!(
        "    : > '{}'\n    while [ ! -f '{}' ]; do sleep 0.05; done\n",
        ready_marker.to_str().unwrap(),
        release_marker.to_str().unwrap()
    );
    let slow_bundle =
        build_operator_bundle_with_verify_and_validator(tmp.path(), 707, "    exit 0", &hook);
    let fast_bundle = build_operator_bundle(&tmp.path().join("fast"), 707);
    let cfg = tmp.path().join("operator.yaml");
    write_operator_config(&cfg, 707);
    let full_cfg = tmp.path().join("fullnode.yaml");
    write_full_node_config(&full_cfg, 707);

    let shared_dir = tmp.path().join("parallel");
    fs::create_dir_all(&shared_dir).unwrap();
    let stale_dir = shared_dir.join(".outbe-disk-stale-orphan");
    fs::create_dir_all(&stale_dir).unwrap();

    // Start first prepare (blocks inside fake outbe-keygen holding live .outbe-prepare-* and
    // .outbe-disk-joining-* staging dirs).
    let first_img = shared_dir.join("first.img");
    let first_child = Command::new(OUTBE_UP_BIN)
        .args(["disk", "prepare", "--role", "joining-validator", "--bundle"])
        .arg(&slow_bundle)
        .arg("--config")
        .arg(&cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&first_img)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready_marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for first prepare to enter staging"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let live_dirs_before: Vec<PathBuf> = fs::read_dir(&shared_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            (name.starts_with(".outbe-prepare-") || name.starts_with(".outbe-disk-joining-"))
                && p.is_dir()
        })
        .collect();
    assert_eq!(
        live_dirs_before.len(),
        2,
        "expected live .outbe-prepare-* and .outbe-disk-joining-* dirs, found: {live_dirs_before:?}"
    );

    // Run a SECOND concurrent `disk prepare --cleanup-stale` targeting the SAME directory.
    let second_img = shared_dir.join("second.img");
    let second_out = Command::new(OUTBE_UP_BIN)
        .args([
            "disk",
            "prepare",
            "--role",
            "full-node",
            "--cleanup-stale",
            "--bundle",
        ])
        .arg(&fast_bundle)
        .arg("--config")
        .arg(&full_cfg)
        .args(["--size-gib", "1", "--output"])
        .arg(&second_img)
        .output()
        .unwrap();
    assert!(
        second_out.status.success(),
        "second prepare --cleanup-stale failed:\n{}",
        String::from_utf8_lossy(&second_out.stderr)
    );

    // Truly stale orphan MUST have been removed, while BOTH live staging dirs MUST remain intact!
    assert!(
        !stale_dir.exists(),
        "--cleanup-stale must remove unlocked stale staging dir"
    );
    for live_dir in &live_dirs_before {
        assert!(
            live_dir.is_dir(),
            "--cleanup-stale deleted a live concurrent staging dir: {}",
            live_dir.display()
        );
    }

    // Release the first prepare and assert it completes cleanly and builds its image!
    fs::write(&release_marker, b"go").unwrap();
    let first_out = first_child.wait_with_output().unwrap();
    assert!(
        first_out.status.success(),
        "first prepare failed after concurrent --cleanup-stale:\n{}",
        String::from_utf8_lossy(&first_out.stderr)
    );
    assert!(first_img.is_file());
}
