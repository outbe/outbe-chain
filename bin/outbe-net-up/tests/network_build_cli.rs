use std::{fs, path::Path, process::Command};

use serde_json::Value;

const OUTBE_NET_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-net-up");
const REAL_S5_BUNDLE: &str = "/home/ubuntu/outbe-vm-work/s5-bundle/outbe-software-bundle-v1-671c5c9d1b61ab2acb9619d97fc340379caa8c9d.tar.zst";

#[test]
fn config_example_and_update_cli_validation_work() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("network.yaml");

    let init = Command::new(OUTBE_NET_UP_BIN)
        .args(["network", "config-example", "--output"])
        .arg(&cfg_path)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "config-example failed:\n{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(cfg_path.is_file());

    // `network update` must require an image source (`--image-archive` or `--image-url`).
    let missing_img = Command::new(OUTBE_NET_UP_BIN)
        .args(["network", "update", "--artifacts", "/tmp/fake-artifacts"])
        .output()
        .unwrap();
    assert!(!missing_img.status.success());
    let stderr = String::from_utf8_lossy(&missing_img.stderr);
    assert!(
        stderr.contains("--image-archive") && stderr.contains("--image-url"),
        "{stderr}"
    );

    // `network update` must reject `--force` (strictly graceful shutdown only).
    let force_rej = Command::new(OUTBE_NET_UP_BIN)
        .args([
            "network",
            "update",
            "--artifacts",
            "/tmp/fake-artifacts",
            "--image-archive",
            "/tmp/fake-image.tar.zst",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(!force_rej.status.success());
    let stderr = String::from_utf8_lossy(&force_rej.stderr);
    assert!(stderr.contains("--force"), "{stderr}");

    // `network update` must reject `--dkg-guard-blocks 0` (F2: cannot disable DKG guard).
    let zero_guard = Command::new(OUTBE_NET_UP_BIN)
        .args([
            "network",
            "update",
            "--artifacts",
            "/tmp/fake-artifacts",
            "--image-archive",
            "/tmp/fake-image.tar.zst",
            "--dkg-guard-blocks",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!zero_guard.status.success());
    let stderr = String::from_utf8_lossy(&zero_guard.stderr);
    assert!(stderr.contains("dkg-guard-blocks"), "{stderr}");

    // `network update` must reject removed `--skip-dkg-boundary-wait`.
    let skip_rej = Command::new(OUTBE_NET_UP_BIN)
        .args([
            "network",
            "update",
            "--artifacts",
            "/tmp/fake-artifacts",
            "--image-archive",
            "/tmp/fake-image.tar.zst",
            "--skip-dkg-boundary-wait",
        ])
        .output()
        .unwrap();
    assert!(!skip_rej.status.success());
    let stderr = String::from_utf8_lossy(&skip_rej.stderr);
    assert!(stderr.contains("--skip-dkg-boundary-wait"), "{stderr}");
}

#[test]
fn network_build_with_real_s5_bundle_builds_vm_devnet_and_mixed_host_testnet() {
    let bundle = Path::new(REAL_S5_BUNDLE);
    if !bundle.is_file() {
        eprintln!(
            "skipping real-bundle build test: {} not present",
            bundle.display()
        );
        return;
    }

    let tmp = tempfile::tempdir().unwrap();

    // 1. Single-host 4-VM devnet build against the real software bundle.
    let devnet_cfg = tmp.path().join("devnet.yaml");
    fs::write(
        &devnet_cfg,
        format!(
            r#"
version: 1
name: s6-devnet
network: devnet
chain_id: 424242
certified_rpc: "http://10.77.0.68:8545"
vm:
  vmm_host:
    host: 198.51.100.10
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
founders:
{}
hyperlane:
  gasless: true
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#,
            (0..4)
                .map(|i| format!(
                    "  - name: validator-{i}\n    advertise_ip: 10.77.0.{}\n    hyperlane_public_url: http://10.77.0.{}:9000\n    vm:\n      vm_ip: 10.77.0.{}",
                    68 + i,
                    68 + i,
                    68 + i
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .unwrap();

    let devnet_out = tmp.path().join("devnet-artifacts");
    let build_dev = Command::new(OUTBE_NET_UP_BIN)
        .args(["network", "build", "--software-bundle"])
        .arg(bundle)
        .arg("--config")
        .arg(&devnet_cfg)
        .arg("--output")
        .arg(&devnet_out)
        .output()
        .unwrap();
    assert!(
        build_dev.status.success(),
        "real-bundle devnet build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_dev.stdout),
        String::from_utf8_lossy(&build_dev.stderr)
    );
    let dev_manifest: Value =
        serde_json::from_slice(&fs::read(devnet_out.join("network-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(dev_manifest["deployment"], "vm");
    assert_eq!(dev_manifest["network"], "devnet");
    assert_eq!(dev_manifest["chain_id"], 424242);
    assert_eq!(dev_manifest["founders"].as_array().unwrap().len(), 4);
    assert!(devnet_out.join("genesis.json").is_file());
    assert!(devnet_out
        .join("joining/outbe-validator-operator-bundle.tar.zst")
        .is_file());

    // 2. Mixed 2-host (AMD+Intel) 4-VM testnet build with per-host `port_offset` 0/1.
    // First verify build refuses a non-devnet VM config missing `feeder.rpc_endpoints`.
    let missing_feeder_cfg = tmp.path().join("testnet-missing-feeder.yaml");
    fs::write(
        &missing_feeder_cfg,
        r#"
version: 1
name: s6-mixed-testnet
network: testnet
chain_id: 54322345
certified_rpc: "http://10.77.0.80:8545"
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
      vm_ip: 10.77.0.80
      port_offset: 0
  - name: validator-1
    advertise_ip: 198.51.100.10
    hyperlane_public_url: http://198.51.100.10:9001
    vm:
      vm_ip: 10.77.0.81
      port_offset: 1
  - name: validator-2
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9000
    vm:
      vm_ip: 10.77.0.80
      port_offset: 0
      vmm_host:
        host: 203.0.113.20
  - name: validator-3
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9001
    vm:
      vm_ip: 10.77.0.81
      port_offset: 1
      vmm_host:
        host: 203.0.113.20
hyperlane:
  gasless: true
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {}
"#,
    )
    .unwrap();
    let bad_build = Command::new(OUTBE_NET_UP_BIN)
        .args(["network", "build", "--software-bundle"])
        .arg(bundle)
        .arg("--config")
        .arg(&missing_feeder_cfg)
        .arg("--output")
        .arg(tmp.path().join("testnet-missing-feeder-out"))
        .output()
        .unwrap();
    assert!(
        !bad_build.status.success(),
        "testnet VM build without feeder.rpc_endpoints must fail"
    );
    let bad_stderr = String::from_utf8_lossy(&bad_build.stderr);
    assert!(
        bad_stderr.contains("feeder.rpc_endpoints"),
        "expected missing feeder.rpc_endpoints error, got:\n{bad_stderr}"
    );

    let testnet_cfg = tmp.path().join("testnet.yaml");
    fs::write(
        &testnet_cfg,
        r#"
version: 1
name: s6-mixed-testnet
network: testnet
chain_id: 54322345
certified_rpc: "http://10.77.0.80:8545"
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
      vm_ip: 10.77.0.80
      port_offset: 0
  - name: validator-1
    advertise_ip: 198.51.100.10
    hyperlane_public_url: http://198.51.100.10:9001
    vm:
      vm_ip: 10.77.0.81
      port_offset: 1
  - name: validator-2
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9000
    vm:
      vm_ip: 10.77.0.80
      port_offset: 0
      vmm_host:
        host: 203.0.113.20
  - name: validator-3
    advertise_ip: 203.0.113.20
    hyperlane_public_url: http://203.0.113.20:9001
    vm:
      vm_ip: 10.77.0.81
      port_offset: 1
      vmm_host:
        host: 203.0.113.20
hyperlane:
  gasless: true
feeder:
  rpc_endpoints:
    1: "https://ethereum-rpc.example.invalid"
    97: "https://bsc-testnet-rpc.example.invalid"
    11155111: "https://sepolia-rpc.example.invalid"
  api_keys:
    pyth: "test-pyth-key-placeholder"
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {}
"#,
    )
    .unwrap();

    let testnet_out = tmp.path().join("testnet-artifacts");
    let build_test = Command::new(OUTBE_NET_UP_BIN)
        .args(["network", "build", "--software-bundle"])
        .arg(bundle)
        .arg("--config")
        .arg(&testnet_cfg)
        .arg("--output")
        .arg(&testnet_out)
        .output()
        .unwrap();
    assert!(
        build_test.status.success(),
        "real-bundle mixed-host testnet build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build_test.stdout),
        String::from_utf8_lossy(&build_test.stderr)
    );
    let test_manifest: Value =
        serde_json::from_slice(&fs::read(testnet_out.join("network-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(test_manifest["deployment"], "vm");
    assert_eq!(test_manifest["network"], "testnet");
    assert_eq!(test_manifest["chain_id"], 54322345);
    assert_eq!(test_manifest["founders"][0]["vm"]["port_offset"], 0);
    assert_eq!(test_manifest["founders"][1]["vm"]["port_offset"], 1);
    assert_eq!(test_manifest["founders"][2]["vm"]["port_offset"], 0);
    assert_eq!(test_manifest["founders"][3]["vm"]["port_offset"], 1);
    let fw = test_manifest["firewall_mappings"]
        .as_array()
        .expect("mixed-host manifest must include firewall_mappings");
    // Six mappings for each of the four founders. The Hyperlane port is the sixth.
    assert_eq!(fw.len(), 24);
    assert_eq!(fw[0]["member"], "validator-0");
    assert_eq!(fw[0]["host"], "198.51.100.10");
    assert_eq!(fw[0]["public_ip"], "198.51.100.10");
    assert_eq!(fw[0]["public_port"], 30303);
    assert_eq!(fw[0]["proto"], "tcp");
    assert_eq!(fw[0]["vm_ip"], "10.77.0.80");
    assert_eq!(fw[0]["vm_port"], 30303);
    assert_eq!(fw[0]["purpose"], "reth-p2p");
    let build_stdout = String::from_utf8_lossy(&build_test.stdout);
    assert!(
        build_stdout.contains("DNAT / firewall mappings:")
            && build_stdout.contains("consensus-p2p")
            && build_stdout.contains("reth-discovery"),
        "build stdout must print DNAT / firewall mappings table:\n{build_stdout}"
    );

    let bootnodes = fs::read_to_string(testnet_out.join("reth-bootnodes.txt")).unwrap();
    assert!(bootnodes.contains("@198.51.100.10:30303"), "{bootnodes}");
    assert!(bootnodes.contains("@198.51.100.10:30304"), "{bootnodes}");
    assert!(bootnodes.contains("@203.0.113.20:30303"), "{bootnodes}");
    assert!(bootnodes.contains("@203.0.113.20:30304"), "{bootnodes}");

    let env0 = fs::read_to_string(testnet_out.join("vm/validator-0/validator.env")).unwrap();
    assert!(
        env0.contains("OUTBE_EXTERNAL_IP=\"198.51.100.10\""),
        "{env0}"
    );
    assert!(env0.contains("OUTBE_RPC_BIND_IP=\"10.77.0.80\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_IP=\"10.77.0.80\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_PREFIX=\"24\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_GATEWAY=\"10.77.0.1\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\""), "{env0}");
    assert!(env0.contains("OUTBE_CONSENSUS_PORT=\"30400\""), "{env0}");
    assert!(env0.contains("OUTBE_RETH_P2P_PORT=\"30303\""), "{env0}");
    assert!(
        env0.contains("OUTBE_RETH_DISCOVERY_PORT=\"31303\""),
        "{env0}"
    );
    assert!(env0.contains("OUTBE_RADICLE_PORT=\"8776\""), "{env0}");
    assert!(env0.contains("198.51.100.10:30400"), "{env0}");
    assert!(env0.contains("198.51.100.10:30401"), "{env0}");
    assert!(env0.contains("203.0.113.20:30400"), "{env0}");
    assert!(env0.contains("203.0.113.20:30401"), "{env0}");

    let env1 = fs::read_to_string(testnet_out.join("vm/validator-1/validator.env")).unwrap();
    assert!(env1.contains("OUTBE_CONSENSUS_PORT=\"30401\""), "{env1}");
    assert!(env1.contains("OUTBE_RETH_P2P_PORT=\"30304\""), "{env1}");
    assert!(
        env1.contains("OUTBE_RETH_DISCOVERY_PORT=\"31304\""),
        "{env1}"
    );
    assert!(env1.contains("OUTBE_RADICLE_PORT=\"8777\""), "{env1}");

    let feeder0 =
        fs::read_to_string(testnet_out.join("vm/validator-0/feeder-public.toml")).unwrap();
    assert!(
        feeder0.contains("1 = \"https://ethereum-rpc.example.invalid\""),
        "{feeder0}"
    );
    assert!(
        !feeder0.contains("REPLACE_WITH_"),
        "VM feeder-public.toml must never contain REPLACE_WITH_ placeholders:\n{feeder0}"
    );
}
