//! Canonical systemd policy for Outbe validator hosts.

const SERVICE_TEMPLATE: &str = include_str!("../templates/service.unit");
const ONESHOT_TEMPLATE: &str = include_str!("../templates/oneshot.unit");
const TARGET_TEMPLATE: &str = include_str!("../templates/target.unit");
const TIMER_TEMPLATE: &str = include_str!("../templates/timer.unit");

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

#[derive(Clone, Copy)]
enum Account {
    Root,
    Outbe,
}

struct ServiceSpec {
    name: &'static str,
    description: &'static str,
    account: Account,
    unit_directives: Vec<String>,
    service_directives: Vec<String>,
    sandbox_directives: Vec<String>,
    limit_nofile: u64,
    wanted_by: &'static str,
}

struct OneshotSpec {
    name: &'static str,
    description: &'static str,
    unit_directives: Vec<String>,
    service_directives: Vec<String>,
    sandbox_directives: Vec<String>,
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
            name: "outbe-enclave.service",
            description: "Outbe TEE enclave",
            account: Account::Root,
            unit_directives: lines(&[
                "After=network-online.target",
                "Wants=network-online.target",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                &command("ExecStart=@@OUTBE_UP@@ internal enclave"),
                &command("ExecStartPost=@@OUTBE_UP@@ internal wait-enclave"),
            ]),
            sandbox_directives: lines(&[
                "ReadWritePaths=/opt/outbe-chain/tee",
                "BindPaths=/opt/outbe-chain/tee:/var/lib/outbe/tee",
            ]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&ServiceSpec {
            name: "outbe-radicle.service",
            description: "Outbe validator Radicle sidecar",
            account: Account::Outbe,
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
                "  --listen 0.0.0.0:8776 \\",
                "  --status-listen 127.0.0.1:8876 \\",
                "  --max-validators 128 \\",
                "  --external-inbound-reserve 16 \\",
                "  --advertise ${OUTBE_EXTERNAL_IP}:8776",
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/keys"]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&joining_full_node_spec(outbe_up)),
        render_service(&joining_validator_spec(outbe_up)),
        render_service(&ServiceSpec {
            name: "outbe-feeder.service",
            description: "Outbe validator price feeder",
            account: Account::Outbe,
            unit_directives: lines(&[
                "After=outbe-validator.service",
                "Wants=outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "RuntimeDirectory=outbe-feeder",
                "RuntimeDirectoryMode=0700",
                "StateDirectory=outbe-feeder",
                "StateDirectoryMode=0700",
                &command("ExecStartPre=@@OUTBE_UP@@ internal wait-rpc"),
                &command("ExecStart=@@OUTBE_UP@@ internal feeder"),
            ]),
            sandbox_directives: Vec::new(),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_oneshot(&OneshotSpec {
            name: "outbe-tee-renew.service",
            description: "Outbe TEE lease renewal",
            unit_directives: lines(&[
                "After=network-online.target outbe-enclave.service",
                "Requisite=outbe-enclave.service",
            ]),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "ExecStart=/opt/outbe-chain/outbe-cli tee renew \\",
                "  --enclave-socket 127.0.0.1:17000 \\",
                "  --node-data-dir /opt/outbe-chain/validator-0/data \\",
                "  --reth-p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\",
                "  --private-key-file /opt/outbe-chain/keys/evm-key.hex \\",
                "  --rpc-url ${OUTBE_CERTIFIED_RPC}",
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/validator-0/data"]),
        }),
        render_timer("Outbe TEE lease renewal check"),
        render_target("Outbe validator stack"),
    ];
    units.sort_by(|left, right| left.name.cmp(&right.name));
    units
}

/// Render founding-validator units with the same policy and host-local paths.
pub fn founding_units(config: FoundingConfig<'_>) -> Vec<UnitFile> {
    let validator_dir = format!("{}/validator-{}", config.base_dir, config.validator_index);
    let mut units = vec![
        render_service(&ServiceSpec {
            name: "outbe-enclave.service",
            description: "Outbe TEE enclave",
            account: Account::Root,
            unit_directives: lines(&[
                "After=network-online.target",
                "Wants=network-online.target",
                "PartOf=outbe-validator.target",
            ]),
            service_directives: lines(&[
                &format!("WorkingDirectory={validator_dir}"),
                &format!("ExecStart={validator_dir}/run-enclave.sh"),
            ]),
            sandbox_directives: lines(&[
                &format!("ReadWritePaths={}", config.base_dir),
                &format!("BindPaths={validator_dir}/tee:/var/lib/outbe/tee"),
            ]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&founding_service(&FoundingService {
            name: "outbe-radicle.service",
            description: "Outbe validator Radicle sidecar",
            script: "run-radicle.sh",
            validator_dir: &validator_dir,
            base_dir: config.base_dir,
            unit_directives: lines(&[
                "After=network-online.target outbe-enclave.service",
                "Wants=network-online.target outbe-enclave.service",
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
                "After=network-online.target outbe-enclave.service outbe-radicle.service",
                "Wants=network-online.target outbe-enclave.service outbe-radicle.service",
                "PartOf=outbe-validator.target",
            ]),
            limit_nofile: 1_048_576,
        })),
        render_service(&founding_service(&FoundingService {
            name: "outbe-feeder.service",
            description: "Outbe validator price feeder",
            script: "run-feeder.sh",
            validator_dir: &validator_dir,
            base_dir: config.base_dir,
            unit_directives: lines(&[
                "After=network-online.target outbe-validator.service",
                "Wants=network-online.target outbe-validator.service",
                "PartOf=outbe-validator.target",
            ]),
            limit_nofile: 65_535,
        })),
        render_oneshot(&OneshotSpec {
            name: "outbe-tee-renew.service",
            description: "Outbe TEE lease renewal",
            unit_directives: lines(&[
                "After=network-online.target outbe-enclave.service outbe-validator.service",
                "Requisite=outbe-enclave.service outbe-validator.service",
            ]),
            service_directives: lines(&[
                &format!("WorkingDirectory={validator_dir}"),
                &format!("ExecStart={validator_dir}/run-tee-renew.sh"),
            ]),
            sandbox_directives: lines(&[&format!("ReadWritePaths={}", config.base_dir)]),
        }),
        render_timer("Outbe TEE lease renewal check"),
        render_target("Outbe validator stack"),
    ];
    units.sort_by(|left, right| left.name.cmp(&right.name));
    units
}

fn joining_full_node_spec(outbe_up: &str) -> ServiceSpec {
    ServiceSpec {
        name: "outbe-full-node.service",
        description: "Outbe certified full node",
        account: Account::Outbe,
        unit_directives: lines(&[
            "After=network-online.target",
            "Wants=network-online.target",
            "Conflicts=outbe-validator.service",
        ]),
        service_directives: lines(&[
            "EnvironmentFile=/opt/outbe-chain/validator.env",
            "Environment=RUST_MIN_STACK=16777216",
            "ExecStart=/opt/outbe-chain/outbe-chain node \\",
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
            "  --port 30303 \\",
            "  --discovery.port 30303 \\",
            "  --nat extip:${OUTBE_EXTERNAL_IP} \\",
            "  --discovery.v5.addr 0.0.0.0 \\",
            "  --discovery.v5.port 31303 \\",
            "  --p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\",
            "  --bootnodes ${OUTBE_BOOTNODES} \\",
            "  $OUTBE_CONSENSUS_EXTRA_ARGS \\",
            "  --metrics 127.0.0.1:9101 \\",
            "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\",
            "  --log.file.directory /opt/outbe-chain/validator-0/logs \\",
            "  --upstream ${OUTBE_CERTIFIED_RPC}",
            &format!("ExecStartPost={outbe_up} internal wait-rpc"),
        ]),
        sandbox_directives: lines(&[
            "ReadWritePaths=/opt/outbe-chain/keys /opt/outbe-chain/validator-0",
        ]),
        limit_nofile: 1_048_576,
        wanted_by: "multi-user.target",
    }
}

fn joining_validator_spec(outbe_up: &str) -> ServiceSpec {
    ServiceSpec {
        name: "outbe-validator.service",
        description: "Outbe validator",
        account: Account::Outbe,
        unit_directives: lines(&[
            "After=network-online.target outbe-enclave.service outbe-radicle.service",
            "Wants=network-online.target outbe-enclave.service outbe-radicle.service",
            "Conflicts=outbe-full-node.service",
            "PartOf=outbe-validator.target",
        ]),
        service_directives: lines(&[
            "EnvironmentFile=/opt/outbe-chain/validator.env",
            "Environment=RUST_MIN_STACK=16777216",
            &format!("ExecStartPre={outbe_up} internal wait-validator-dependencies"),
            "ExecStart=/opt/outbe-chain/outbe-chain node \\",
            "  --tee-session-mode ${OUTBE_TEE_SESSION_MODE} \\",
            "  --chain /opt/outbe-chain/genesis.json \\",
            "  --datadir /opt/outbe-chain/validator-0/data \\",
            "  --engine.persistence-threshold 0 \\",
            "  --engine.memory-block-buffer-target 0 \\",
            "  --consensus.storage-dir /opt/outbe-chain/validator-0/consensus \\",
            "  --tee-enclave-socket 127.0.0.1:17000 \\",
            "  --http \\",
            "  --http.addr ${OUTBE_RPC_BIND_IP} \\",
            "  --http.port 8545 \\",
            "  --http.api eth,net,web3,outbe \\",
            "  --authrpc.port 8551 \\",
            "  --port 30303 \\",
            "  --discovery.port 30303 \\",
            "  --nat extip:${OUTBE_EXTERNAL_IP} \\",
            "  --discovery.v5.addr 0.0.0.0 \\",
            "  --discovery.v5.port 31303 \\",
            "  --p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\",
            "  --bootnodes ${OUTBE_BOOTNODES} \\",
            "  --metrics 127.0.0.1:9101 \\",
            "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\",
            "  --log.file.directory /opt/outbe-chain/validator-0/logs \\",
            "  --validator \\",
            "  --consensus.signing-key /opt/outbe-chain/keys/signing-key.hex \\",
            "  --validator.evm-key /opt/outbe-chain/keys/evm-key.hex \\",
            "  --consensus.keys-dir /opt/outbe-chain/keys \\",
            "  --consensus.listen-addr 0.0.0.0:30400 \\",
            "  --consensus.peers ${OUTBE_CONSENSUS_PEERS} \\",
            "  $OUTBE_CONSENSUS_EXTRA_ARGS \\",
            "  --upstream ${OUTBE_CERTIFIED_RPC} \\",
            "  --radicle.control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\",
            "  --radicle.status-address 127.0.0.1:8876",
            &format!("ExecStartPost={outbe_up} internal wait-rpc"),
        ]),
        sandbox_directives: lines(&[
            "ReadWritePaths=/opt/outbe-chain/keys /opt/outbe-chain/validator-0",
        ]),
        limit_nofile: 1_048_576,
        wanted_by: "outbe-validator.target",
    }
}

fn founding_service(spec: &FoundingService<'_>) -> ServiceSpec {
    ServiceSpec {
        name: spec.name,
        description: spec.description,
        account: Account::Outbe,
        unit_directives: spec.unit_directives.clone(),
        service_directives: lines(&[
            &format!("WorkingDirectory={}", spec.validator_dir),
            &format!("ExecStart={}/{}", spec.validator_dir, spec.script),
        ]),
        sandbox_directives: lines(&[&format!("ReadWritePaths={}", spec.base_dir)]),
        limit_nofile: spec.limit_nofile,
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
            ("ACCOUNT", account(spec.account).to_owned()),
            ("SERVICE_DIRECTIVES", join(&spec.service_directives)),
            ("LIMIT_NOFILE", spec.limit_nofile.to_string()),
            ("SANDBOX_DIRECTIVES", join(&spec.sandbox_directives)),
            ("WANTED_BY", spec.wanted_by.to_owned()),
        ],
    )
}

fn render_oneshot(spec: &OneshotSpec) -> UnitFile {
    render_unit(
        spec.name,
        ONESHOT_TEMPLATE,
        &[
            ("DESCRIPTION", spec.description.to_owned()),
            ("UNIT_DIRECTIVES", join(&spec.unit_directives)),
            ("SERVICE_DIRECTIVES", join(&spec.service_directives)),
            ("SANDBOX_DIRECTIVES", join(&spec.sandbox_directives)),
        ],
    )
}

fn render_timer(description: &str) -> UnitFile {
    render_unit(
        "outbe-tee-renew.timer",
        TIMER_TEMPLATE,
        &[("DESCRIPTION", description.to_owned())],
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
                    "Wants=outbe-enclave.service",
                    "Wants=outbe-radicle.service",
                    "Wants=outbe-validator.service",
                    "Wants=outbe-feeder.service",
                    "After=outbe-enclave.service outbe-radicle.service",
                    "After=outbe-validator.service outbe-feeder.service",
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

const fn account(account: Account) -> &'static str {
    match account {
        Account::Root => "User=root",
        Account::Outbe => "User=outbe\nGroup=outbe",
    }
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
            "outbe-enclave.service",
            "outbe-radicle.service",
            "outbe-validator.service",
            "outbe-feeder.service",
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
        for unit in joining.iter().chain(&founding) {
            assert!(!unit.content.contains("@@"));
            assert!(!unit.content.contains("%i"));
            assert!(!unit.content.contains("@.service"));
        }
        let full_node = unit(&joining, "outbe-full-node.service");
        assert!(full_node.contains("--upstream"));
        assert!(!full_node.contains("wait-full-node-dependencies"));
        assert!(!full_node.contains("--validator"));
        assert!(!full_node.contains("radicle.control-socket"));

        let validator = unit(&joining, "outbe-validator.service");
        assert!(validator.contains("wait-validator-dependencies"));
        assert!(validator.contains("--tee-session-mode"));
        assert!(validator.contains("--validator"));
        assert!(validator.contains("radicle.control-socket"));
        assert!(unit(&founding, "outbe-validator.service")
            .contains("/opt/outbe-chain/validator-2/run-node.sh"));
    }

    fn unit<'a>(units: &'a [UnitFile], name: &str) -> &'a str {
        &units.iter().find(|unit| unit.name == name).unwrap().content
    }
}
