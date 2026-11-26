use super::*;
use crate::network::{
    build_genesis::{ensure_base_genesis, read_hyperlane_admin_slot},
    build_joining::joining_operator_config,
    build_vm::{emit_vm_founder_trees, write_vm_bootnodes, VmTreeInputs},
};
use alloy_primitives::{Address, B256, U256};
use outbe_operator_node_role::{config::OperatorConfig, FEEDER_FUNDING_WEI};
use outbe_primitives::tee_genesis_v1::{
    initial_tee_policy_v1, tee_attestation_v1_genesis_field, InitialTeeProfileV1,
};
use serde_json::{json, Map};

fn feeder_keys(root: &Path, count: usize) -> (PathBuf, Vec<String>) {
    let keys = root.join("keys");
    let mut addresses = Vec::new();
    for index in 0..count {
        let dir = keys.join(format!("validator-{index}"));
        fs::create_dir_all(&dir).unwrap();
        let feeder_key = dir.join("feeder-key.hex");
        fs::write(&feeder_key, format!("{:064x}", index + 1)).unwrap();
        let evm_key = dir.join("evm-key.hex");
        fs::write(&evm_key, format!("{:064x}", index + 101)).unwrap();
        addresses.push(format!("{:#x}", validator_address(&feeder_key).unwrap()));
    }
    (keys, addresses)
}

#[test]
fn genesis_cli_config_carries_prefunds_and_runtime_axes() {
    let temp = tempfile::tempdir().unwrap();
    let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    let (keys, feeder_addresses) = feeder_keys(temp.path(), config.founders.len());
    let value = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
    assert_eq!(value["network"], "testnet");
    assert_eq!(value["tee"]["mode"], "no-attestation-dev");
    outbe_genesis_cli::network::validate_config(&value).unwrap();
    assert_eq!(
        value["balance"]["0x1111111111111111111111111111111111111111"],
        "1000000000000000000000"
    );
    for address in feeder_addresses {
        assert_eq!(value["balance"][address], FEEDER_FUNDING_WEI.to_string());
    }
}

#[test]
fn mainnet_rejects_generated_unattested_policy() {
    let temp = tempfile::tempdir().unwrap();
    let mut config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    config.network = Network::Mainnet;
    config.chain_id = Network::Mainnet.chain_id();
    config.validate().unwrap();
    let (keys, _) = feeder_keys(temp.path(), config.founders.len());
    let value = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
    let error = outbe_genesis_cli::network::validate_config(&value).unwrap_err();
    assert!(error.to_string().contains("TDX/SNP attestation"), "{error}");
}

#[test]
fn reads_genesis_block_hash_and_network_binding() {
    let hash = B256::repeat_byte(0x42);
    let policy =
        initial_tee_policy_v1(InitialTeeProfileV1::NoAttestationDev, 424_242, hash).unwrap();
    let genesis = json!({
        "config": {
            "teeAttestationV1": tee_attestation_v1_genesis_field(&policy).unwrap()
        }
    });
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(temp.path(), serde_json::to_vec(&genesis).unwrap()).unwrap();
    let (actual, binding) = genesis_identity(temp.path()).unwrap();
    assert_eq!(actual, format!("{hash:#x}"));
    assert_eq!(binding.genesis_hash, hash);
    assert_eq!(binding.chain_id, U256::from(424_242).to_be_bytes::<32>());
}

#[test]
fn vm_multi_host_port_offset_applies_to_bootnodes_and_consensus_peers() {
    let temp = tempfile::tempdir().unwrap();
    let software_root = temp.path().join("software");
    let staging_root = temp.path().join("staging");
    let keys_dir = staging_root.join("secrets/founder-keys");
    let fake_bins = temp.path().join("bins");
    fs::create_dir_all(&staging_root).unwrap();
    fs::create_dir_all(&fake_bins).unwrap();

    let fake_keygen = fake_bins.join("outbe-keygen");
    fs::write(&fake_keygen, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();

    let pairs = software_root.join("share/outbe-genesis/profiles/testnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock_http\" }]\n",
        )
        .unwrap();
    let crs = software_root.join(".bb-crs/bn254_g1.dat");
    fs::create_dir_all(crs.parent().unwrap()).unwrap();
    fs::write(&crs, b"crs-bytes").unwrap();
    fs::write(
        staging_root.join("genesis.json"),
        br#"{"config":{"chainId":54322345}}"#,
    )
    .unwrap();

    let config_path = temp.path().join("network.yaml");
    let config: NetworkConfig = serde_yaml_ng::from_str(
        r#"
version: 1
name: mixed-testnet
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
    rpc_bind_ip: 10.77.0.10
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
  use_local_defaults: true
genesis:
  prefunded_accounts: {}
"#,
    )
    .unwrap();
    config.validate().unwrap();

    let mut validators = Vec::new();
    for (index, founder) in config.founders.iter().enumerate() {
        let val_keys = keys_dir.join(format!("validator-{index}"));
        fs::create_dir_all(val_keys.join("radicle/keys")).unwrap();
        let key_hex = format!("{:064x}", index + 1);
        for file in [
            "signing-key.hex",
            "reth-p2p-secret.hex",
            "radicle/keys/radicle",
        ] {
            fs::write(val_keys.join(file), format!("{key_hex}\n")).unwrap();
        }
        for file in ["evm-key.hex", "feeder-key.hex", "hyperlane-key.hex"] {
            fs::write(val_keys.join(file), &key_hex).unwrap();
        }
        let addr = format!(
            "{:#x}",
            validator_address(&val_keys.join("evm-key.hex")).unwrap()
        );
        validators.push(json!({
            "address": addr,
            "hyperlane_address": addr,
            "public_key": format!("{:096x}", index + 1),
            "p2p_address": format!("{}:30400", founder.advertise_ip),
        }));
    }

    let bootnodes =
        write_vm_bootnodes(&json!({}), &config, &validators, &keys_dir, &staging_root).unwrap();
    assert!(
        bootnodes[0].ends_with("@198.51.100.10:30303"),
        "{bootnodes:?}"
    );
    assert!(
        bootnodes[1].ends_with("@198.51.100.10:30304"),
        "{bootnodes:?}"
    );
    assert!(
        bootnodes[2].ends_with("@203.0.113.20:30303"),
        "{bootnodes:?}"
    );
    assert!(
        bootnodes[3].ends_with("@203.0.113.20:30304"),
        "{bootnodes:?}"
    );

    let artifacts = emit_vm_founder_trees(&VmTreeInputs {
        staging_root: &staging_root,
        software_root: &software_root,
        keys_dir: &keys_dir,
        config_path: &config_path,
        config: &config,
        validators: &validators,
        bootnodes: &bootnodes,
    })
    .unwrap();
    assert_eq!(artifacts[0].vm.as_ref().unwrap().port_offset, 0);
    assert_eq!(artifacts[1].vm.as_ref().unwrap().port_offset, 1);
    assert_eq!(artifacts[2].vm.as_ref().unwrap().port_offset, 0);
    assert_eq!(artifacts[3].vm.as_ref().unwrap().port_offset, 1);

    let genesis_cfg = genesis_cli_config(&config, &software_root, &keys_dir, None).unwrap();
    assert_eq!(
        genesis_cfg["validators"],
        json!([
            "198.51.100.10",
            "198.51.100.10:30401",
            "203.0.113.20",
            "203.0.113.20:30401",
        ])
    );

    let env0 = fs::read_to_string(staging_root.join("vm/validator-0/validator.env")).unwrap();
    assert!(
        env0.contains("OUTBE_EXTERNAL_IP=\"198.51.100.10\""),
        "{env0}"
    );
    assert!(env0.contains("OUTBE_RPC_BIND_IP=\"10.77.0.10\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_IP=\"10.77.0.10\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_PREFIX=\"24\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_GATEWAY=\"10.77.0.1\""), "{env0}");
    assert!(env0.contains("OUTBE_VM_DNS=\"1.1.1.1,8.8.8.8\""), "{env0}");
    assert!(env0.contains("OUTBE_RPC_PORT=\"8545\""), "{env0}");
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

    let env1 = fs::read_to_string(staging_root.join("vm/validator-1/validator.env")).unwrap();
    assert!(env1.contains("OUTBE_CONSENSUS_PORT=\"30401\""), "{env1}");
    assert!(env1.contains("OUTBE_RETH_P2P_PORT=\"30304\""), "{env1}");
    assert!(
        env1.contains("OUTBE_RETH_DISCOVERY_PORT=\"31304\""),
        "{env1}"
    );
    assert!(env1.contains("OUTBE_RADICLE_PORT=\"8777\""), "{env1}");

    for index in 0..4 {
        let tree = staging_root.join(format!("vm/validator-{index}"));
        outbe_operator_node_role::vm_boot::verify_tree_without_mount(&tree, &fake_bins).unwrap();
    }
}

#[test]
fn genesis_cli_config_defaults_epoch_and_dkg_windows_when_omitted() {
    let temp = tempfile::tempdir().unwrap();
    let mut config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    config.genesis.epoch_length_blocks = None;
    config.genesis.dkg_prepare_window_blocks = None;
    config.genesis.dkg_activation_grace_blocks = None;
    config.validate().unwrap();

    let (keys, _) = feeder_keys(temp.path(), config.founders.len());
    let defaulted = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
    assert_eq!(defaulted["epoch_length_blocks"], 1200);
    assert_eq!(defaulted["dkg_prepare_window_blocks"], 300);
    assert_eq!(defaulted["dkg_activation_grace_blocks"], 300);
    outbe_genesis_cli::network::validate_config(&defaulted).unwrap();

    // Explicit valid values (>= 120) are preserved verbatim.
    config.genesis.epoch_length_blocks = Some(2400);
    config.genesis.dkg_prepare_window_blocks = Some(120);
    config.genesis.dkg_activation_grace_blocks = Some(180);
    config.validate().unwrap();
    let explicit = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
    assert_eq!(explicit["epoch_length_blocks"], 2400);
    assert_eq!(explicit["dkg_prepare_window_blocks"], 120);
    assert_eq!(explicit["dkg_activation_grace_blocks"], 180);

    // When `base_genesis` is provided, epoch/DKG defaults are NOT injected.
    config.genesis.epoch_length_blocks = None;
    config.genesis.dkg_prepare_window_blocks = None;
    config.genesis.dkg_activation_grace_blocks = None;
    let base_path = temp.path().join("base-genesis.json");
    let with_base = genesis_cli_config(&config, temp.path(), &keys, Some(&base_path)).unwrap();
    assert!(with_base.get("epoch_length_blocks").is_none());
    assert!(with_base.get("dkg_prepare_window_blocks").is_none());
    assert!(with_base.get("dkg_activation_grace_blocks").is_none());
}

#[test]
fn joining_config_serializes_numeric_fields_as_yaml_numbers() {
    let temp = tempfile::tempdir().unwrap();
    let pairs = temp
        .path()
        .join("share/outbe-genesis/profiles/testnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock_http\" }]\n",
        )
        .unwrap();
    let config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    let operator = joining_operator_config(
        &config,
        temp.path(),
        vec!["enode://bootnode".into()],
        vec!["public-key@127.0.0.1:30400".into()],
    )
    .unwrap();
    let yaml = serde_yaml_ng::to_string(&operator).unwrap();
    assert!(yaml.contains("version: 1"));
    assert!(yaml.contains(&format!("chain_id: {}", config.chain_id)));
    assert!(yaml.contains("reth_p2p: 30303"));
    assert!(yaml.contains("consensus: 30400"));
    assert!(!yaml.contains("$serde_json"));
    assert!(!yaml.contains("poll_interval_secs:"));
    assert!(!yaml.contains("signer_funding_wei:"));
    assert!(!yaml.contains("rpc_consensus:"));
    assert!(!yaml.contains("interval_secs:"));
    assert!(!yaml.contains("rpc_urls: ["));
    let decoded: OperatorConfig = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(decoded.network.chain_id, config.chain_id);
    assert_eq!(decoded.hyperlane.poll_interval_secs, 30);
    assert_eq!(decoded.hyperlane.signer_funding_wei, "1000000000000000000");
    assert!(decoded
        .hyperlane
        .origins
        .iter()
        .all(|origin| origin.interval_secs == 15));
}

#[test]
fn devnet_uses_the_self_contained_mock_feeder_profile() {
    let temp = tempfile::tempdir().unwrap();
    let pairs = temp
        .path()
        .join("share/outbe-genesis/profiles/devnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock\" }]\n",
        )
        .unwrap();
    let mut config: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    config.network = Network::Devnet;
    config.chain_id = Network::Devnet.chain_id();
    let (keys, feeder_addresses) = feeder_keys(temp.path(), config.founders.len());

    let genesis = genesis_cli_config(&config, temp.path(), &keys, None).unwrap();
    assert_eq!(genesis["price_provider"], "mock");
    assert_eq!(genesis["feeder_pairs"], json!(pairs));
    for address in feeder_addresses {
        assert_eq!(genesis["balance"][address], FEEDER_FUNDING_WEI.to_string());
    }

    let operator = joining_operator_config(
        &config,
        temp.path(),
        vec!["enode://bootnode".into()],
        vec!["public-key@127.0.0.1:30400".into()],
    )
    .unwrap();
    assert!(operator.feeder.public_toml.contains("provider = \"mock\""));
    assert!(!operator.feeder.public_toml.contains("[[api_providers]]"));
    assert!(!operator.feeder.public_toml.contains("REPLACE_WITH_RPC_URL"));
}

#[test]
fn validates_a_custom_pre_tee_base_genesis() {
    let genesis = json!({
        "config": {"chainId": 54_322_345},
        "alloc": {},
        "timestamp": "0x1"
    });
    ensure_base_genesis(&genesis, 54_322_345).unwrap();
    assert!(ensure_base_genesis(&genesis, 676).is_err());

    let mut finalized = genesis;
    finalized["config"]["teeAttestationV1"] = json!({});
    assert!(ensure_base_genesis(&finalized, 54_322_345).is_err());
}

#[test]
fn vm_founder_trees_pass_vm_boot_verification() {
    let temp = tempfile::tempdir().unwrap();
    let software_root = temp.path().join("software");
    let staging_root = temp.path().join("staging");
    let keys_dir = staging_root.join("secrets/founder-keys");
    let fake_bins = temp.path().join("bins");
    fs::create_dir_all(&staging_root).unwrap();
    fs::create_dir_all(&fake_bins).unwrap();

    let fake_keygen = fake_bins.join("outbe-keygen");
    fs::write(&fake_keygen, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();

    let pairs = software_root.join("share/outbe-genesis/profiles/devnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock\" }]\n",
        )
        .unwrap();
    let crs = software_root.join(".bb-crs/bn254_g1.dat");
    fs::create_dir_all(crs.parent().unwrap()).unwrap();
    fs::write(&crs, b"crs-bytes").unwrap();

    fs::write(
        staging_root.join("genesis.json"),
        br#"{"config":{"chainId":424242}}"#,
    )
    .unwrap();
    let auth_keys = temp.path().join("dev.pub");
    fs::write(
        &auth_keys,
        b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItest dev@outbe\n",
    )
    .unwrap();

    let config_path = temp.path().join("network.yaml");
    let config: NetworkConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: vm-devnet
network: devnet
chain_id: 424242
certified_rpc: "http://10.77.0.10:8545"
vm:
  vmm_host:
    host: 192.0.2.50
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
  authorized_keys: ./dev.pub
founders:
{}
hyperlane:
  gasless: true
  origins:
    - domain: 424242
      rpc_urls: ["https://rpc.example.invalid"]
      reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#,
            (0..4)
                .map(|index| format!(
                    "  - name: validator-{index}\n    advertise_ip: 10.77.0.{}\n    hyperlane_public_url: http://192.0.2.50:{}\n    vm:\n      vm_ip: 10.77.0.{}",
                    index + 10,
                    9000 + index,
                    index + 10,
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
        .unwrap();
    config.validate().unwrap();

    let mut validators = Vec::new();
    let mut bootnodes = Vec::new();
    for index in 0..4 {
        let val_keys = keys_dir.join(format!("validator-{index}"));
        fs::create_dir_all(val_keys.join("radicle/keys")).unwrap();
        let key_hex = format!("{:064x}", index + 1);
        for file in [
            "signing-key.hex",
            "reth-p2p-secret.hex",
            "radicle/keys/radicle",
        ] {
            fs::write(val_keys.join(file), format!("{key_hex}\n")).unwrap();
        }
        for file in ["evm-key.hex", "feeder-key.hex", "hyperlane-key.hex"] {
            fs::write(val_keys.join(file), &key_hex).unwrap();
        }
        let addr = format!(
            "{:#x}",
            validator_address(&val_keys.join("evm-key.hex")).unwrap()
        );
        let pubkey = format!("{:096x}", index + 1);
        let host = format!("10.77.0.{}", index + 10);
        validators.push(json!({
            "address": addr,
            "hyperlane_address": addr,
            "public_key": pubkey,
            "p2p_address": format!("{host}:30400"),
        }));
        bootnodes.push(format!("enode://{:0128x}@{host}:30303", index + 1));
    }

    let artifacts = emit_vm_founder_trees(&VmTreeInputs {
        staging_root: &staging_root,
        software_root: &software_root,
        keys_dir: &keys_dir,
        config_path: &config_path,
        config: &config,
        validators: &validators,
        bootnodes: &bootnodes,
    })
    .unwrap();
    assert_eq!(artifacts.len(), 4);

    for (index, artifact) in artifacts.iter().enumerate() {
        assert!(artifact.archive.is_none());
        let vm = artifact.vm.as_ref().unwrap();
        assert_eq!(vm.data_tree, format!("vm/validator-{index}"));
        assert!(vm.has_authorized_keys);

        let tree = staging_root.join(&vm.data_tree);
        assert!(tree.join("dev/authorized_keys").is_file());
        assert!(!tree.join("outbe-chain").exists());
        assert!(!tree.join("systemd").exists());

        outbe_operator_node_role::vm_boot::verify_tree_without_mount(&tree, &fake_bins).unwrap();
    }
}

#[test]
fn devnet_seeded_genesis_sets_hyperlane_admin_to_founder_zero_and_testnet_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let real_testnet_profile = repo_root.join("bin/outbe-genesis-cli/profiles/testnet.yaml");
    let software = temp.path().join("software");
    let profile_dst = software.join("share/outbe-genesis/profiles/testnet.yaml");
    fs::create_dir_all(profile_dst.parent().unwrap()).unwrap();
    fs::copy(&real_testnet_profile, &profile_dst).unwrap();
    let devnet_pairs = software.join("share/outbe-genesis/profiles/devnet/pairs.toml");
    fs::create_dir_all(devnet_pairs.parent().unwrap()).unwrap();
    fs::write(
            &devnet_pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock\" }]\n",
        )
        .unwrap();
    let testnet_pairs = software.join("share/outbe-genesis/profiles/testnet/pairs.toml");
    fs::create_dir_all(testnet_pairs.parent().unwrap()).unwrap();
    fs::write(
            &testnet_pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock_http\" }]\n",
        )
        .unwrap();

    let (keys, _) = feeder_keys(temp.path(), 4);
    let founder0_addr = validator_address(&keys.join("validator-0/evm-key.hex")).unwrap();
    let profile_admin: Address = "0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05"
        .parse()
        .unwrap();
    assert_ne!(founder0_addr, profile_admin);

    let validators_fixture: Vec<Value> = (0..4)
        .map(|i| {
            let addr = format!(
                "{:#x}",
                validator_address(&keys.join(format!("validator-{i}/evm-key.hex"))).unwrap()
            );
            let feeder = format!(
                "{:#x}",
                validator_address(&keys.join(format!("validator-{i}/feeder-key.hex"))).unwrap()
            );
            json!({
                "address": addr,
                "feeder_address": feeder,
                "public_key": format!("{:096x}", i + 1),
                "radicle_node_id": format!("{:064x}", i + 1),
            })
        })
        .collect();

    // 1. Devnet: real build_seed + seed_validators produces slot 9 == founder 0.
    let mut devnet_cfg: NetworkConfig =
        serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    devnet_cfg.network = Network::Devnet;
    devnet_cfg.chain_id = Network::Devnet.chain_id();
    let devnet_cli = genesis_cli_config(&devnet_cfg, &software, &keys, None).unwrap();
    let devnet_seed = outbe_genesis_cli::stages::build_seed(&devnet_cli).unwrap();
    let mut devnet_alloc = Map::new();
    let mut devnet_genesis = json!({
        "config": {"chainId": 424242, "epochLengthBlocks": 1200},
        "timestamp": "0x6800000",
        "alloc": {}
    });
    outbe_genesis_cli::seed_validators::seed_founders(
        &devnet_genesis,
        &devnet_seed,
        &validators_fixture,
        &mut devnet_alloc,
    )
    .unwrap();
    devnet_genesis["alloc"] = Value::Object(devnet_alloc);
    assert_eq!(
        read_hyperlane_admin_slot(&devnet_genesis).unwrap(),
        founder0_addr
    );
    let devnet_genesis_path = temp.path().join("devnet.genesis.json");
    fs::write(
        &devnet_genesis_path,
        serde_json::to_vec(&devnet_genesis).unwrap(),
    )
    .unwrap();
    verify_devnet_hyperlane_admin(&devnet_genesis_path, founder0_addr).unwrap();
    assert!(verify_devnet_hyperlane_admin(&devnet_genesis_path, profile_admin).is_err());

    // 2. Devnet with conflicting explicit override must fail.
    let mut conflict_cfg = devnet_cfg.clone();
    conflict_cfg.genesis.overrides.insert(
        "hyperlane_controller".into(),
        serde_yaml_ng::from_str("admin: \"0x2Af7d3C5C3f82Fee4eA037A674f55fa2eD011c05\"").unwrap(),
    );
    let err = genesis_cli_config(&conflict_cfg, &software, &keys, None).unwrap_err();
    assert!(
        err.to_string().contains("founder 0"),
        "expected founder 0 policy error, got: {err}"
    );

    // 3. Testnet: unchanged — inherits testnet.yaml admin.
    let testnet_cfg: NetworkConfig =
        serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    let testnet_cli = genesis_cli_config(&testnet_cfg, &software, &keys, None).unwrap();
    let testnet_seed = outbe_genesis_cli::stages::build_seed(&testnet_cli).unwrap();
    let mut testnet_alloc = Map::new();
    let mut testnet_genesis = json!({
        "config": {"chainId": 54322345, "epochLengthBlocks": 1200},
        "timestamp": "0x6800000",
        "alloc": {}
    });
    outbe_genesis_cli::seed_validators::seed_founders(
        &testnet_genesis,
        &testnet_seed,
        &validators_fixture,
        &mut testnet_alloc,
    )
    .unwrap();
    testnet_genesis["alloc"] = Value::Object(testnet_alloc);
    assert_eq!(
        read_hyperlane_admin_slot(&testnet_genesis).unwrap(),
        profile_admin
    );
}

#[test]
fn vm_devnet_local_evm_generates_exactly_two_origins_and_passes_vm_boot() {
    let temp = tempfile::tempdir().unwrap();
    let software_root = temp.path().join("software");
    let staging_root = temp.path().join("staging");
    let keys_dir = staging_root.join("secrets/founder-keys");
    let fake_bins = temp.path().join("bins");
    fs::create_dir_all(&staging_root).unwrap();
    fs::create_dir_all(&fake_bins).unwrap();

    let fake_keygen = fake_bins.join("outbe-keygen");
    fs::write(&fake_keygen, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();

    let pairs = software_root.join("share/outbe-genesis/profiles/devnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(
            &pairs,
            "[[currency_pairs]]\nbase = \"COEN\"\nquote = \"840\"\nmarkets = [{ provider = \"mock\" }]\n",
        )
        .unwrap();
    let crs = software_root.join(".bb-crs/bn254_g1.dat");
    fs::create_dir_all(crs.parent().unwrap()).unwrap();
    fs::write(&crs, b"crs-bytes").unwrap();
    fs::write(
        staging_root.join("genesis.json"),
        br#"{"config":{"chainId":424242}}"#,
    )
    .unwrap();

    let config_path = temp.path().join("network.yaml");
    let config: NetworkConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: vm-devnet-hl3
network: devnet
chain_id: 424242
certified_rpc: "http://10.77.0.68:8545"
vm:
  vmm_host:
    host: 192.0.2.50
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
founders:
{}
hyperlane:
  gasless: true
  local_evm:
    domain: 31337
    rpc_url: "http://10.77.0.1:8546"
    reorg_period: 0
topology:
  use_local_defaults: true
genesis:
  prefunded_accounts: {{}}
"#,
            (0..4)
                .map(|index| format!(
                    "  - name: validator-{index}\n    advertise_ip: 10.77.0.{}\n    hyperlane_public_url: http://10.77.0.{}:9000\n    vm:\n      vm_ip: 10.77.0.{}",
                    68 + index,
                    68 + index,
                    68 + index,
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
        .unwrap();
    config.validate().unwrap();

    let mut validators = Vec::new();
    let mut bootnodes = Vec::new();
    for index in 0..4 {
        let val_keys = keys_dir.join(format!("validator-{index}"));
        fs::create_dir_all(val_keys.join("radicle/keys")).unwrap();
        let key_hex = format!("{:064x}", index + 1);
        for file in [
            "signing-key.hex",
            "reth-p2p-secret.hex",
            "radicle/keys/radicle",
        ] {
            fs::write(val_keys.join(file), format!("{key_hex}\n")).unwrap();
        }
        for file in ["evm-key.hex", "feeder-key.hex", "hyperlane-key.hex"] {
            fs::write(val_keys.join(file), &key_hex).unwrap();
        }
        let addr = format!(
            "{:#x}",
            validator_address(&val_keys.join("evm-key.hex")).unwrap()
        );
        let host = format!("10.77.0.{}", 68 + index);
        validators.push(json!({
            "address": addr,
            "hyperlane_address": addr,
            "public_key": format!("{:096x}", index + 1),
            "p2p_address": format!("{host}:30400"),
        }));
        bootnodes.push(format!("enode://{:0128x}@{host}:30303", index + 1));
    }

    emit_vm_founder_trees(&VmTreeInputs {
        staging_root: &staging_root,
        software_root: &software_root,
        keys_dir: &keys_dir,
        config_path: &config_path,
        config: &config,
        validators: &validators,
        bootnodes: &bootnodes,
    })
    .unwrap();

    for index in 0..4 {
        let tree = staging_root.join(format!("vm/validator-{index}"));
        let raw = fs::read_to_string(tree.join("hyperlane-public.toml")).unwrap();
        assert!(!raw.contains("[account]"));
        assert!(!raw.contains("private_key"));
        assert!(!raw.contains("testnet.outbe.net"));
        assert!(!raw.contains("bnbchain.org"));
        assert!(!raw.contains("publicnode.com"));
        let parsed: toml::Table = raw.parse().unwrap();
        let origins = parsed["origin"].as_array().unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0]["domain"].as_integer(), Some(424_242));
        assert_eq!(
            origins[0]["rpc_urls"].as_array().unwrap()[0].as_str(),
            Some(format!("http://10.77.0.{}:8545", 68 + index).as_str())
        );
        assert_eq!(origins[0]["rpc_consensus"].as_str(), Some("single"));
        assert_eq!(origins[0]["reorg_period"].as_integer(), Some(0));
        assert_eq!(origins[1]["domain"].as_integer(), Some(31_337));
        assert_eq!(
            origins[1]["rpc_urls"].as_array().unwrap()[0].as_str(),
            Some("http://10.77.0.1:8546")
        );
        assert_eq!(origins[1]["rpc_consensus"].as_str(), Some("single"));
        assert_eq!(origins[1]["reorg_period"].as_integer(), Some(0));

        outbe_operator_node_role::vm_boot::verify_tree_without_mount(&tree, &fake_bins).unwrap();
    }
}

#[test]
fn hyperlane_public_toml_and_local_evm_reject_invalid_configs() {
    use outbe_operator_node_role::config::validate_hyperlane_public_toml;

    let valid = "\
[hyperlane]
poll_interval_secs = 30
gasless = true

[health]
enabled = true
bind_address = \"127.0.0.1:9003\"

[storage]
db_path = \"/opt/outbe-chain/state/hyperlane/db\"

[server]
bind_address = \"0.0.0.0:9000\"
public_url = \"http://10.77.0.68:9000\"

[[origin]]
domain = 424242
rpc_urls = [\"http://10.77.0.68:8545\"]
rpc_consensus = \"single\"
reorg_period = 0

[[origin]]
domain = 31337
rpc_urls = [\"http://10.77.0.1:8546\"]
rpc_consensus = \"single\"
reorg_period = 0
";
    validate_hyperlane_public_toml(valid, 424_242, true).unwrap();

    assert!(validate_hyperlane_public_toml(valid, 54_322_345, true).is_err());

    let with_account = format!("{valid}\n[account]\nprivate_key = \"0x01\"\n");
    assert!(validate_hyperlane_public_toml(&with_account, 424_242, true).is_err());

    let zero_domain = valid.replace("domain = 31337", "domain = 0");
    assert!(validate_hyperlane_public_toml(&zero_domain, 424_242, true).is_err());

    let dup_domain = valid.replace("domain = 31337", "domain = 424242");
    assert!(validate_hyperlane_public_toml(&dup_domain, 424_242, true).is_err());

    let with_creds = valid.replace("http://10.77.0.1:8546", "http://user:pass@10.77.0.1:8546");
    assert!(validate_hyperlane_public_toml(&with_creds, 424_242, true).is_err());
}

#[test]
fn custom_rpc_port_flows_into_vm_env_hyperlane_and_bare_host_genesis_config() {
    let temp = tempfile::tempdir().unwrap();
    let software = temp.path().join("software");
    let pairs = software.join("share/outbe-genesis/profiles/devnet/pairs.toml");
    fs::create_dir_all(pairs.parent().unwrap()).unwrap();
    fs::write(&pairs, "[[pairs]]\nbase = \"ETH\"\nquote = \"USD\"\n").unwrap();
    fs::create_dir_all(software.join(".bb-crs")).unwrap();
    fs::write(software.join(".bb-crs/bn254_g1.dat"), b"crs").unwrap();

    let keys_dir = temp.path().join("keys");
    let mut validators = Vec::new();
    for i in 0..4 {
        let vdir = keys_dir.join(format!("validator-{i}"));
        fs::create_dir_all(vdir.join("radicle/keys")).unwrap();
        let key_hex = format!("{:064x}", i + 1);
        for file in [
            "signing-key.hex",
            "evm-key.hex",
            "feeder-key.hex",
            "hyperlane-key.hex",
            "reth-p2p-secret.hex",
        ] {
            fs::write(vdir.join(file), &key_hex).unwrap();
        }
        fs::write(vdir.join("radicle/keys/radicle"), &key_hex).unwrap();
        validators.push(json!({
            "address": format!("0x{:040x}", i + 1),
            "hyperlane_address": format!("0x{:040x}", i + 100),
            "public_key": format!("{:096x}", i + 1),
            "p2p_address": format!("10.77.0.{}:30400", 68 + i),
        }));
    }

    let mut vm_cfg: NetworkConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: custom-rpc-vm
network: devnet
chain_id: 424242
certified_rpc: "http://10.77.0.68:18545"
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
                    "  - name: validator-{i}\n    advertise_ip: 10.77.0.{}\n    rpc_port: {}\n    hyperlane_public_url: http://10.77.0.{}:9000\n    vm:\n      vm_ip: 10.77.0.{}",
                    68 + i,
                    18_545 + i,
                    68 + i,
                    68 + i
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
        .unwrap();
    vm_cfg.validate().unwrap();

    let staging = temp.path().join("staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(
        staging.join("genesis.json"),
        r#"{"config":{"chainId":424242}}"#,
    )
    .unwrap();
    let bootnodes: Vec<String> = (0..4)
        .map(|i| format!("enode://{:0128x}@10.77.0.{}:30303", i + 1, 68 + i))
        .collect();
    let artifacts = emit_vm_founder_trees(&VmTreeInputs {
        staging_root: &staging,
        software_root: &software,
        keys_dir: &keys_dir,
        config_path: &temp.path().join("network.yaml"),
        config: &vm_cfg,
        validators: &validators,
        bootnodes: &bootnodes,
    })
    .unwrap();
    assert_eq!(artifacts[0].rpc_port, 18_545);
    assert_eq!(artifacts[1].rpc_port, 18_546);

    let fake_bins = temp.path().join("fake-bins");
    fs::create_dir_all(&fake_bins).unwrap();
    let fake_keygen = fake_bins.join("outbe-keygen");
    fs::write(&fake_keygen, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();

    for i in 0..4 {
        let tree = staging.join(format!("vm/validator-{i}"));
        let env = fs::read_to_string(tree.join("validator.env")).unwrap();
        let expected_port = 18_545 + i;
        assert!(
            env.contains(&format!("OUTBE_RPC_PORT=\"{expected_port}\"")),
            "{env}"
        );
        let hl = fs::read_to_string(tree.join("hyperlane-public.toml")).unwrap();
        assert!(
            hl.contains(&format!("http://10.77.0.{}:{expected_port}", 68 + i)),
            "{hl}"
        );
        outbe_operator_node_role::vm_boot::verify_tree_without_mount(&tree, &fake_bins).unwrap();
    }

    // Bare-host genesis_cli_config emits `rpc_port` when uniform and `validator_runtime` when distinct.
    let mut bare: NetworkConfig = serde_yaml_ng::from_str(super::super::config::EXAMPLE).unwrap();
    for founder in &mut bare.founders {
        founder.rpc_port = Some(18_545);
    }
    bare.validate().unwrap();
    let uniform_cli = genesis_cli_config(&bare, &software, &keys_dir, None).unwrap();
    assert_eq!(uniform_cli.get("rpc_port"), Some(&json!(18_545)));
    assert!(uniform_cli.get("validator_runtime").is_none());

    bare.founders[1].rpc_port = Some(18_546);
    bare.validate().unwrap();
    let per_founder_cli = genesis_cli_config(&bare, &software, &keys_dir, None).unwrap();
    assert_eq!(
        per_founder_cli.pointer("/validator_runtime/0/rpc_port"),
        Some(&json!(18_545))
    );
    assert_eq!(
        per_founder_cli.pointer("/validator_runtime/1/rpc_port"),
        Some(&json!(18_546))
    );
    // Loopback rpc_bind_ip != vm.vm_ip is refused at network config validation & build time.
    vm_cfg.founders[0].rpc_bind_ip = Some(Ipv4Addr::LOCALHOST);
    let cfg_err = vm_cfg.validate().unwrap_err().to_string();
    assert!(
        cfg_err.contains("rpc_bind_ip=127.0.0.1 != vm.vm_ip=10.77.0.68"),
        "{cfg_err}"
    );
    let lb_staging = temp.path().join("lb-staging");
    fs::create_dir_all(&lb_staging).unwrap();
    fs::write(
        lb_staging.join("genesis.json"),
        r#"{"config":{"chainId":424242}}"#,
    )
    .unwrap();
    let build_err = emit_vm_founder_trees(&VmTreeInputs {
        staging_root: &lb_staging,
        software_root: &software,
        keys_dir: &keys_dir,
        config_path: &temp.path().join("network.yaml"),
        config: &vm_cfg,
        validators: &validators,
        bootnodes: &bootnodes,
    })
    .unwrap_err()
    .to_string();
    assert!(
        build_err.contains("rpc_bind_ip=127.0.0.1 != vm.vm_ip=10.77.0.68"),
        "{build_err}"
    );
}

#[test]
fn firewall_mappings_cover_published_vm_founders_and_omit_single_host_local_devnet() {
    // 1. Multi-host 2+2 VM testnet -> 4 founders x 6 DNAT ports = 24 entries.
    let mixed: NetworkConfig = serde_yaml_ng::from_str(
        r#"
version: 1
name: mixed-fw
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
    let mappings = build_vm_firewall_mappings(&mixed).unwrap();
    assert_eq!(mappings.len(), 24);
    assert_eq!(
        mappings[0],
        FirewallMappingEntry {
            member: "validator-0".to_owned(),
            host: "198.51.100.10".to_owned(),
            public_ip: Ipv4Addr::new(198, 51, 100, 10),
            public_port: 30_303,
            proto: "tcp".to_owned(),
            vm_ip: Ipv4Addr::new(10, 77, 0, 10),
            vm_port: 30_303,
            purpose: "reth-p2p".to_owned(),
        }
    );
    // validator-1 uses port_offset 1 -> consensus-p2p is 30401.
    assert!(mappings.iter().any(|m| {
        m.member == "validator-1"
            && m.host == "198.51.100.10"
            && m.public_ip == Ipv4Addr::new(198, 51, 100, 10)
            && m.public_port == 30_401
            && m.proto == "tcp"
            && m.vm_ip == Ipv4Addr::new(10, 77, 0, 11)
            && m.vm_port == 30_401
            && m.purpose == "consensus-p2p"
    }));
    assert!(mappings.iter().any(|m| {
        m.member == "validator-1"
            && m.public_port == 9_001
            && m.proto == "tcp"
            && m.vm_ip == Ipv4Addr::new(10, 77, 0, 11)
            && m.vm_port == 9_000
            && m.purpose == "hyperlane"
    }));
    let table = render_firewall_mappings_table(&mappings);
    assert!(table.contains("MEMBER"), "{table}");
    assert!(table.contains("PUBLIC_PORT"), "{table}");
    assert!(table.contains("reth-discovery"), "{table}");
    assert!(table.contains("radicle"), "{table}");
    assert!(table.contains("hyperlane"), "{table}");

    // 2. Single-host local devnet (advertise_ip == vm_ip, port_offset == 0) -> empty mappings.
    let local_dev: NetworkConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
name: local-dev
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
        ))
        .unwrap();
    local_dev.validate().unwrap();
    assert!(build_vm_firewall_mappings(&local_dev).unwrap().is_empty());
}
