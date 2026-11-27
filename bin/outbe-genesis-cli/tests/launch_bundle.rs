//! Launch bundle rendering, systemd units, Caddyfile, feeder configs, and `.tgz` archive tests.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use alloy_primitives::B256;
use flate2::read::GzDecoder;
use outbe_genesis_cli::launch_bundle::{
    feeder_config, feeder_pairs, radicle_script, render, write_systemd_units,
    DEFAULT_MAX_VALIDATORS,
};
use outbe_genesis_cli::network::profiles_dir;
use outbe_genesis_cli::stages::build_seed;
use outbe_primitives::tee_genesis_v1::{
    initial_tee_policy_v1, tee_attestation_v1_genesis_field, InitialTeeProfileV1,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn minimal_config(keys_dir: &str) -> Value {
    let crs_path = Path::new(keys_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("bn254_g1.dat");
    json!({
        "chain_id": 424242,
        "validators": ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"],
        "keys_dir": keys_dir,
        "crs_path": crs_path,
        "tee": {"mode": "no-attestation-dev"},
        "hyperlane_binary": "/opt/outbe-chain/outbe-hyperlane-validator",
        "hyperlane_signer_funding_wei": "1000000000000000000",
        "hyperlane_public_urls": [
            "http://10.0.0.1:9000",
            "http://10.0.0.2:9000",
            "http://10.0.0.3:9000",
            "http://10.0.0.4:9000"
        ],
        "hyperlane_origins": [{
            "domain": 424242,
            "rpc_urls": ["http://10.0.0.1:8545"],
            "rpc_consensus": "single",
            "reorg_period": 0,
            "interval_secs": 15
        }],
    })
}

fn merge_obj(mut base: Value, extra: Value) -> Value {
    let base_obj = base.as_object_mut().expect("base object");
    for (k, v) in extra.as_object().expect("extra object") {
        base_obj.insert(k.clone(), v.clone());
    }
    base
}

fn fake_validators() -> Vec<Value> {
    (0..4)
        .map(|index| {
            json!({
                "address": format!("0x{}", (index + 1).to_string().repeat(40)),
                "hyperlane_address": format!("0x{}", (index + 5).to_string().repeat(40)),
                "public_key": format!("{:02x}", index + 1).repeat(48),
                "radicle_node_id": format!("0x{}", format!("{:02x}", index + 9).repeat(32)),
                "p2p_address": format!("10.0.0.{}:30400", index + 1),
            })
        })
        .collect()
}

fn render_fixture(root: &Path, overrides: Option<Value>) -> (PathBuf, PathBuf) {
    let keys_dir = root.join("keys");
    let output_dir = root.join("net");
    fs::create_dir_all(&output_dir).expect("mkdir net");
    fs::write(root.join("bn254_g1.dat"), b"fixture CRS").expect("write CRS");
    for index in 0..4 {
        let dir = keys_dir.join(format!("validator-{index}"));
        fs::create_dir_all(&dir).expect("mkdir validator");
        fs::write(dir.join("evm-key.hex"), format!("{:064x}\n", index + 1)).expect("write evm key");
        fs::write(dir.join("feeder-key.hex"), format!("{:064x}\n", index + 11))
            .expect("write feeder key");
        fs::write(
            dir.join("hyperlane-key.hex"),
            format!("{:064x}\n", index + 21),
        )
        .expect("write Hyperlane key");
    }
    let genesis = output_dir.join("genesis.json");
    let tee_policy = initial_tee_policy_v1(
        InitialTeeProfileV1::NoAttestationDev,
        424242,
        B256::repeat_byte(0x42),
    )
    .expect("dev tee policy");
    let genesis_json = json!({
        "config": {
            "chainId": 424242,
            "teeAttestationV1": tee_attestation_v1_genesis_field(&tee_policy)
                .expect("tee genesis field"),
        }
    });
    fs::write(&genesis, serde_json::to_vec(&genesis_json).unwrap()).expect("write genesis");
    let mut config = minimal_config(&keys_dir.display().to_string());
    if let Some(extra) = overrides {
        config = merge_obj(config, extra);
    }
    render(&config, &fake_validators(), &genesis, &keys_dir).expect("render launch bundle");
    (keys_dir, output_dir)
}

#[test]
fn radicle_sidecar_script_ceiling_and_no_config_file() {
    let default_script =
        radicle_script(&minimal_config("/keys"), 0, "10.0.0.1", "/keys").expect("radicle_script");
    assert!(default_script.contains(&format!("--max-validators {DEFAULT_MAX_VALIDATORS}")));
    assert!(!default_script.contains("--max-validators 4 "));
    assert!(!default_script.contains("config.json"));

    let custom_script = radicle_script(
        &merge_obj(
            minimal_config("/keys"),
            json!({"validator_set": {"max_validators": 64}}),
        ),
        0,
        "10.0.0.1",
        "/keys",
    )
    .expect("custom radicle_script");
    assert!(custom_script.contains("--max-validators 64"));
}

#[test]
fn render_writes_valid_bash_scripts_and_stable_bootnodes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (keys_dir, output_dir) = render_fixture(tmp.path(), None);

    assert!(output_dir.join("reth-bootnodes.txt").is_file());
    assert!(output_dir.join("DEPLOY.md").is_file());
    assert!(output_dir.join(".bb-crs/bn254_g1.dat").is_file());

    let removed_unit = format!("systemd/{}-{}.service", "outbe", "enclave");
    let removed_service = format!("{}-{}", "outbe", "enclave.service");
    for index in 0..4 {
        let directory = output_dir.join(format!("validator-{index}"));
        for removed in [
            "run-enclave.sh",
            "run-tee-renew.sh",
            removed_unit.as_str(),
            "systemd/outbe-tee-renew.service",
            "systemd/outbe-tee-renew.timer",
        ] {
            assert!(
                !directory.join(removed).exists(),
                "{removed} must not be generated"
            );
        }
        assert!(directory.join("reth-bootnodes.txt").is_file());
        for name in [
            "run-radicle.sh",
            "run-node.sh",
            "run-feeder.sh",
            "run-hyperlane.sh",
            "configure-hyperlane.sh",
            "install-systemd.sh",
        ] {
            let script = directory.join(name);
            assert!(script.is_file(), "{name} missing");
            let mode = fs::metadata(&script).expect("meta").permissions().mode();
            assert!(mode & 0o111 != 0, "{name} not executable");
            let status = Command::new("bash")
                .arg("-n")
                .arg(&script)
                .status()
                .expect("bash -n");
            assert!(status.success(), "bash -n failed for {name}");
        }
        for entry in walkdir::WalkDir::new(&directory) {
            let entry = entry.expect("bundle entry");
            if entry.file_type().is_file() {
                let content = fs::read_to_string(entry.path()).expect("bundle text");
                for removed in [
                    "--tee-enclave-socket",
                    "--tee-session-mode",
                    "aesmd.service",
                    "/dev/sgx_enclave",
                    "outbe-tee-enclave",
                    removed_service.as_str(),
                    "outbe-tee-renew",
                    "/var/lib/outbe/tee",
                    "/opt/outbe/sgx",
                ] {
                    assert!(
                        !content.contains(removed),
                        "{}: {removed}",
                        entry.path().display()
                    );
                }
            }
        }
        assert!(directory.join("feeder.toml").is_file());
        let hyperlane =
            fs::read_to_string(directory.join("hyperlane-public.toml")).expect("Hyperlane config");
        assert!(hyperlane.contains("bind_address = \"0.0.0.0:9000\""));
        assert!(!hyperlane.contains("private_key"));
        let hyperlane: toml::Value = toml::from_str(&hyperlane).expect("valid Hyperlane TOML");
        assert_eq!(
            hyperlane["hyperlane"]["poll_interval_secs"].as_integer(),
            Some(30)
        );
        assert_eq!(hyperlane["origin"][0]["domain"].as_integer(), Some(424_242));
        let feeder = fs::read_to_string(directory.join("feeder.toml")).expect("feeder config");
        assert!(feeder.contains(&format!("private_key = \"0x{:064x}\"", index + 11)));
        assert!(!feeder.contains(&format!("private_key = \"0x{:064x}\"", index + 1)));
        assert!(directory.join("systemd/outbe-validator.target").is_file());
        let node = fs::read_to_string(directory.join("run-node.sh")).expect("node script");
        assert!(node.contains("--tee-bootstrap-timeout-secs 300"));
        assert!(node.contains(&format!("/validator-{index}/reth-bootnodes.txt")));
    }

    let first_bootnodes =
        fs::read_to_string(output_dir.join("reth-bootnodes.txt")).expect("read bootnodes");
    render(
        &minimal_config(&keys_dir.display().to_string()),
        &fake_validators(),
        &output_dir.join("genesis.json"),
        &keys_dir,
    )
    .expect("re-render");
    let second_bootnodes =
        fs::read_to_string(output_dir.join("reth-bootnodes.txt")).expect("read bootnodes");
    assert_eq!(first_bootnodes, second_bootnodes);
    let lines: Vec<&str> = first_bootnodes.trim().lines().collect();
    assert_eq!(lines.len(), 4);
    for (line, validator) in lines.iter().zip(fake_validators().iter()) {
        let host = validator["p2p_address"]
            .as_str()
            .expect("p2p")
            .split(':')
            .next()
            .expect("host");
        assert!(line.starts_with("enode://"));
        assert!(line.ends_with(&format!("@{host}:30303")));
    }
}

#[test]
fn feeder_pairs_and_config_templates() {
    let cfg = json!({"feeder_pairs": "testnet/pairs.toml"});
    let pairs = &build_seed(&cfg).expect("build_seed")["oracle"]["pairs"];
    let mut quotes: Vec<String> = pairs
        .as_array()
        .expect("pairs")
        .iter()
        .map(|p| match &p["quote"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    quotes.sort();
    let mut expected_quotes = vec!["840", "978", "392", "826", "156", "344"];
    expected_quotes.sort_unstable();
    assert_eq!(quotes, expected_quotes);
    assert_eq!(pairs[0]["initial_rate"], json!("1000000"));

    let tmp = tempfile::tempdir().expect("tempdir");
    let (_, output_dir) = render_fixture(tmp.path(), Some(cfg));
    let dir0 = output_dir.join("validator-0");
    assert_eq!(
        fs::read_to_string(dir0.join("pairs.toml")).expect("read pairs.toml"),
        fs::read_to_string(profiles_dir().join("testnet/pairs.toml"))
            .expect("read committed pairs")
    );
    let feeder_toml = fs::read_to_string(dir0.join("feeder.toml")).expect("read feeder.toml");
    assert!(feeder_toml.contains("[rpc_endpoints]\n1 = \"REPLACE_WITH_RPC_URL\""));
    assert!(feeder_toml.contains("name = \"pyth\""));

    let binance_cfg = merge_obj(
        minimal_config("/keys"),
        json!({
            "price_provider": "binance",
            "price_feed_websocket": "wss://stream.binance.com:9443/ws",
        }),
    );
    let binance_toml = feeder_config(
        &binance_cfg,
        0,
        &json!({"address": format!("0x{}", "11".repeat(20))}),
        &"22".repeat(32),
    )
    .expect("feeder_config");
    assert!(binance_toml.contains("name = \"binance\""));
    assert!(binance_toml.contains("websocket = \"wss://stream.binance.com:9443/ws\""));
    assert!(feeder_pairs(&binance_cfg)
        .expect("feeder_pairs")
        .contains("markets = [{ provider = \"binance\", quote = \"USDT\" }]"));
    assert!(!binance_toml.contains("chain_denom"));
    assert!(!binance_toml.contains("rest ="));

    let mock_cfg = merge_obj(
        minimal_config("/keys"),
        json!({
            "feeder_pairs": "devnet/pairs.toml",
            "price_provider": "mock",
        }),
    );
    let mock_toml = feeder_config(
        &mock_cfg,
        0,
        &json!({"address": format!("0x{}", "11".repeat(20))}),
        &"22".repeat(32),
    )
    .expect("mock feeder_config");
    assert!(!mock_toml.contains("[[api_providers]]"));
    assert!(!mock_toml.contains("rest ="));
    assert!(feeder_pairs(&mock_cfg)
        .expect("mock feeder_pairs")
        .contains("provider = \"mock\""));
}

#[test]
fn distribution_archives_exclude_removed_sidecar() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_, output_dir) = render_fixture(tmp.path(), None);
    assert!(!output_dir.join("sgx").exists());

    let dist = output_dir.join("dist");
    assert!(dist.join("SHA256SUMS").is_file());
    let unpack_mode = fs::metadata(dist.join("unpack.sh"))
        .expect("unpack meta")
        .permissions()
        .mode();
    assert!(unpack_mode & 0o111 != 0);
    let unpack = fs::read_to_string(dist.join("unpack.sh")).expect("read unpack");
    assert!(unpack.contains("if [ \"$(id -u)\" -eq 0 ]"));
    assert!(unpack.contains("as_root mkdir"));
    assert!(!unpack.contains("\nsudo "));

    for index in 0..4 {
        let archive_path = dist.join(format!("validator-{index}.tgz"));
        assert!(archive_path.is_file());
        let file = File::open(&archive_path).expect("open archive");
        let gz = GzDecoder::new(file);
        let mut tar = tar::Archive::new(gz);
        let names: Vec<String> = tar
            .entries()
            .expect("entries")
            .map(|e| {
                e.expect("entry")
                    .path()
                    .expect("path")
                    .display()
                    .to_string()
            })
            .collect();
        let joined = names.join("\n");
        for required in [
            "genesis.json",
            "reth-bootnodes.txt",
            &format!("validator-{index}"),
            &format!("validator-{index}/systemd"),
            &format!("validator-{index}/install-systemd.sh"),
        ] {
            assert!(
                joined.contains(required),
                "{required} missing from archive {index}"
            );
        }
        assert!(joined.contains(&format!("keys/validator-{index}")));
        let removed_service = format!("{}-{}", "outbe", "enclave.service");
        for removed in [
            "sgx/",
            "outbe-tee-enclave",
            "run-enclave.sh",
            "run-tee-renew.sh",
            removed_service.as_str(),
            "outbe-tee-renew.service",
            "outbe-tee-renew.timer",
        ] {
            assert!(!joined.contains(removed), "{removed} must not be packaged");
        }
        assert!(joined.contains(".bb-crs/bn254_g1.dat"));
        for other in 0..4 {
            if other != index {
                assert!(!joined.contains(&format!("keys/validator-{other}")));
            }
        }
    }

    for line in fs::read_to_string(dist.join("SHA256SUMS"))
        .expect("read sums")
        .lines()
    {
        let (digest, name) = line.split_once("  ").expect("sum line");
        let bytes = fs::read(dist.join(name)).expect("read archive");
        assert_eq!(hex::encode(Sha256::digest(&bytes)), digest);
    }
}

#[test]
fn caddy_systemd_units_and_resident_tee() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_, output_dir) = render_fixture(tmp.path(), None);

    let caddy =
        fs::read_to_string(output_dir.join("validator-0").join("Caddyfile")).expect("caddy");
    assert!(caddy.contains("reverse_proxy 127.0.0.1:8545"));
    assert!(caddy.contains("reverse_proxy 127.0.0.1:8876"));
    assert!(!caddy.contains("8776"));
    assert!(caddy.contains("auto_https off"));

    let unit_dir = output_dir.join("validator-2/systemd");
    for (role, script, dep) in [
        ("radicle", "run-radicle.sh", None),
        ("validator", "run-node.sh", Some("outbe-radicle.service")),
        ("feeder", "run-feeder.sh", Some("outbe-validator.service")),
    ] {
        let unit =
            fs::read_to_string(unit_dir.join(format!("outbe-{role}.service"))).expect("read unit");
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("RestartMode=direct"));
        assert!(unit.contains("StartLimitIntervalSec=0"));
        assert!(unit.contains(script));
        assert!(unit.contains("ProtectSystem=strict"));
        assert!(!unit.contains("%i"));
        assert!(unit.contains("User=outbe"));
        assert!(unit.contains("Group=outbe"));
        if let Some(d) = dep {
            assert!(unit.contains(&format!("Wants=network-online.target {d}")));
        } else {
            assert!(unit.contains("Wants=network-online.target"));
        }
    }

    let install_systemd = fs::read_to_string(output_dir.join("validator-2/install-systemd.sh"))
        .expect("install-systemd");
    assert!(install_systemd.contains("if [ \"$(id -u)\" -eq 0 ]"));
    assert!(install_systemd.contains("as_root systemctl"));
    assert!(!install_systemd.contains("\nsudo "));
    assert!(!install_systemd.contains("outbe-tee-renew"));
    assert!(install_systemd.contains("enable --now outbe-validator.target"));
    assert!(install_systemd.contains("systemctl stop outbe-validator.target"));
    assert!(install_systemd
        .contains("rm -f /etc/systemd/system/multi-user.target.wants/outbe-validator.target"));
    assert!(install_systemd.contains(&format!("SYSTEMD_DIR={}/systemd", output_dir.display())));
    assert!(install_systemd.contains(&format!("STATE_DIR={}/state", output_dir.display())));
    assert!(install_systemd.contains("$STATE_DIR/feeder"));
    assert!(install_systemd.contains("$STATE_DIR/hyperlane"));
    assert!(install_systemd.contains("ln -sfn \"$SYSTEMD_DIR/$name\""));
    assert!(!install_systemd.contains("install -m 0644 \"$UNIT_DIR\"/outbe-* /etc/systemd/system/"));
    assert!(!install_systemd.contains("founding validator services:"));

    let install_caddy =
        fs::read_to_string(output_dir.join("validator-2/install-caddy.sh")).expect("install-caddy");
    assert!(install_caddy.contains("as_root apt-get"));
    assert!(install_caddy.contains("as_root systemctl"));
    assert!(!install_caddy.contains("\nsudo "));

    let node2 =
        fs::read_to_string(output_dir.join("validator-2").join("run-node.sh")).expect("node2");
    assert!(node2.contains("--http.api eth,net,web3,outbe,fsEpoch"));
    assert!(node2.contains("--nat extip:10.0.0.3"));
    assert!(node2.contains("--consensus.storage-dir"));
    assert!(!node2.contains("--consensus.use-local-defaults"));
    assert!(!node2.contains("--tee-session-mode"));

    let tmp_private = tempfile::tempdir().expect("tmp_private");
    let (_, out_private) = render_fixture(
        tmp_private.path(),
        Some(json!({"consensus_use_local_defaults": true})),
    );
    let node_private =
        fs::read_to_string(out_private.join("validator-2/run-node.sh")).expect("node_private");
    assert!(node_private.contains("--consensus.use-local-defaults"));

    let tmp_rpc = tempfile::tempdir().expect("tmp_rpc");
    let (_, out_rpc) = render_fixture(tmp_rpc.path(), Some(json!({"rpc_bind_ip": "validator"})));
    let rpc_dir = out_rpc.join("validator-2");
    assert!(fs::read_to_string(rpc_dir.join("run-node.sh"))
        .expect("rpc node")
        .contains("--http.addr 10.0.0.3"));
    assert!(fs::read_to_string(rpc_dir.join("feeder.toml"))
        .expect("rpc feeder")
        .contains("rpc_endpoint = \"http://10.0.0.3:8545\""));
    assert!(fs::read_to_string(rpc_dir.join("Caddyfile"))
        .expect("rpc caddy")
        .contains("reverse_proxy 10.0.0.3:8545"));
}

#[test]
fn per_validator_runtime_configures_nat_ports_and_private_rpc() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let runtime = json!({
        "validator_runtime": [
            {
                "rpc_bind_ip": "10.77.0.11",
                "reth_p2p_port": 30303,
                "reth_discv5_port": 31303,
                "radicle_port": 8776,
                "consensus_p2p_port": 30400
            },
            {
                "rpc_bind_ip": "10.77.0.12",
                "reth_p2p_port": 30304,
                "reth_discv5_port": 31304,
                "radicle_port": 8777,
                "consensus_p2p_port": 30401
            },
            {
                "rpc_bind_ip": "10.77.0.13",
                "reth_p2p_port": 30305,
                "reth_discv5_port": 31305,
                "radicle_port": 8778,
                "consensus_p2p_port": 30402
            },
            {
                "rpc_bind_ip": "10.77.0.14",
                "reth_p2p_port": 30306,
                "reth_discv5_port": 31306,
                "radicle_port": 8779,
                "consensus_p2p_port": 30403
            }
        ]
    });
    let (_, output) = render_fixture(tmp.path(), Some(runtime));
    let node = fs::read_to_string(output.join("validator-1/run-node.sh")).expect("node");
    assert!(node.contains("--http.addr 10.77.0.12 --http.port 8545"));
    assert!(node.contains("--port 30304"));
    assert!(node.contains("--discovery.v5.port 31304"));
    let radicle = fs::read_to_string(output.join("validator-1/run-radicle.sh")).expect("radicle");
    assert!(radicle.contains("--listen 0.0.0.0:8777"));
    assert!(radicle.contains("--advertise 10.0.0.2:8777"));
    let bootnodes = fs::read_to_string(output.join("reth-bootnodes.txt")).expect("bootnodes");
    assert!(bootnodes
        .lines()
        .nth(1)
        .is_some_and(|line| line.ends_with(":30304")));
}

#[test]
fn founding_installer_prepares_without_starting_services() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    write_systemd_units(root, "/opt/outbe", 0).expect("generate systemd installer");
    let stub = root.join("stub.bash");
    fs::write(
        &stub,
        r#"dirname() { printf '%s\n' "${2%/*}"; }
getent() { return 0; }
id() { printf '0\n'; }
chown() { return 0; }
install() { return 0; }
ln() { return 0; }
rm() { return 0; }
systemctl() { printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"; }
"#,
    )
    .expect("write shell command stubs");
    let log = root.join("systemctl.log");
    for (args, expected) in [
        (&["--prepare-only"][..], "stop outbe-validator.target"),
        (&[][..], "enable --now outbe-validator.target"),
    ] {
        fs::write(&log, "").expect("clear systemctl log");
        let output = Command::new("/bin/bash")
            .arg(root.join("install-systemd.sh"))
            .args(args)
            .env_clear()
            .env("PATH", root)
            .env("BASH_ENV", &stub)
            .env("SYSTEMCTL_LOG", &log)
            .output()
            .expect("run generated installer");
        assert!(output.status.success(), "{output:?}");
        let calls = fs::read_to_string(&log).expect("systemctl log");
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            ["daemon-reload", expected]
        );
    }
}
