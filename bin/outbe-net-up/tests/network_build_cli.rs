use std::{fs, path::Path, process::Command};

use serde_json::Value;

const OUTBE_NET_UP_BIN: &str = env!("CARGO_BIN_EXE_outbe-net-up");
const REAL_S5_BUNDLE: &str = "/home/ubuntu/outbe-vm-work/s5-bundle/outbe-software-bundle-v1-671c5c9d1b61ab2acb9619d97fc340379caa8c9d.tar.zst";

#[test]
fn config_example_and_update_placeholder_work_via_cli() {
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

    let upd = Command::new(OUTBE_NET_UP_BIN)
        .args([
            "network",
            "update",
            "--artifacts",
            "/tmp/fake-artifacts",
            "--image",
            "outbe-vm-dev-0123456789abcdef",
        ])
        .output()
        .unwrap();
    assert!(!upd.status.success());
    let stderr = String::from_utf8_lossy(&upd.stderr);
    assert!(
        stderr.contains("not yet implemented") && stderr.contains("S7"),
        "{stderr}"
    );
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
    host: 162.19.107.121
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
                    "  - name: validator-{i}\n    advertise_ip: 10.77.0.{}\n    rpc_bind_ip: 10.77.0.{}\n    hyperlane_public_url: http://10.77.0.{}:9000\n    vm:\n      bridge_ip: 10.77.0.{}",
                    68 + i,
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
    host: 162.19.107.121
founders:
  - name: validator-0
    advertise_ip: 162.19.107.121
    rpc_bind_ip: 10.77.0.80
    hyperlane_public_url: http://162.19.107.121:9000
    vm:
      bridge_ip: 10.77.0.80
      port_offset: 0
  - name: validator-1
    advertise_ip: 162.19.107.121
    rpc_bind_ip: 10.77.0.81
    hyperlane_public_url: http://162.19.107.121:9001
    vm:
      bridge_ip: 10.77.0.81
      port_offset: 1
  - name: validator-2
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.80
    hyperlane_public_url: http://141.95.168.110:9000
    vm:
      bridge_ip: 10.77.0.80
      port_offset: 0
      vmm_host:
        host: 141.95.168.110
  - name: validator-3
    advertise_ip: 141.95.168.110
    rpc_bind_ip: 10.77.0.81
    hyperlane_public_url: http://141.95.168.110:9001
    vm:
      bridge_ip: 10.77.0.81
      port_offset: 1
      vmm_host:
        host: 141.95.168.110
hyperlane:
  gasless: true
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

    let bootnodes = fs::read_to_string(testnet_out.join("reth-bootnodes.txt")).unwrap();
    assert!(bootnodes.contains("@162.19.107.121:30303"), "{bootnodes}");
    assert!(bootnodes.contains("@162.19.107.121:30304"), "{bootnodes}");
    assert!(bootnodes.contains("@141.95.168.110:30303"), "{bootnodes}");
    assert!(bootnodes.contains("@141.95.168.110:30304"), "{bootnodes}");

    let env0 = fs::read_to_string(testnet_out.join("vm/validator-0/validator.env")).unwrap();
    assert!(env0.contains("162.19.107.121:30400"), "{env0}");
    assert!(env0.contains("162.19.107.121:30401"), "{env0}");
    assert!(env0.contains("141.95.168.110:30400"), "{env0}");
    assert!(env0.contains("141.95.168.110:30401"), "{env0}");

    let feeder0 =
        fs::read_to_string(testnet_out.join("vm/validator-0/feeder-public.toml")).unwrap();
    assert!(
        feeder0.contains("https://prc.testnet.outbe.net"),
        "{feeder0}"
    );
}
