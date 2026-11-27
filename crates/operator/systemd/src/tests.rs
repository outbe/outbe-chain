use super::*;

const COMMON_POLICY: &[&str] = &[
    "Restart=always",
    "RestartMode=direct",
    "RestartSec=5",
    "RestartSteps=5",
    "RestartMaxDelaySec=60",
    "TimeoutStartSec=150",
    "TimeoutStopSec=90",
    "KillMode=mixed",
    "KillSignal=SIGTERM",
    "UMask=0077",
    "NoNewPrivileges=true",
    "PrivateTmp=true",
    "ProtectHome=true",
    "ProtectSystem=strict",
];

#[test]
fn joining_and_founding_share_service_policy() {
    let joining = joining_units("/usr/local/bin/outbe-up");
    let founding = founding_units(FoundingConfig {
        base_dir: "/opt/outbe-chain",
        validator_index: 0,
    });
    for name in [
        "outbe-radicle.service",
        "outbe-validator.service",
        "outbe-feeder.service",
        "outbe-hyperlane-validator.service",
    ] {
        let joining = unit(&joining, name);
        let founding = unit(&founding, name);
        for directive in COMMON_POLICY {
            assert!(joining.contains(directive), "{name}: {directive}");
            assert!(founding.contains(directive), "{name}: {directive}");
        }
    }
}

#[test]
fn units_are_concrete_and_flow_specific_only_where_required() {
    let joining = joining_units("/usr/local/bin/outbe-up");
    let founding = founding_units(FoundingConfig {
        base_dir: "/opt/outbe-chain",
        validator_index: 2,
    });
    let removed_service = format!("{}-{}", "outbe", "enclave.service");
    for unit in joining.iter().chain(&founding) {
        assert!(!unit.name.contains("enclave"));
        assert!(!unit.name.contains("tee-renew"));
        for removed in [
            removed_service.as_str(),
            "aesmd.service",
            "/opt/outbe/sgx",
            "/var/lib/outbe/tee",
            "internal enclave",
            "internal wait-enclave",
            "--tee-enclave-socket",
            "--tee-session-mode",
        ] {
            assert!(!unit.content.contains(removed), "{}: {removed}", unit.name);
        }
        assert!(!unit.content.contains("@@"));
        assert!(!unit.content.contains("%i"));
        assert!(!unit.content.contains("@.service"));
    }
    assert!(joining
        .iter()
        .all(|unit| unit.name != "outbe-full-node.service"));

    let validator = unit(&joining, "outbe-validator.service");
    assert!(validator
        .contains("ExecStartPre=/usr/local/bin/outbe-up internal wait-validator-dependencies"));
    assert!(validator.contains("--validator"));
    assert!(validator.contains("radicle.control-socket"));
    assert!(validator.contains("${OUTBE_RETH_P2P_PORT}"));
    assert!(validator.contains("${OUTBE_RETH_DISCOVERY_PORT}"));
    assert!(validator.contains("${OUTBE_CONSENSUS_PORT}"));
    assert!(validator.contains("--tee-bootstrap-timeout-secs 300"));
    assert!(unit(&joining, "outbe-radicle.service").contains("${OUTBE_RADICLE_PORT}"));
    assert!(unit(&founding, "outbe-validator.service")
        .contains("/opt/outbe-chain/validator-2/run-node.sh"));

    let joining_hyperlane = unit(&joining, "outbe-hyperlane-validator.service");
    let founding_hyperlane = unit(&founding, "outbe-hyperlane-validator.service");
    assert!(joining_hyperlane.contains("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE=1"));
    assert!(!founding_hyperlane.contains("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE"));
    for hyperlane in [joining_hyperlane, founding_hyperlane] {
        assert!(hyperlane.contains("/opt/outbe-chain/state/hyperlane"));
        assert!(!hyperlane.contains("StateDirectory="));
    }
    for feeder in [
        unit(&joining, "outbe-feeder.service"),
        unit(&founding, "outbe-feeder.service"),
    ] {
        assert!(feeder.contains("/opt/outbe-chain/state/feeder"));
        assert!(!feeder.contains("StateDirectory="));
    }
}

fn unit<'a>(units: &'a [UnitFile], name: &str) -> &'a str {
    &units.iter().find(|unit| unit.name == name).unwrap().content
}

#[test]
fn joining_units_match_pinned_output() {
    let joining = joining_units("/usr/local/bin/outbe-up");
    let expected = vec![
        (
            "outbe-feeder.service",
            concat!(
                "[Unit]\n",
                "Description=Outbe validator price feeder\n",
                "After=outbe-validator.service\n",
                "Wants=outbe-validator.service\n",
                "PartOf=outbe-validator.target\n",
                "StartLimitIntervalSec=0\n",
                "\n",
                "[Service]\n",
                "Type=simple\n",
                "User=outbe\n",
                "Group=outbe\n",
                "EnvironmentFile=/opt/outbe-chain/validator.env\n",
                "RuntimeDirectory=outbe-feeder\n",
                "RuntimeDirectoryMode=0700\n",
                "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/feeder\n",
                "ExecStartPre=/usr/local/bin/outbe-up internal wait-rpc\n",
                "ExecStart=/usr/local/bin/outbe-up internal feeder\n",
                "Restart=always\n",
                "RestartMode=direct\n",
                "RestartSec=5\n",
                "RestartSteps=5\n",
                "RestartMaxDelaySec=60\n",
                "TimeoutStartSec=150\n",
                "TimeoutStopSec=90\n",
                "KillMode=mixed\n",
                "KillSignal=SIGTERM\n",
                "SuccessExitStatus=143 SIGTERM\n",
                "UMask=0077\n",
                "LimitNOFILE=65535\n",
                "NoNewPrivileges=true\n",
                "PrivateTmp=true\n",
                "ProtectHome=true\n",
                "ProtectSystem=strict\n",
                "ReadWritePaths=/opt/outbe-chain/state/feeder\n",
                "\n",
                "[Install]\n",
                "WantedBy=outbe-validator.target\n",
            ),
        ),
        (
            "outbe-hyperlane-validator.service",
            concat!(
                "[Unit]\n",
                "Description=Outbe Hyperlane validator\n",
                "After=outbe-validator.service\n",
                "Wants=outbe-validator.service\n",
                "PartOf=outbe-validator.target\n",
                "StartLimitIntervalSec=0\n",
                "\n",
                "[Service]\n",
                "Type=simple\n",
                "User=outbe\n",
                "Group=outbe\n",
                "EnvironmentFile=/opt/outbe-chain/validator.env\n",
                "RuntimeDirectory=outbe-hyperlane-validator\n",
                "RuntimeDirectoryMode=0700\n",
                "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/hyperlane\n",
                "Environment=OUTBE_HYPERLANE_WAIT_FOR_ACTIVE=1\n",
                "ExecStartPre=/usr/local/bin/outbe-up internal wait-rpc\n",
                "ExecStart=/usr/local/bin/outbe-up internal hyperlane\n",
                "Restart=always\n",
                "RestartMode=direct\n",
                "RestartSec=5\n",
                "RestartSteps=5\n",
                "RestartMaxDelaySec=60\n",
                "TimeoutStartSec=150\n",
                "TimeoutStopSec=90\n",
                "KillMode=mixed\n",
                "KillSignal=SIGTERM\n",
                "SuccessExitStatus=143 SIGTERM\n",
                "UMask=0077\n",
                "LimitNOFILE=65535\n",
                "NoNewPrivileges=true\n",
                "PrivateTmp=true\n",
                "ProtectHome=true\n",
                "ProtectSystem=strict\n",
                "ReadWritePaths=/opt/outbe-chain/state/hyperlane\n",
                "\n",
                "[Install]\n",
                "WantedBy=outbe-validator.target\n",
            ),
        ),
        (
            "outbe-radicle.service",
            concat!(
                "[Unit]\n",
                "Description=Outbe validator Radicle sidecar\n",
                "After=network-online.target\n",
                "Wants=network-online.target\n",
                "PartOf=outbe-validator.target\n",
                "StartLimitIntervalSec=0\n",
                "\n",
                "[Service]\n",
                "Type=simple\n",
                "User=outbe\n",
                "Group=outbe\n",
                "EnvironmentFile=/opt/outbe-chain/validator.env\n",
                "ExecStartPre=/usr/bin/rm -f /opt/outbe-chain/keys/radicle/node/outbe-control.sock\n",
                "ExecStart=/opt/outbe-chain/outbe-radicle \\\n",
                "  --home /opt/outbe-chain/keys/radicle \\\n",
                "  --control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\\n",
                "  --listen 0.0.0.0:${OUTBE_RADICLE_PORT} \\\n",
                "  --status-listen 127.0.0.1:8876 \\\n",
                "  --max-validators 128 \\\n",
                "  --external-inbound-reserve 16 \\\n",
                "  --advertise ${OUTBE_EXTERNAL_IP}:${OUTBE_RADICLE_PORT}\n",
                "Restart=always\n",
                "RestartMode=direct\n",
                "RestartSec=5\n",
                "RestartSteps=5\n",
                "RestartMaxDelaySec=60\n",
                "TimeoutStartSec=150\n",
                "TimeoutStopSec=90\n",
                "KillMode=mixed\n",
                "KillSignal=SIGTERM\n",
                "SuccessExitStatus=143 SIGTERM\n",
                "UMask=0077\n",
                "LimitNOFILE=65535\n",
                "NoNewPrivileges=true\n",
                "PrivateTmp=true\n",
                "ProtectHome=true\n",
                "ProtectSystem=strict\n",
                "ReadWritePaths=/opt/outbe-chain/keys\n",
                "\n",
                "[Install]\n",
                "WantedBy=outbe-validator.target\n",
            ),
        ),
        (
            "outbe-validator.service",
            concat!(
                "[Unit]\n",
                "Description=Outbe validator\n",
                "After=network-online.target outbe-radicle.service\n",
                "Wants=network-online.target outbe-radicle.service\n",
                "PartOf=outbe-validator.target\n",
                "StartLimitIntervalSec=0\n",
                "\n",
                "[Service]\n",
                "Type=simple\n",
                "User=outbe\n",
                "Group=outbe\n",
                "EnvironmentFile=/opt/outbe-chain/validator.env\n",
                "Environment=RUST_MIN_STACK=16777216\n",
                "ExecStartPre=/usr/local/bin/outbe-up internal wait-validator-dependencies\n",
                "ExecStart=/opt/outbe-chain/outbe-chain node \\\n",
                "  --chain /opt/outbe-chain/genesis.json \\\n",
                "  --datadir /opt/outbe-chain/validator-0/data \\\n",
                "  --engine.persistence-threshold 0 \\\n",
                "  --engine.memory-block-buffer-target 0 \\\n",
                "  --consensus.storage-dir /opt/outbe-chain/validator-0/consensus \\\n",
                "  --http \\\n",
                "  --http.addr ${OUTBE_RPC_BIND_IP} \\\n",
                "  --http.port ${OUTBE_RPC_PORT} \\\n",
                "  --http.api eth,net,web3,outbe \\\n",
                "  --authrpc.port 8551 \\\n",
                "  --port ${OUTBE_RETH_P2P_PORT} \\\n",
                "  --discovery.port ${OUTBE_RETH_P2P_PORT} \\\n",
                "  --nat extip:${OUTBE_EXTERNAL_IP} \\\n",
                "  --discovery.v5.addr 0.0.0.0 \\\n",
                "  --discovery.v5.port ${OUTBE_RETH_DISCOVERY_PORT} \\\n",
                "  --p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\\n",
                "  --bootnodes ${OUTBE_BOOTNODES} \\\n",
                "  --metrics 127.0.0.1:9101 \\\n",
                "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\\n",
                "  --log.file.directory /opt/outbe-chain/validator-0/logs \\\n",
                "  --validator \\\n",
                "  --consensus.signing-key /opt/outbe-chain/keys/signing-key.hex \\\n",
                "  --validator.evm-key /opt/outbe-chain/keys/evm-key.hex \\\n",
                "  --consensus.keys-dir /opt/outbe-chain/keys \\\n",
                "  --consensus.listen-addr 0.0.0.0:${OUTBE_CONSENSUS_PORT} \\\n",
                "  --tee-bootstrap-timeout-secs 300 \\\n",
                "  --consensus.peers ${OUTBE_CONSENSUS_PEERS} \\\n",
                "  $OUTBE_CONSENSUS_EXTRA_ARGS \\\n",
                "  --upstream ${OUTBE_CERTIFIED_RPC} \\\n",
                "  --radicle.control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\\n",
                "  --radicle.status-address 127.0.0.1:8876\n",
                "ExecStartPost=/usr/local/bin/outbe-up internal wait-rpc\n",
                "Restart=always\n",
                "RestartMode=direct\n",
                "RestartSec=5\n",
                "RestartSteps=5\n",
                "RestartMaxDelaySec=60\n",
                "TimeoutStartSec=150\n",
                "TimeoutStopSec=90\n",
                "KillMode=mixed\n",
                "KillSignal=SIGTERM\n",
                "SuccessExitStatus=143 SIGTERM\n",
                "UMask=0077\n",
                "LimitNOFILE=1048576\n",
                "NoNewPrivileges=true\n",
                "PrivateTmp=true\n",
                "ProtectHome=true\n",
                "ProtectSystem=strict\n",
                "ReadWritePaths=/opt/outbe-chain/keys /opt/outbe-chain/validator-0\n",
                "\n",
                "[Install]\n",
                "WantedBy=outbe-validator.target\n",
            ),
        ),
        (
            "outbe-validator.target",
            concat!(
                "[Unit]\n",
                "Description=Outbe validator stack\n",
                "Wants=outbe-radicle.service\n",
                "Wants=outbe-validator.service\n",
                "Wants=outbe-feeder.service\n",
                "Wants=outbe-hyperlane-validator.service\n",
                "After=outbe-radicle.service\n",
                "After=outbe-validator.service outbe-feeder.service\n",
                "After=outbe-hyperlane-validator.service\n",
                "\n",
                "[Install]\n",
                "WantedBy=multi-user.target\n",
            ),
        ),
    ];

    let actual: Vec<(&str, &str)> = joining
        .iter()
        .map(|unit| (unit.name.as_str(), unit.content.as_str()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn vm_units_share_service_policy_and_target() {
    let vm = vm_units();
    let names: Vec<&str> = vm.iter().map(|unit| unit.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "outbe-feeder.service",
            "outbe-full-node.service",
            "outbe-hyperlane-validator.service",
            "outbe-joining-validator.service",
            "outbe-radicle.service",
            "outbe-validator.service",
            "outbe-validator.target",
            "outbe-vm-boot.service",
        ]
    );

    for name in [
        "outbe-radicle.service",
        "outbe-validator.service",
        "outbe-joining-validator.service",
        "outbe-full-node.service",
        "outbe-feeder.service",
        "outbe-hyperlane-validator.service",
    ] {
        let content = unit(&vm, name);
        for directive in COMMON_POLICY {
            assert!(content.contains(directive), "{name}: {directive}");
        }
        assert!(
            content.contains("EnvironmentFile=/opt/outbe-chain/validator.env"),
            "{name}: missing validator.env"
        );
        assert!(
            content.contains("Requires=outbe-vm-boot.service"),
            "{name}: missing Requires=outbe-vm-boot.service"
        );
        assert!(
            content.contains("After=outbe-vm-boot.service"),
            "{name}: missing After=outbe-vm-boot.service"
        );
        assert!(
            !content.contains("run-"),
            "{name}: VM unit must not reference shell launch scripts"
        );
        assert!(
            !content.contains("StateDirectory="),
            "{name}: VM unit must use /opt/outbe-chain state paths"
        );
    }

    // No extra role marker channel: vm-boot reads OUTBE_NODE_ROLE from validator.env and
    // starts only that role's units directly.
    for unit in &vm {
        assert!(
            !unit.content.contains("ConditionPathExists="),
            "{}: must not introduce a ConditionPathExists role channel",
            unit.name
        );
        assert!(
            !unit.content.contains("/run/outbe-vm"),
            "{}: must not reference /run/outbe-vm",
            unit.name
        );
    }

    let boot = unit(&vm, "outbe-vm-boot.service");
    assert!(boot.contains("Type=oneshot"));
    assert!(boot.contains("RemainAfterExit=yes"));
    assert!(boot.contains("Requires=outbe-data-mount.service"));
    assert!(!boot.contains("network-online.target"));
    assert!(
        boot.contains("After=local-fs.target systemd-networkd.service outbe-data-mount.service")
    );
    assert!(boot.contains("ReadWritePaths=/opt/outbe-chain /run/systemd/network\n"));
    assert!(
        boot.contains("ExecStart=/usr/local/bin/outbe-vm-agent vm-boot --bin-dir /usr/local/bin")
    );
    assert!(!boot.contains("EnvironmentFile="));

    let hyperlane = unit(&vm, "outbe-hyperlane-validator.service");
    assert!(!hyperlane.contains("OUTBE_HYPERLANE_WAIT_FOR_ACTIVE"));
    assert!(!hyperlane.contains("Wants=outbe-validator.service"));
    let feeder = unit(&vm, "outbe-feeder.service");
    assert!(!feeder.contains("Wants=outbe-validator.service"));

    let target = unit(&vm, "outbe-validator.target");
    assert!(target.contains("Requires=outbe-vm-boot.service"));
    assert!(target.contains("After=outbe-vm-boot.service"));
    assert!(!target.contains("Wants="));

    for unit in &vm {
        assert!(!unit.content.contains("@@"));
        assert!(!unit.content.contains("%i"));
        assert!(!unit.content.contains("@.service"));
        assert!(
            !unit.content.contains("outbe-up") && !unit.content.contains("outbe-net-up"),
            "{}: VM unit must use outbe-vm-agent, never outbe-up or outbe-net-up",
            unit.name
        );
    }
}

#[test]
fn vm_units_execute_only_image_binaries_and_write_only_under_data_root() {
    let vm = vm_units();
    let mut exec_count = 0usize;

    for unit in &vm {
        let directives = logical_directives(&unit.content);
        let mut has_rw_paths = false;

        for (key, value) in directives {
            if key.starts_with("ExecStart") {
                exec_count += 1;
                let raw_exe = value
                    .split_whitespace()
                    .next()
                    .expect("ExecStart* must name an executable");
                let exe = raw_exe.trim_start_matches(['-', '+', '!', '@', ':']);
                assert!(
                    !exe.starts_with("/opt/outbe-chain"),
                    "{}: {key} executes from data disk: {exe}",
                    unit.name
                );
                assert!(
                    exe.starts_with("/usr/local/bin/") || exe.starts_with("/usr/bin/"),
                    "{}: {key} executes outside image bin dirs: {exe}",
                    unit.name
                );
            }

            if key == "ReadWritePaths" {
                has_rw_paths = true;
                let paths: Vec<&str> = value.split_whitespace().collect();
                assert!(!paths.is_empty(), "{}: empty ReadWritePaths", unit.name);
                for path in paths {
                    let allowed_vm_boot_net =
                        unit.name == "outbe-vm-boot.service" && path == "/run/systemd/network";
                    assert!(
                        path == "/opt/outbe-chain"
                            || path.starts_with("/opt/outbe-chain/")
                            || allowed_vm_boot_net,
                        "{}: ReadWritePaths outside /opt/outbe-chain: {path}",
                        unit.name
                    );
                }
            }
        }

        if unit.name.ends_with(".service") {
            assert!(has_rw_paths, "{}: missing ReadWritePaths", unit.name);
        }
    }

    assert!(
        exec_count >= 13,
        "expected ExecStart* directives across all VM role units"
    );
    assert!(unit(&vm, "outbe-feeder.service")
        .contains("ExecStart=/usr/local/bin/outbe-vm-agent feeder --bin-dir /usr/local/bin"));
    assert!(unit(&vm, "outbe-hyperlane-validator.service")
        .contains("ExecStart=/usr/local/bin/outbe-vm-agent hyperlane --bin-dir /usr/local/bin"));
}

#[test]
fn vm_validator_args_match_pinned_founding_flags() {
    use std::collections::BTreeSet;

    let vm = vm_units();
    let vm_validator = unit(&vm, "outbe-validator.service");
    let vm_exec = exec_start_tokens(vm_validator);
    assert_eq!(vm_exec[0], "/usr/local/bin/outbe-chain");
    assert_eq!(vm_exec[1], "node");

    let vm_flags = node_flag_set(&vm_exec[2..]);

    // Founding node_script flags in bin/outbe-genesis-cli/src/launch_bundle/scripts.rs.
    let expected_founding_flags: BTreeSet<&str> = [
        "--validator",
        "--chain",
        "--datadir",
        "--engine.persistence-threshold",
        "--engine.memory-block-buffer-target",
        "--consensus.signing-key",
        "--validator.evm-key",
        "--consensus.listen-addr",
        "$OUTBE_CONSENSUS_EXTRA_ARGS",
        "--consensus.storage-dir",
        "--radicle.control-socket",
        "--radicle.status-address",
        "--http",
        "--http.addr",
        "--http.port",
        "--http.api",
        "--authrpc.port",
        "--port",
        "--discovery.port",
        "--nat",
        "--discovery.v5.addr",
        "--discovery.v5.port",
        "--p2p-secret-key",
        "--bootnodes",
        "--metrics",
        "--ipcpath",
        "--log.file.directory",
    ]
    .into_iter()
    .collect();

    assert_eq!(vm_flags, expected_founding_flags);

    // Verify joining adds only its four join-specific flags relative to VM founder.
    let joining = joining_units("/usr/local/bin/outbe-up");
    let joining_exec = exec_start_tokens(unit(&joining, "outbe-validator.service"));
    let joining_flags = node_flag_set(&joining_exec[2..]);

    let joining_only: BTreeSet<&str> = joining_flags.difference(&vm_flags).copied().collect();
    let vm_only: BTreeSet<&str> = vm_flags.difference(&joining_flags).copied().collect();
    assert_eq!(
        joining_only,
        BTreeSet::from([
            "--consensus.keys-dir",
            "--consensus.peers",
            "--tee-bootstrap-timeout-secs",
            "--upstream",
        ])
    );
    assert!(vm_only.is_empty(), "unexpected VM-only flags: {vm_only:?}");
}

#[test]
fn vm_units_use_explicit_env_ports_for_listen_and_advertise_and_keep_fixed_ports() {
    let vm = vm_units();
    let expand = |raw: &str| {
        raw.replace("${OUTBE_EXTERNAL_IP}", "198.51.100.10")
            .replace("${OUTBE_RPC_BIND_IP}", "10.77.0.81")
            .replace("${OUTBE_RPC_PORT}", "8546")
            .replace("${OUTBE_CONSENSUS_PORT}", "30401")
            .replace("${OUTBE_RETH_P2P_PORT}", "30304")
            .replace("${OUTBE_RETH_DISCOVERY_PORT}", "31304")
            .replace("${OUTBE_RADICLE_PORT}", "8777")
    };

    let validator = expand(unit(&vm, "outbe-validator.service"));
    assert!(validator.contains("--consensus.listen-addr 0.0.0.0:30401"));
    assert!(validator.contains("--port 30304"));
    assert!(validator.contains("--discovery.port 30304"));
    assert!(validator.contains("--discovery.v5.port 31304"));
    assert!(validator.contains("--nat extip:198.51.100.10"));
    assert!(validator.contains("--http.addr 10.77.0.81"));
    assert!(validator.contains("--http.port 8546"));
    // Fixed guest ports stay unchanged.
    assert!(validator.contains("--authrpc.port 8551"));
    assert!(validator.contains("--metrics 127.0.0.1:9101"));
    assert!(validator.contains("--radicle.status-address 127.0.0.1:8876"));

    let radicle = expand(unit(&vm, "outbe-radicle.service"));
    assert!(radicle.contains("--listen 0.0.0.0:8777"));
    assert!(radicle.contains("--advertise 198.51.100.10:8777"));
    assert!(radicle.contains("--status-listen 127.0.0.1:8876"));

    let joining = expand(unit(&vm, "outbe-joining-validator.service"));
    assert!(joining.contains("--http.port 8546"));
    assert!(joining.contains("--consensus.listen-addr 0.0.0.0:30401"));
    assert!(joining.contains("--port 30304"));
    assert!(joining.contains("--discovery.port 30304"));
    assert!(joining.contains("--discovery.v5.port 31304"));
    assert!(joining.contains("--consensus.keys-dir /opt/outbe-chain/keys"));
    assert!(joining.contains("--consensus.peers ${OUTBE_CONSENSUS_PEERS}"));
    assert!(joining.contains("--upstream ${OUTBE_CERTIFIED_RPC}"));

    let full_node = expand(unit(&vm, "outbe-full-node.service"));
    assert!(full_node.contains("--port 30304"));
    assert!(full_node.contains("--discovery.port 30304"));
    assert!(full_node.contains("--discovery.v5.port 31304"));
    assert!(full_node.contains("--http.addr 10.77.0.81"));
    assert!(full_node.contains("--http.port 8546"));
    assert!(full_node.contains("--upstream ${OUTBE_CERTIFIED_RPC}"));
}

#[test]
fn vm_joining_validator_args_match_bare_host_joining_flags_and_full_node_is_keyless() {
    let vm = vm_units();
    let bare_joining = joining_units("/usr/local/bin/outbe-up");

    // VM joining validator must have the EXACT same flag set as bare-host joining_units().
    let bare_exec = exec_start_tokens(unit(&bare_joining, "outbe-validator.service"));
    let vm_join_exec = exec_start_tokens(unit(&vm, "outbe-joining-validator.service"));
    assert_eq!(vm_join_exec[0], "/usr/local/bin/outbe-chain");
    assert_eq!(vm_join_exec[1], "node");
    assert_eq!(
        node_flag_set(&vm_join_exec[2..]),
        node_flag_set(&bare_exec[2..]),
        "VM joining validator must use the exact flags of bare-host joining_units()"
    );

    // VM full-node must be strictly keyless, require --upstream (ADR-005), and omit all
    // validator / key / radicle flags and dependencies.
    let full_content = unit(&vm, "outbe-full-node.service");
    let full_exec = exec_start_tokens(full_content);
    assert_eq!(full_exec[0], "/usr/local/bin/outbe-chain");
    assert_eq!(full_exec[1], "node");
    let full_flags = node_flag_set(&full_exec[2..]);
    assert!(full_flags.contains("--upstream"));
    assert!(full_flags.contains("$OUTBE_CONSENSUS_EXTRA_ARGS"));
    for forbidden in [
        "--validator",
        "--p2p-secret-key",
        "--consensus.signing-key",
        "--validator.evm-key",
        "--consensus.keys-dir",
        "--consensus.listen-addr",
        "--consensus.peers",
        "--radicle.control-socket",
        "--radicle.status-address",
    ] {
        assert!(
            !full_flags.contains(forbidden),
            "full-node unit must not pass {forbidden}"
        );
    }
    assert!(!full_content.contains("/opt/outbe-chain/keys"));
    assert!(!full_content.contains("outbe-radicle.service"));
    assert!(!full_content.contains("wait-validator-dependencies"));
    assert!(full_content.contains("ReadWritePaths=/opt/outbe-chain/validator-0\n"));
    assert!(full_content.contains("ExecStartPost=/usr/local/bin/outbe-vm-agent wait-rpc\n"));
}

#[test]
fn vm_dev_dropins_bind_metrics_on_service_ip_and_preserve_prod_vm_units() {
    let vm = vm_units();
    let dropins = vm_dev_dropins();
    let names: Vec<&str> = dropins.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "outbe-full-node.service.d/20-outbe-dev.conf",
            "outbe-joining-validator.service.d/20-outbe-dev.conf",
            "outbe-validator.service.d/20-outbe-dev.conf",
        ]
    );
    // Console output is not a dev override: both flavors get it from the console drop-ins.
    for dropin in &dropins {
        assert!(
            !dropin.content.contains("StandardOutput="),
            "{}",
            dropin.name
        );
    }

    // Prod base units must remain pinned on 127.0.0.1:9101 and without journal+console.
    for name in [
        "outbe-validator.service",
        "outbe-joining-validator.service",
        "outbe-full-node.service",
    ] {
        let base = unit(&vm, name);
        assert!(
            base.contains("  --metrics 127.0.0.1:9101 \\"),
            "{name}: prod unit must keep loopback metrics bind"
        );
        assert!(
            !base.contains("--metrics ${OUTBE_VM_IP}:9101"),
            "{name}: prod unit must never bind metrics on VM IP"
        );
        assert!(
            !base.contains("StandardOutput=journal+console"),
            "{name}: prod unit must not forward stdout to console"
        );
        assert!(
            !base.contains("--log.stdout.filter"),
            "{name}: prod unit must not set dev stdout log filter"
        );

        let dropin = unit(&dropins, &format!("{name}.d/20-outbe-dev.conf"));
        assert!(dropin.contains("\nExecStart=\nExecStart=/usr/local/bin/outbe-chain node \\"));
        assert!(dropin.contains("  --metrics ${OUTBE_VM_IP}:9101 \\"));
        // The node prints at its default stdout level. Debug records go to the log file.
        assert!(!dropin.contains("--log.stdout.filter"));
        assert!(!dropin.contains("127.0.0.1:9101"));

        // Dev ExecStart differs from canonical prod unit ONLY in --metrics bind address.
        let base_tokens = exec_start_tokens(base);
        let mut expected_dev_tokens = base_tokens.clone();
        let metrics_pos = expected_dev_tokens
            .iter()
            .position(|tok| tok == "127.0.0.1:9101")
            .expect("base ExecStart must contain 127.0.0.1:9101");
        expected_dev_tokens[metrics_pos] = "${OUTBE_VM_IP}:9101".to_owned();
        let dev_tokens = exec_start_tokens(dropin);
        assert_eq!(
            dev_tokens, expected_dev_tokens,
            "{name}: dev ExecStart must differ only in --metrics address"
        );
    }
}

#[test]
fn vm_console_dropins_print_each_service_to_the_console_one_time() {
    let dropins = vm_console_dropins();
    let names: Vec<&str> = dropins.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "dstack-guest-agent.service.d/10-outbe-console.conf",
            "outbe-feeder.service.d/10-outbe-console.conf",
            "outbe-full-node.service.d/10-outbe-console.conf",
            "outbe-hyperlane-validator.service.d/10-outbe-console.conf",
            "outbe-joining-validator.service.d/10-outbe-console.conf",
            "outbe-radicle.service.d/10-outbe-console.conf",
            "outbe-validator.service.d/10-outbe-console.conf",
        ]
    );
    for dropin in &dropins {
        assert_eq!(
            dropin.content,
            "[Service]\nStandardOutput=journal+console\nStandardError=journal+console\n",
            "{}",
            dropin.name
        );
    }
}

fn logical_directives(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut buffer = String::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        if let Some(stripped) = line.strip_suffix('\\') {
            buffer.push_str(stripped);
            buffer.push(' ');
            continue;
        }
        buffer.push_str(line);
        if let Some((key, value)) = buffer.split_once('=') {
            out.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        buffer.clear();
    }
    out
}

fn exec_start_tokens(content: &str) -> Vec<String> {
    let (_, value) = logical_directives(content)
        .into_iter()
        .find(|(key, value)| key == "ExecStart" && !value.is_empty())
        .expect("unit must define ExecStart");
    value.split_whitespace().map(str::to_owned).collect()
}

fn node_flag_set(tokens: &[String]) -> std::collections::BTreeSet<&str> {
    tokens
        .iter()
        .filter(|token| token.starts_with("--") || token.as_str() == "$OUTBE_CONSENSUS_EXTRA_ARGS")
        .map(String::as_str)
        .collect()
}
