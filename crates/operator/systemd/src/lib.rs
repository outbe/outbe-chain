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

/// Relative systemd drop-in filename used by dev-flavor-only diagnostic overrides.
pub const VM_DEV_DROPIN_NAME: &str = "20-outbe-dev.conf";

const VM_NODE_SERVICES: &[&str] = &[
    "outbe-validator.service",
    "outbe-joining-validator.service",
    "outbe-full-node.service",
];

const VM_CONSOLE_ONLY_SERVICES: &[&str] = &[
    "dstack-guest-agent.service",
    "outbe-feeder.service",
    "outbe-hyperlane-validator.service",
    "outbe-radicle.service",
];

/// Render dev-flavor-only systemd drop-ins (`<unit>.d/20-outbe-dev.conf`).
///
/// Never installed into the production image:
/// - Forwards stdout/stderr of `outbe-*` services and `dstack-guest-agent.service` to the serial
///   console (`StandardOutput=journal+console`, `StandardError=journal+console`).
/// - Overrides `ExecStart` on `outbe-chain` node units (`outbe-validator.service`,
///   `outbe-joining-validator.service`, `outbe-full-node.service`) so `--metrics` binds on
///   `${OUTBE_VM_IP}:9101` instead of `127.0.0.1:9101` and stdout logging includes
///   `--log.stdout.filter info,commonware_p2p=debug`, derived directly from [`vm_units`] so flags
///   cannot drift.
pub fn vm_dev_dropins() -> Vec<UnitFile> {
    let base_units = vm_units();
    let mut dropins = Vec::new();

    for service_name in VM_NODE_SERVICES {
        let base = base_units
            .iter()
            .find(|u| u.name == *service_name)
            .unwrap_or_else(|| panic!("missing base VM unit {service_name}"));
        let exec_start = extract_exec_start_block(&base.content)
            .unwrap_or_else(|| panic!("missing ExecStart in {service_name}"));
        let from = "  --metrics 127.0.0.1:9101 \\";
        let to = "  --metrics ${OUTBE_VM_IP}:9101 \\\n  --log.stdout.filter info,commonware_p2p=debug \\";
        assert_eq!(
            exec_start.matches(from).count(),
            1,
            "{service_name}: expected exactly one loopback --metrics flag in ExecStart"
        );
        let dev_exec_start = exec_start.replacen(from, to, 1);
        let content = format!(
            "[Service]\n\
             StandardOutput=journal+console\n\
             StandardError=journal+console\n\
             ExecStart=\n\
             {dev_exec_start}\n"
        );
        dropins.push(UnitFile {
            name: format!("{service_name}.d/{VM_DEV_DROPIN_NAME}"),
            content,
        });
    }

    for service_name in VM_CONSOLE_ONLY_SERVICES {
        dropins.push(UnitFile {
            name: format!("{service_name}.d/{VM_DEV_DROPIN_NAME}"),
            content: concat!(
                "[Service]\n",
                "StandardOutput=journal+console\n",
                "StandardError=journal+console\n",
            )
            .to_owned(),
        });
    }

    dropins.sort_by(|left, right| left.name.cmp(&right.name));
    dropins
}

fn extract_exec_start_block(unit_content: &str) -> Option<String> {
    let mut lines_iter = unit_content.lines();
    let first = lines_iter.find(|line| line.starts_with("ExecStart="))?;
    let mut block = vec![first];
    if first.trim_end().ends_with('\\') {
        for line in lines_iter {
            block.push(line);
            if !line.trim_end().ends_with('\\') {
                break;
            }
        }
    }
    Some(block.join("\n"))
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
                "  --listen 0.0.0.0:${OUTBE_RADICLE_PORT} \\",
                "  --status-listen 127.0.0.1:8876 \\",
                "  --max-validators 128 \\",
                "  --external-inbound-reserve 16 \\",
                "  --advertise ${OUTBE_EXTERNAL_IP}:${OUTBE_RADICLE_PORT}",
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
fn common_node_exec_lines(bin_dir: &str, p2p_secret_key: Option<&str>) -> Vec<String> {
    let mut args = lines(&[
        &format!("ExecStart={bin_dir}/outbe-chain node \\"),
        "  --chain /opt/outbe-chain/genesis.json \\",
        "  --datadir /opt/outbe-chain/validator-0/data \\",
        "  --engine.persistence-threshold 0 \\",
        "  --engine.memory-block-buffer-target 0 \\",
        "  --consensus.storage-dir /opt/outbe-chain/validator-0/consensus \\",
        "  --http \\",
        "  --http.addr ${OUTBE_RPC_BIND_IP} \\",
        "  --http.port ${OUTBE_RPC_PORT} \\",
        "  --http.api eth,net,web3,outbe \\",
        "  --authrpc.port 8551 \\",
        "  --port ${OUTBE_RETH_P2P_PORT} \\",
        "  --discovery.port ${OUTBE_RETH_P2P_PORT} \\",
        "  --nat extip:${OUTBE_EXTERNAL_IP} \\",
        "  --discovery.v5.addr 0.0.0.0 \\",
        "  --discovery.v5.port ${OUTBE_RETH_DISCOVERY_PORT} \\",
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
    directives.push("  --consensus.listen-addr 0.0.0.0:${OUTBE_CONSENSUS_PORT} \\".to_owned());
    if joining {
        directives.push("  --tee-bootstrap-timeout-secs 300 \\".to_owned());
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
    directives.extend(common_node_exec_lines(profile.bin_dir, None));
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
mod tests;
