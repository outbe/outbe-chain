//! Canonical systemd policy for Outbe validator hosts.

const SERVICE_TEMPLATE: &str = include_str!("../templates/service.unit");
const TARGET_TEMPLATE: &str = include_str!("../templates/target.unit");
const HOST_BIN_DIR: &str = "/opt/outbe-chain";
const VM_BIN_DIR: &str = "/usr/local/bin";
const VM_AGENT: &str = "/usr/local/bin/outbe-vm-agent";

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

struct ManagedProfile<'a> {
    bin_dir: &'a str,
    helper_prefix: String,
    internal_bin_dir: Option<&'a str>,
    is_vm: bool,
    joining: bool,
}

/// Render the joining-validator units embedded by `outbe-up` and packaged in releases.
pub fn joining_units(outbe_up: &str) -> Vec<UnitFile> {
    managed_units(&ManagedProfile {
        bin_dir: HOST_BIN_DIR,
        helper_prefix: format!("{outbe_up} internal"),
        internal_bin_dir: None,
        is_vm: false,
        joining: true,
    })
}

/// Render static units for a VM image that runs binaries from `/usr/local/bin`
/// and reads node data from `/opt/outbe-chain`.
///
/// `outbe-validator.target` starts `outbe-vm-boot.service`, which reads `OUTBE_NODE_ROLE` from
/// `/opt/outbe-chain/validator.env` and starts only that role's services. All guest helper
/// invocations use `/usr/local/bin/outbe-vm-agent` (never `outbe-up`).
pub fn vm_units() -> Vec<UnitFile> {
    managed_units(&ManagedProfile {
        bin_dir: VM_BIN_DIR,
        helper_prefix: VM_AGENT.to_owned(),
        internal_bin_dir: Some(VM_BIN_DIR),
        is_vm: true,
        joining: false,
    })
}

fn managed_units(profile: &ManagedProfile<'_>) -> Vec<UnitFile> {
    let internal_cmd = |subcommand: &str| match profile.internal_bin_dir {
        Some(dir) => format!(
            "ExecStart={} {subcommand} --bin-dir {dir}",
            profile.helper_prefix
        ),
        None => format!("ExecStart={} {subcommand}", profile.helper_prefix),
    };
    let wait_rpc = format!("ExecStartPre={} wait-rpc", profile.helper_prefix);

    let with_vm_boot = |mut directives: Vec<String>| {
        if profile.is_vm {
            directives.push("Requires=outbe-vm-boot.service".to_owned());
            directives.push("After=outbe-vm-boot.service".to_owned());
        }
        directives
    };

    let mut hyperlane_directives = lines(&[
        "EnvironmentFile=/opt/outbe-chain/validator.env",
        "RuntimeDirectory=outbe-hyperlane-validator",
        "RuntimeDirectoryMode=0700",
        "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/hyperlane",
    ]);
    if !profile.is_vm && profile.joining {
        hyperlane_directives.push("Environment=OUTBE_HYPERLANE_WAIT_FOR_ACTIVE=1".to_owned());
    }
    hyperlane_directives.push(wait_rpc.clone());
    hyperlane_directives.push(internal_cmd("hyperlane"));

    let radicle_listen = if profile.is_vm {
        "  --listen 0.0.0.0:${OUTBE_RADICLE_PORT} \\"
    } else {
        "  --listen 0.0.0.0:8776 \\"
    };
    let radicle_advertise = if profile.is_vm {
        "  --advertise ${OUTBE_EXTERNAL_IP}:${OUTBE_RADICLE_PORT}"
    } else {
        "  --advertise ${OUTBE_EXTERNAL_IP}:8776"
    };

    let sidecar_unit_directives = if profile.is_vm {
        // Order after whichever validator service `vm-boot` started without pulling in both.
        with_vm_boot(lines(&[
            "After=outbe-validator.service outbe-joining-validator.service",
            "PartOf=outbe-validator.target",
        ]))
    } else {
        lines(&[
            "After=outbe-validator.service",
            "Wants=outbe-validator.service",
            "PartOf=outbe-validator.target",
        ])
    };

    let mut units = vec![
        render_service(&ServiceSpec {
            name: "outbe-radicle.service",
            description: "Outbe validator Radicle sidecar",
            unit_directives: with_vm_boot(lines(&[
                "After=network-online.target",
                "Wants=network-online.target",
                "PartOf=outbe-validator.target",
            ])),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "ExecStartPre=/usr/bin/rm -f /opt/outbe-chain/keys/radicle/node/outbe-control.sock",
                &format!("ExecStart={}/outbe-radicle \\", profile.bin_dir),
                "  --home /opt/outbe-chain/keys/radicle \\",
                "  --control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\",
                radicle_listen,
                "  --status-listen 127.0.0.1:8876 \\",
                "  --max-validators 128 \\",
                "  --external-inbound-reserve 16 \\",
                radicle_advertise,
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/keys"]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&managed_validator_spec(
            profile,
            "outbe-validator.service",
            "Outbe validator",
            profile.joining,
        )),
        render_service(&ServiceSpec {
            name: "outbe-feeder.service",
            description: "Outbe validator price feeder",
            unit_directives: sidecar_unit_directives.clone(),
            service_directives: lines(&[
                "EnvironmentFile=/opt/outbe-chain/validator.env",
                "RuntimeDirectory=outbe-feeder",
                "RuntimeDirectoryMode=0700",
                "Environment=STATE_DIRECTORY=/opt/outbe-chain/state/feeder",
                &wait_rpc,
                &internal_cmd("feeder"),
            ]),
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/state/feeder"]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_service(&ServiceSpec {
            name: "outbe-hyperlane-validator.service",
            description: "Outbe Hyperlane validator",
            unit_directives: sidecar_unit_directives,
            service_directives: hyperlane_directives,
            sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/state/hyperlane"]),
            limit_nofile: 65_535,
            wanted_by: "outbe-validator.target",
        }),
        render_managed_target(profile),
    ];
    if profile.is_vm {
        units.push(render_service(&managed_validator_spec(
            profile,
            "outbe-joining-validator.service",
            "Outbe joining validator",
            true,
        )));
        units.push(render_service(&managed_full_node_spec(profile)));
        units.push(render_vm_boot_service(
            &profile.helper_prefix,
            profile.bin_dir,
        ));
    }
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
            wanted_by: "outbe-validator.target",
        }),
        render_target("Outbe validator stack"),
    ];
    units.sort_by(|left, right| left.name.cmp(&right.name));
    units
}

/// Single-source Reth / `outbe-chain node` base arguments shared by every managed node role
/// (bare-host joining validator, VM founder, VM joining validator, and VM keyless full node).
fn common_node_exec_lines(
    bin_dir: &str,
    explicit_ports: bool,
    p2p_secret_key: Option<&str>,
) -> Vec<String> {
    let reth_port_line = if explicit_ports {
        "  --port ${OUTBE_RETH_PORT} \\"
    } else {
        "  --port 30303 \\"
    };
    let discovery_port_line = if explicit_ports {
        "  --discovery.port ${OUTBE_RETH_PORT} \\"
    } else {
        "  --discovery.port 30303 \\"
    };
    let discv5_port_line = if explicit_ports {
        "  --discovery.v5.port ${OUTBE_DISCV5_PORT} \\"
    } else {
        "  --discovery.v5.port 31303 \\"
    };
    let mut args = lines(&[
        &format!("ExecStart={bin_dir}/outbe-chain node \\"),
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
        reth_port_line,
        discovery_port_line,
        "  --nat extip:${OUTBE_EXTERNAL_IP} \\",
        "  --discovery.v5.addr 0.0.0.0 \\",
        discv5_port_line,
    ]);
    if let Some(key_path) = p2p_secret_key {
        args.push(format!("  --p2p-secret-key {key_path} \\"));
    }
    args.extend(lines(&[
        "  --bootnodes ${OUTBE_BOOTNODES} \\",
        "  --metrics 127.0.0.1:9101 \\",
        "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\",
        "  --log.file.directory /opt/outbe-chain/validator-0/logs \\",
    ]));
    args
}

fn managed_validator_spec(
    profile: &ManagedProfile<'_>,
    name: &'static str,
    description: &'static str,
    joining: bool,
) -> ServiceSpec {
    let consensus_listen_line = if profile.is_vm {
        "  --consensus.listen-addr 0.0.0.0:${OUTBE_CONSENSUS_PORT} \\"
    } else {
        "  --consensus.listen-addr 0.0.0.0:30400 \\"
    };
    let mut directives = lines(&[
        "EnvironmentFile=/opt/outbe-chain/validator.env",
        "Environment=RUST_MIN_STACK=16777216",
        &format!(
            "ExecStartPre={} wait-validator-dependencies",
            profile.helper_prefix
        ),
    ]);
    directives.extend(common_node_exec_lines(
        profile.bin_dir,
        profile.is_vm,
        Some("/opt/outbe-chain/keys/reth-p2p-secret.hex"),
    ));
    directives.extend(lines(&[
        "  --validator \\",
        "  --consensus.signing-key /opt/outbe-chain/keys/signing-key.hex \\",
        "  --validator.evm-key /opt/outbe-chain/keys/evm-key.hex \\",
    ]));
    if joining {
        directives.push("  --consensus.keys-dir /opt/outbe-chain/keys \\".to_owned());
    }
    directives.push(consensus_listen_line.to_owned());
    if joining {
        directives.push("  --consensus.peers ${OUTBE_CONSENSUS_PEERS} \\".to_owned());
    }
    directives.push("  $OUTBE_CONSENSUS_EXTRA_ARGS \\".to_owned());
    if joining {
        directives.push("  --upstream ${OUTBE_CERTIFIED_RPC} \\".to_owned());
    }
    directives.extend(lines(&[
        "  --radicle.control-socket /opt/outbe-chain/keys/radicle/node/outbe-control.sock \\",
        "  --radicle.status-address 127.0.0.1:8876",
        &format!("ExecStartPost={} wait-rpc", profile.helper_prefix),
    ]));

    let mut unit_directives = lines(&[
        "After=network-online.target outbe-radicle.service",
        "Wants=network-online.target outbe-radicle.service",
        "PartOf=outbe-validator.target",
    ]);
    if profile.is_vm {
        unit_directives.push("Requires=outbe-vm-boot.service".to_owned());
        unit_directives.push("After=outbe-vm-boot.service".to_owned());
    }

    ServiceSpec {
        name,
        description,
        unit_directives,
        service_directives: directives,
        sandbox_directives: lines(&[
            "ReadWritePaths=/opt/outbe-chain/keys /opt/outbe-chain/validator-0",
        ]),
        limit_nofile: 1_048_576,
        wanted_by: "outbe-validator.target",
    }
}

fn managed_full_node_spec(profile: &ManagedProfile<'_>) -> ServiceSpec {
    // Canonical full-node flag set (no bare-host analogue yet): ADR-005 requires `--upstream`
    // whenever `--validator` is omitted; zero validator, key, or Radicle flags are passed.
    // Reth generates its own ephemeral P2P key inside `--datadir`, so "keyless" here means
    // zero provisioned keys on the data disk or operator host.
    let mut directives = lines(&[
        "EnvironmentFile=/opt/outbe-chain/validator.env",
        "Environment=RUST_MIN_STACK=16777216",
    ]);
    directives.extend(common_node_exec_lines(profile.bin_dir, profile.is_vm, None));
    directives.extend(lines(&[
        "  $OUTBE_CONSENSUS_EXTRA_ARGS \\",
        "  --upstream ${OUTBE_CERTIFIED_RPC}",
        &format!("ExecStartPost={} wait-rpc", profile.helper_prefix),
    ]));

    let unit_directives = lines(&[
        "After=network-online.target",
        "Wants=network-online.target",
        "PartOf=outbe-validator.target",
        "Requires=outbe-vm-boot.service",
        "After=outbe-vm-boot.service",
    ]);

    ServiceSpec {
        name: "outbe-full-node.service",
        description: "Outbe full node",
        unit_directives,
        service_directives: directives,
        sandbox_directives: lines(&["ReadWritePaths=/opt/outbe-chain/validator-0"]),
        limit_nofile: 1_048_576,
        wanted_by: "outbe-validator.target",
    }
}

fn render_vm_boot_service(helper_prefix: &str, bin_dir: &str) -> UnitFile {
    // Run before all node daemons to verify the unmeasured data disk, configure the static IPv4
    // drop-in under /run/systemd/network, read `OUTBE_NODE_ROLE` from `validator.env`, and start
    // the corresponding role services via `systemctl start --no-block`.
    let content = format!(
        concat!(
            "[Unit]\n",
            "Description=Outbe VM data disk verification and boot preparation\n",
            "Requires=outbe-data-mount.service\n",
            "After=local-fs.target systemd-networkd.service outbe-data-mount.service\n",
            "PartOf=outbe-validator.target\n",
            "\n",
            "[Service]\n",
            "Type=oneshot\n",
            "RemainAfterExit=yes\n",
            "ExecStart={helper_prefix} vm-boot --bin-dir {bin_dir}\n",
            "StandardOutput=journal+console\n",
            "StandardError=journal+console\n",
            "UMask=0077\n",
            "NoNewPrivileges=true\n",
            "PrivateTmp=true\n",
            "ProtectHome=true\n",
            "ProtectSystem=strict\n",
            "ReadWritePaths=/opt/outbe-chain /run/systemd/network\n",
            "\n",
            "[Install]\n",
            "WantedBy=outbe-validator.target\n",
        ),
        helper_prefix = helper_prefix,
        bin_dir = bin_dir,
    );
    UnitFile {
        name: "outbe-vm-boot.service".to_owned(),
        content,
    }
}

fn render_managed_target(profile: &ManagedProfile<'_>) -> UnitFile {
    if !profile.is_vm {
        return render_target("Outbe validator stack");
    }
    // `outbe-validator.target` triggers `outbe-vm-boot.service`, which reads `OUTBE_NODE_ROLE`
    // from `/opt/outbe-chain/validator.env` and starts only the units belonging to that role.
    // Every role service sets `PartOf=outbe-validator.target` so stopping the target stops them.
    render_unit(
        "outbe-validator.target",
        TARGET_TEMPLATE,
        &[
            ("DESCRIPTION", "Outbe validator stack".to_owned()),
            (
                "UNIT_DIRECTIVES",
                join(&lines(&[
                    "Requires=outbe-vm-boot.service",
                    "After=outbe-vm-boot.service",
                ])),
            ),
        ],
    )
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
                    "  --listen 0.0.0.0:8776 \\\n",
                    "  --status-listen 127.0.0.1:8876 \\\n",
                    "  --max-validators 128 \\\n",
                    "  --external-inbound-reserve 16 \\\n",
                    "  --advertise ${OUTBE_EXTERNAL_IP}:8776\n",
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
                    "  --http.port 8545 \\\n",
                    "  --http.api eth,net,web3,outbe \\\n",
                    "  --authrpc.port 8551 \\\n",
                    "  --port 30303 \\\n",
                    "  --discovery.port 30303 \\\n",
                    "  --nat extip:${OUTBE_EXTERNAL_IP} \\\n",
                    "  --discovery.v5.addr 0.0.0.0 \\\n",
                    "  --discovery.v5.port 31303 \\\n",
                    "  --p2p-secret-key /opt/outbe-chain/keys/reth-p2p-secret.hex \\\n",
                    "  --bootnodes ${OUTBE_BOOTNODES} \\\n",
                    "  --metrics 127.0.0.1:9101 \\\n",
                    "  --ipcpath /opt/outbe-chain/validator-0/data/reth.ipc \\\n",
                    "  --log.file.directory /opt/outbe-chain/validator-0/logs \\\n",
                    "  --validator \\\n",
                    "  --consensus.signing-key /opt/outbe-chain/keys/signing-key.hex \\\n",
                    "  --validator.evm-key /opt/outbe-chain/keys/evm-key.hex \\\n",
                    "  --consensus.keys-dir /opt/outbe-chain/keys \\\n",
                    "  --consensus.listen-addr 0.0.0.0:30400 \\\n",
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
        assert!(boot
            .contains("After=local-fs.target systemd-networkd.service outbe-data-mount.service"));
        assert!(boot.contains("ReadWritePaths=/opt/outbe-chain /run/systemd/network\n"));
        assert!(boot
            .contains("ExecStart=/usr/local/bin/outbe-vm-agent vm-boot --bin-dir /usr/local/bin"));
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
        assert!(unit(&vm, "outbe-hyperlane-validator.service").contains(
            "ExecStart=/usr/local/bin/outbe-vm-agent hyperlane --bin-dir /usr/local/bin"
        ));
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

        // Verify joining adds only its three join-specific flags relative to VM.
        let joining = joining_units("/usr/local/bin/outbe-up");
        let joining_exec = exec_start_tokens(unit(&joining, "outbe-validator.service"));
        let joining_flags = node_flag_set(&joining_exec[2..]);

        let joining_only: BTreeSet<&str> = joining_flags.difference(&vm_flags).copied().collect();
        let vm_only: BTreeSet<&str> = vm_flags.difference(&joining_flags).copied().collect();
        assert_eq!(
            joining_only,
            BTreeSet::from(["--consensus.keys-dir", "--consensus.peers", "--upstream"])
        );
        assert!(vm_only.is_empty(), "unexpected VM-only flags: {vm_only:?}");
    }

    #[test]
    fn vm_units_use_explicit_env_ports_for_listen_and_advertise_and_keep_fixed_ports() {
        let vm = vm_units();
        let expand = |raw: &str| {
            raw.replace("${OUTBE_EXTERNAL_IP}", "162.19.107.121")
                .replace("${OUTBE_RPC_BIND_IP}", "10.77.0.81")
                .replace("${OUTBE_CONSENSUS_PORT}", "30401")
                .replace("${OUTBE_RETH_PORT}", "30304")
                .replace("${OUTBE_DISCV5_PORT}", "31304")
                .replace("${OUTBE_RADICLE_PORT}", "8777")
        };

        let validator = expand(unit(&vm, "outbe-validator.service"));
        assert!(validator.contains("--consensus.listen-addr 0.0.0.0:30401"));
        assert!(validator.contains("--port 30304"));
        assert!(validator.contains("--discovery.port 30304"));
        assert!(validator.contains("--discovery.v5.port 31304"));
        assert!(validator.contains("--nat extip:162.19.107.121"));
        // Fixed guest ports stay unchanged.
        assert!(validator.contains("--http.addr 10.77.0.81"));
        assert!(validator.contains("--http.port 8545"));
        assert!(validator.contains("--authrpc.port 8551"));
        assert!(validator.contains("--metrics 127.0.0.1:9101"));
        assert!(validator.contains("--radicle.status-address 127.0.0.1:8876"));

        let radicle = expand(unit(&vm, "outbe-radicle.service"));
        assert!(radicle.contains("--listen 0.0.0.0:8777"));
        assert!(radicle.contains("--advertise 162.19.107.121:8777"));
        assert!(radicle.contains("--status-listen 127.0.0.1:8876"));

        let joining = expand(unit(&vm, "outbe-joining-validator.service"));
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
            .find(|(key, _)| key == "ExecStart")
            .expect("unit must define ExecStart");
        value.split_whitespace().map(str::to_owned).collect()
    }

    fn node_flag_set(tokens: &[String]) -> std::collections::BTreeSet<&str> {
        tokens
            .iter()
            .filter(|token| {
                token.starts_with("--") || token.as_str() == "$OUTBE_CONSENSUS_EXTRA_ARGS"
            })
            .map(String::as_str)
            .collect()
    }
}
