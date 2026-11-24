//! Canonical systemd policy for Outbe validator hosts.

const SERVICE_TEMPLATE: &str = include_str!("../templates/service.unit");
const TARGET_TEMPLATE: &str = include_str!("../templates/target.unit");

/// One rendered systemd asset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitFile {
    pub name: String,
    pub content: String,
}

/// Host-specific values needed by the founding-validator units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FoundingConfig<'a> {
    pub base_dir: &'a str,
    pub validator_index: usize,
}

struct ServiceSpec {
    name: &'static str,
    description: &'static str,
    unit_directives: Vec<String>,
    service_directives: Vec<String>,
    sandbox_directives: Vec<String>,
    limit_nofile: u64,
    timeout_start_sec: u64,
    wanted_by: &'static str,
}

struct FoundingService<'a> {
    name: &'static str,
    description: &'static str,
    script: &'static str,
    validator_dir: &'a str,
    base_dir: &'a str,
    unit_directives: Vec<String>,
    limit_nofile: u64,
}

/// Render the joining-validator units embedded by `outbe-up` and packaged in releases.
pub fn joining_units(outbe_up: &str) -> Vec<UnitFile> {
    let command = |source: &str| source.replace("@@OUTBE_UP@@", outbe_up);
    let mut units = vec![
        render_service(&ServiceSpec {
            name: "outbe-radicle.service",
            description: "Outbe validator Radicle sidecar",
            unit_directives: lines(&[
                "After=network-online.target",
                "Wants=network-online.target",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "ExecStartPre=/usr/bin/rm -f /opt/outbe-chain/keys/radicle/node/outbe-control.sock",
                "ExecStart=/opt/outbe-chain/outbe-radicle \\",
                "  --home /opt/outbe-chain/keys/radicle \\",
                "  --control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\",
                "  --listen 0.0.0.0:${OUTBE_RADICLE_PORT} \\",
                "  --status-listen 127.0.0.1:8876 \\",
                "  --max-validators 128 \\",
                "  --external-inbound-reserve 16 \\",
                "  --advertise ${OUTBE_EXTERNAL_IP}:${OUTBE_RADICLE_PORT}",
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/keys"]),
            limit_nofile: 65_535,
            timeout_start_sec: 150,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&joining_validator_spec(outbe_up)),
        render_service(&ServiceSpec {
            name: "outbe-feeder.service",
            description: "Outbe validator price feeder",
            unit_directives: lines(&[
                "After=outbe-validator.service",
                "Wants=outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "RuntimeDirectory=outbe-feeder",
                "RuntimeDirectoryMode=0700",
                "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/feeder",
                &command("ExecStartPre=@@OUTBE_UP@@ internal wait-rpc"),
                &command("ExecStart=@@OUTBE_UP@@ internal feeder"),
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/state/feeder"]),
            limit_nofile: 65_535,
            timeout_start_sec: 150,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&ServiceSpec {
            name: "outbe-hyperlane-validator.service",
            description: "Outbe Hyperlane validator",
            unit_directives: lines(&[
                "After=outbe-validator.service",
                "Wants=outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "RuntimeDirectory=outbe-hyperlane-validator",
                "RuntimeDirectoryMode=0700",
                "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/hyperlane",
                "Environment=OUTBE_HYPERLANE_WAIT_FOR_ACTIVE=1",
                &command("ExecStartPre=@@OUTBE_UP@@ internal wait-rpc"),
                &command("ExecStart=@@OUTBE_UP@@ internal hyperlane"),
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/state/hyperlane"]),
            limit_nofile: 65_535,
            timeout_start_sec: 150,
            wanted_by: "outbe-validator.target",
        }),
        render_target("Outbe validator stack"),
    ];
    units.sort_by(|left, right| left.name.cmp(&right.name));
    units
}

/// Render founding-validator units with the same policy and host-local paths.
pub fn founding_units(config: FoundingConfig<'_>) -> Vec<UnitFile> {
    let validator_dir = format!("{}/validator-{}", config.base_dir, config.validator_index);
    let mut units = vec![
        render_service(&founding_service(&FoundingService {
            name: "outbe-radicle.service",
            description: "Outbe validator Radicle sidecar",
            script: "run-radicle.sh",
            validator_dir: &validator_dir,
            base_dir: config.base_dir,
            unit_directives: lines(&[
                "After=network-online.target",
                "Wants=network-online.target",
                "PartOf=outbe-validator.target",
            ]),
            limit_nofile: 65_535,
        })),
        render_service(&founding_service(&FoundingService {
            name: "outbe-validator.service",
            description: "Outbe validator",
            script: "run-node.sh",
            validator_dir: &validator_dir,
            base_dir: config.base_dir,
            unit_directives: lines(&[
                "After=network-online.target outbe-radicle.service",
                "Wants=network-online.target outbe-radicle.service",
                "PartOf=outbe-validator.target",
            ]),
            limit_nofile: 1_048_576,
        })),
        render_service(&ServiceSpec {
            name: "outbe-feeder.service",
            description: "Outbe validator price feeder",
            unit_directives: lines(&[
                "After=network-online.target outbe-validator.service",
                "Wants=network-online.target outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                &format!("WorkingDirectory={validator_dir}"),
                &format!(
                    "Environment=STATE_DIRECTORY={}/state/feeder",
                    config.base_dir
                ),
                &format!("ExecStart={validator_dir}/run-feeder.sh"),
            ]),
            sandbox_directives: lines(&[&format!("ReadWritePaths={}", config.base_dir)]),
            limit_nofile: 65_535,
            timeout_start_sec: 150,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&ServiceSpec {
            name: "outbe-hyperlane-validator.service",
            description: "Outbe Hyperlane validator",
            unit_directives: lines(&[
                "After=network-online.target outbe-validator.service",
                "Wants=network-online.target outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                &format!("WorkingDirectory={validator_dir}"),
                "RuntimeDirectory=outbe-hyperlane-validator",
                "RuntimeDirectoryMode=0700",
                &format!(
                    "Environment=STATE_DIRECTORY={}/state/hyperlane",
                    config.base_dir
                ),
                &format!("ExecStart={validator_dir}/run-hyperlane.sh"),
            ]),
            sandbox_directives: lines(&[&format!("ReadWritePaths={}", config.base_dir)]),
            limit_nofile: 65_535,
            timeout_start_sec: 150,
            wanted_by: "outbe-validator.target",
        }),
        render_target("Outbe validator stack"),
    ];
    units.sort_by(|left, right| left.name.cmp(&right.name));
    units
}

fn joining_validator_spec(outbe_up: &str) -> ServiceSpec {
    ServiceSpec {
        name: "outbe-validator.service",
        description: "Outbe validator",
        unit_directives: lines(&[
            "After=network-online.target outbe-radicle.service",
            "Wants=network-online.target outbe-radicle.service",
            "PartOf=outbe-validator.target",
        ]),
        service_directives: lines(&[
            "EnvironmentFile=/opt/outbe-chain/validator.env",
            "Environment=RUST_MIN_STACK=16777216",
            &format!("ExecStartPre={outbe_up} internal wait-validator-dependencies"),
            "ExecStart=/opt/outbe-chain/outbe-chain node \\",
            "  --config /opt/outbe-chain/reth.toml \\",
            "  --chain /opt/outbe-chain/genesis.json \\",
            "  --datadir /opt/outbe-chain/validator-0/data \\",
            "  --engine.persistence-threshold 0 \\",
            "  --engine.memory-block-buffer-target 0 \\",
            "  --consensus.storage-dir /opt/outbe-chain/validator-0/consensus \\",
            "  --http \\",
            "  --http.addr ${OUTBE_RPC_BIND_IP} \\",
            "  --http.port 8545 \\",
            "  --http.api eth,net,web3,outbe \\",
            "  --authrpc.port 8551 \\",
            "  --port ${OUTBE_RETH_P2P_PORT} \\",
            "  --discovery.port ${OUTBE_RETH_P2P_PORT} \\",
            "  --nat extip:${OUTBE_EXTERNAL_IP} \\",
            "  --discovery.v5.addr 0.0.0.0 \\",
            "  --discovery.v5.port ${OUTBE_RETH_DISCOVERY_PORT} \\",
            "  --p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\",
            "  --bootnodes ${OUTBE_BOOTNODES} \\",
            "  --metrics 127.0.0.1:9101 \\",
            "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\",
            "  --log.file.directory /opt/outbe-chain/validator-0/logs \\",
            "  --validator \\",
            "  --consensus.signing-key /opt/outbe-chain/keys/signing-key.hex \\",
            "  --validator.evm-key /opt/outbe-chain/keys/evm-key.hex \\",
            "  --consensus.keys-dir /opt/outbe-chain/keys \\",
            "  --consensus.listen-addr 0.0.0.0:${OUTBE_CONSENSUS_PORT} \\",
            "  --tee-bootstrap-timeout-secs 300 \\",
            "  --consensus.peers ${OUTBE_CONSENSUS_PEERS} \\",
            "  $OUTBE_CONSENSUS_EXTRA_ARGS \\",
            "  --upstream ${OUTBE_CERTIFIED_RPC} \\",
            "  --radicle.control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\",
            "  --radicle.status-address 127.0.0.1:8876",
            &format!("ExecStartPost={outbe_up} internal wait-rpc --timeout 0"),
        ]),
        sandbox_directives: lines(&[
            "ReadWritePaths=/opt/outbe-chain/keys /opt/outbe-chain/validator-0",
        ]),
        limit_nofile: 1_048_576,
        timeout_start_sec: 0,
        wanted_by: "outbe-validator.target",
    }
}

fn founding_service(spec: &FoundingService<'_>) -> ServiceSpec {
    ServiceSpec {
        name: spec.name,
        description: spec.description,
        unit_directives: spec.unit_directives.clone(),
        service_directives: lines(&[
            &format!("WorkingDirectory={}", spec.validator_dir),
            &format!("ExecStart={}/{}", spec.validator_dir, spec.script),
        ]),
        sandbox_directives: lines(&[&format!("ReadWritePaths={}", spec.base_dir)]),
        limit_nofile: spec.limit_nofile,
        timeout_start_sec: 150,
        wanted_by: "outbe-validator.target",
    }
}
fn render_service(spec: &ServiceSpec) -> UnitFile {
    render_unit(
        spec.name,
        SERVICE_TEMPLATE,
        &[
            ("DESCRIPTION", spec.description.to_owned()),
            ("UNIT_DIRECTIVES", join(&spec.unit_directives)),
            ("ACCOUNT", "User=outbe\nGroup=outbe".to_owned()),
            ("SERVICE_DIRECTIVES", join(&spec.service_directives)),
            ("LIMIT_NOFILE", spec.limit_nofile.to_string()),
            ("TIMEOUT_START_SEC", spec.timeout_start_sec.to_string()),
            ("SANDBOX_DIRECTIVES", join(&spec.sandbox_directives)),
            ("WANTED_BY", spec.wanted_by.to_owned()),
        ],
    )
}

fn render_target(description: &str) -> UnitFile {
    render_unit(
        "outbe-validator.target",
        TARGET_TEMPLATE,
        &[
            ("DESCRIPTION", description.to_owned()),
            (
                "UNIT_DIRECTIVES",
                join(&lines(&[
                    "Wants=outbe-radicle.service",
                    "Wants=outbe-validator.service",
                    "Wants=outbe-feeder.service",
                    "Wants=outbe-hyperlane-validator.service",
                    "After=outbe-radicle.service",
                    "After=outbe-validator.service outbe-feeder.service",
                    "After=outbe-hyperlane-validator.service",
                ])),
            ),
        ],
    )
}

fn render_unit(name: &str, template: &str, values: &[(&str, String)]) -> UnitFile {
    let mut content = template.to_owned();
    for (key, value) in values {
        content = content.replace(&format!("@@{key}@@"), value);
    }
    assert!(!content.contains("@@"), "unresolved token in {name}");
    UnitFile {
        name: name.to_owned(),
        content,
    }
}

fn lines(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn join(values: &[String]) -> String {
    values.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMON_POLICY: &[&str] = &[
        "Restart=always",
        "RestartMode=direct",
        "RestartSec=5",
        "RestartSteps=5",
        "RestartMaxDelaySec=60",
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
        assert!(validator.contains("--config /opt/outbe-chain/reth.toml"));
        assert!(validator.contains("${OUTBE_CONSENSUS_PORT}"));
        assert!(validator.contains("--tee-bootstrap-timeout-secs 300"));
        assert!(validator.contains("TimeoutStartSec=0"));
        assert!(validator.contains("internal wait-rpc --timeout 0"));
        assert!(unit(&joining, "outbe-radicle.service").contains("${OUTBE_RADICLE_PORT}"));
        assert!(unit(&founding, "outbe-validator.service")
            .contains("/opt/outbe-chain/validator-2/run-node.sh"));
        for unit in &founding {
            if unit.name.ends_with(".service") {
                assert!(
                    unit.content.contains("TimeoutStartSec=150"),
                    "{}",
                    unit.name
                );
            }
        }
        for name in [
            "outbe-radicle.service",
            "outbe-feeder.service",
            "outbe-hyperlane-validator.service",
        ] {
            assert!(
                unit(&joining, name).contains("TimeoutStartSec=150"),
                "{name}"
            );
        }

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
}
