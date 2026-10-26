//! Launch bundle rendering, systemd units, Caddyfile, feeder configs, and `.tgz` archive tests.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use outbe_genesis_cli::launch_bundle::{
    feeder_config, feeder_pairs, radicle_script, render, DEFAULT_MAX_VALIDATORS,
    SIGNED_ENCLAVE_FILES,
};
use outbe_genesis_cli::network::profiles_dir;
use outbe_genesis_cli::stages::build_seed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn minimal_config(keys_dir: &str) -> Value {
    json!({
        "chain_id": 424242,
        "validators": ["10.0.0.1", "10.0.0.2", "10.0.0.3", "10.0.0.4"],
        "keys_dir": keys_dir,
        "tee": {"mode": "gramine-direct-dev"},
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
    for index in 0..4 {
        let dir = keys_dir.join(format!("validator-{index}"));
        fs::create_dir_all(&dir).expect("mkdir validator");
        fs::write(dir.join("evm-key.hex"), format!("{:064x}\n", index + 1)).expect("write evm key");
    }
    let genesis = output_dir.join("genesis.json");
    fs::write(&genesis, r#"{"config":{"chainId":424242}}"#).expect("write genesis");
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

    for index in 0..4 {
        let directory = output_dir.join(format!("validator-{index}"));
        for name in [
            "run-enclave.sh",
            "run-radicle.sh",
            "run-node.sh",
            "run-feeder.sh",
            "run-tee-renew.sh",
            "start-all.sh",
            "stop-all.sh",
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
        assert!(directory.join("feeder.toml").is_file());
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
}

#[test]
fn distribution_archives_and_signed_enclave_staging() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let signed_dir = tmp.path().join("signed");
    fs::create_dir_all(&signed_dir).expect("mkdir signed");
    for &name in SIGNED_ENCLAVE_FILES {
        fs::write(signed_dir.join(name), b"x").expect("write enclave file");
    }
    fs::write(signed_dir.join("enclave-key.pem"), b"PRIVATE").expect("write pem");

    let (_, output_dir) = render_fixture(
        tmp.path(),
        Some(json!({"signed_enclave_dir": signed_dir.display().to_string()})),
    );

    let staged: BTreeSet<String> = fs::read_dir(output_dir.join("enclave"))
        .expect("read enclave dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    let expected_staged: BTreeSet<String> =
        SIGNED_ENCLAVE_FILES.iter().map(|&s| s.to_owned()).collect();
    assert_eq!(staged, expected_staged);
    assert!(!staged.contains("enclave-key.pem"));

    let dist = output_dir.join("dist");
    assert!(dist.join("SHA256SUMS").is_file());
    let unpack_mode = fs::metadata(dist.join("unpack.sh"))
        .expect("unpack meta")
        .permissions()
        .mode();
    assert!(unpack_mode & 0o111 != 0);

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
            "systemd",
            "install-systemd.sh",
        ] {
            assert!(
                joined.contains(required),
                "{required} missing from archive {index}"
            );
        }
        assert!(joined.contains(&format!("keys/validator-{index}")));
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
fn caddy_systemd_units_tee_renewal_and_enclave_modes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_, output_dir) = render_fixture(tmp.path(), None);

    let caddy =
        fs::read_to_string(output_dir.join("validator-0").join("Caddyfile")).expect("caddy");
    assert!(caddy.contains("reverse_proxy 127.0.0.1:8545"));
    assert!(caddy.contains("reverse_proxy 127.0.0.1:8876"));
    assert!(!caddy.contains("8776"));
    assert!(caddy.contains("auto_https off"));

    let unit_dir = output_dir.join("systemd");
    for (role, dep) in [
        ("enclave", None),
        ("radicle", Some("outbe-enclave@%i.service")),
        ("node", Some("outbe-radicle@%i.service")),
        ("feeder", Some("outbe-node@%i.service")),
    ] {
        let unit =
            fs::read_to_string(unit_dir.join(format!("outbe-{role}@.service"))).expect("read unit");
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("StartLimitIntervalSec=0"));
        assert!(unit.contains(&format!("run-{role}.sh")));
        if let Some(d) = dep {
            assert!(unit.contains(&format!("Wants=network-online.target {d}")));
        } else {
            assert!(unit.contains("Wants=network-online.target"));
            assert!(unit.contains("User=root"));
        }
    }

    let timer =
        fs::read_to_string(unit_dir.join("outbe-tee-renew@.timer")).expect("read renew timer");
    assert!(timer.contains("OnCalendar=daily"));
    assert!(timer.contains("Persistent=true"));
    let install_systemd =
        fs::read_to_string(output_dir.join("install-systemd.sh")).expect("install-systemd");
    assert!(install_systemd.contains("enable --now \"outbe-tee-renew@$INDEX.timer\""));

    let node2 =
        fs::read_to_string(output_dir.join("validator-2").join("run-node.sh")).expect("node2");
    assert!(node2.contains("--nat extip:10.0.0.3"));
    assert!(node2.contains("--consensus.storage-dir"));
    assert!(node2.contains("--tee-session-mode production-node-host"));

    let enclave0 = fs::read_to_string(output_dir.join("validator-0").join("run-enclave.sh"))
        .expect("enclave0");
    assert!(enclave0.contains("--dkg-seed"));
    assert!(enclave0.contains("unattested"));

    // Mock enclave mode (`enclave_sgx: false`)
    let tmp_mock = tempfile::tempdir().expect("tmp_mock");
    let (_, out_mock) = render_fixture(tmp_mock.path(), Some(json!({"enclave_sgx": false})));
    let node_mock =
        fs::read_to_string(out_mock.join("validator-0").join("run-node.sh")).expect("node_mock");
    assert!(!node_mock.contains("--tee-session-mode"));
    let install_mock =
        fs::read_to_string(out_mock.join("install-systemd.sh")).expect("install_mock");
    assert!(!install_mock.contains("enable --now \"outbe-tee-renew@$INDEX.timer\""));

    // Production DCAP mode
    let tmp_dcap = tempfile::tempdir().expect("tmp_dcap");
    let (_, out_dcap) = render_fixture(
        tmp_dcap.path(),
        Some(json!({
            "chain_id": 54322345,
            "remote_base_dir": "/opt/outbe",
            "remote_keys_dir": "/opt/outbe/keys",
            "tee": {
                "mode": "dcap-required",
                "mrenclave": "ab".repeat(32),
                "mrsigner": "cd".repeat(32),
                "isv_prod_id": 0,
                "minimum_isv_svn": 3,
                "minimum_tcb_evaluation_data_number": 17,
            }
        })),
    );
    let enclave_dcap = fs::read_to_string(out_dcap.join("validator-0").join("run-enclave.sh"))
        .expect("enclave_dcap");
    assert!(!enclave_dcap.contains("docker run"));
    assert!(enclave_dcap.contains("/dev/sgx_enclave"));
    assert!(enclave_dcap.contains("/dev/sgx_provision"));
    assert!(enclave_dcap.contains("aesmd.service"));
    let node_dcap =
        fs::read_to_string(out_dcap.join("validator-1").join("run-node.sh")).expect("node_dcap");
    assert!(node_dcap.contains("/opt/outbe/genesis.json"));
    assert!(node_dcap.contains("/opt/outbe/keys/validator-1"));
    assert!(node_dcap.contains("--consensus.listen-addr 0.0.0.0:30400"));
    assert!(node_dcap.contains("--tee-session-mode production-node-host"));
}
