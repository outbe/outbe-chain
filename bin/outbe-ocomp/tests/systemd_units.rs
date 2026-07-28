use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn unit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/systemd")
}

fn ocomp_units() -> Vec<PathBuf> {
    [
        "outbe-ocomp.slice",
        "outbe-ocomp.target",
        "outbe-validator-stack.target",
        "outbe-ocomp-snapshot-exporter.service",
        "outbe-ocomp-supervisor.service",
        "outbe-ocomp-worker.socket",
        "outbe-ocomp-worker@.service",
    ]
    .into_iter()
    .map(|name| unit_dir().join(name))
    .collect()
}

fn parse_unit(path: &Path) -> BTreeMap<String, Vec<(String, String)>> {
    let mut sections: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut section = String::new();
    for raw in fs::read_to_string(path).expect("unit bytes").lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name.to_owned();
            continue;
        }
        let (key, value) = line.split_once('=').expect("systemd assignment");
        sections
            .entry(section.clone())
            .or_default()
            .push((key.to_owned(), value.to_owned()));
    }
    sections
}

fn values<'a>(
    unit: &'a BTreeMap<String, Vec<(String, String)>>,
    section: &str,
    key: &str,
) -> Vec<&'a str> {
    unit.get(section)
        .into_iter()
        .flatten()
        .filter_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
        .collect()
}

fn parse_records(path: &Path) -> Vec<Vec<String>> {
    fs::read_to_string(path)
        .expect("configuration bytes")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .collect()
}

#[test]
fn systemd_analyze_accepts_the_fixed_ocomp_topology() {
    let root = tempfile::tempdir().expect("systemd verify root");
    let unit_root = root.path().join("etc/systemd/system");
    fs::create_dir_all(&unit_root).expect("unit root");
    for path in ocomp_units() {
        fs::copy(
            &path,
            unit_root.join(path.file_name().expect("unit file name")),
        )
        .expect("copy exact unit into verify root");
    }
    fs::copy(
        unit_dir().join("outbe-validator.service"),
        unit_root.join("outbe-validator.service"),
    )
    .expect("copy referenced validator unit");
    for target in [
        "sysinit.target",
        "basic.target",
        "sockets.target",
        "shutdown.target",
        "network.target",
        "network-online.target",
        "multi-user.target",
    ] {
        fs::write(
            unit_root.join(target),
            format!("[Unit]\nDescription=offline verifier fixture for {target}\n"),
        )
        .expect("offline target fixture");
    }
    let binary = root.path().join("usr/local/bin/outbe-ocomp");
    fs::create_dir_all(binary.parent().expect("binary parent")).expect("binary directory");
    fs::write(&binary, b"OCOMP systemd verification executable fixture\n").expect("binary fixture");
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("binary fixture mode");

    let output = Command::new("systemd-analyze")
        .arg(format!("--root={}", root.path().display()))
        .arg("verify")
        .args(
            ocomp_units()
                .iter()
                .map(|path| path.file_name().expect("unit file name")),
        )
        .output()
        .expect("systemd-analyze must exist on the OCM Linux lane");
    assert!(
        output.status.success(),
        "systemd-analyze verify failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parsed_unit_graph_keeps_node_lifecycle_outside_ocomp() {
    let target = parse_unit(&unit_dir().join("outbe-ocomp.target"));
    assert_eq!(
        values(&target, "Unit", "Wants"),
        vec![
            "outbe-ocomp-snapshot-exporter.service",
            "outbe-ocomp-supervisor.service",
            "outbe-ocomp-worker.socket"
        ]
    );
    for name in [
        "outbe-ocomp-snapshot-exporter.service",
        "outbe-ocomp-supervisor.service",
        "outbe-ocomp-worker@.service",
    ] {
        let unit = parse_unit(&unit_dir().join(name));
        for relation in ["Requires", "BindsTo", "PartOf"] {
            assert!(
                values(&unit, "Unit", relation)
                    .iter()
                    .all(|value| *value != "outbe-validator.service"),
                "{name} must not propagate lifecycle to the node through {relation}"
            );
        }
    }

    let socket = parse_unit(&unit_dir().join("outbe-ocomp-worker.socket"));
    assert_eq!(values(&socket, "Socket", "Accept"), vec!["yes"]);
    assert_eq!(values(&socket, "Socket", "MaxConnections"), vec!["4"]);
    assert_eq!(
        values(&socket, "Socket", "MaxConnectionsPerSource"),
        vec!["4"]
    );

    let worker = parse_unit(&unit_dir().join("outbe-ocomp-worker@.service"));
    assert_eq!(values(&worker, "Service", "Restart"), vec!["no"]);
    assert_eq!(values(&worker, "Service", "StandardInput"), vec!["socket"]);
    assert_eq!(values(&worker, "Service", "PrivateNetwork"), vec!["yes"]);
    assert_eq!(
        values(&worker, "Service", "RestrictAddressFamilies"),
        vec!["AF_UNIX"]
    );
    assert_eq!(
        values(&worker, "Service", "BindReadOnlyPaths"),
        vec!["/var/lib/outbe-ocomp/cas-v1/objects"]
    );
    assert_eq!(
        values(&worker, "Service", "ReadWritePaths"),
        vec!["/var/lib/outbe-ocomp/worker-inbox-v1"]
    );

    let exporter = parse_unit(&unit_dir().join("outbe-ocomp-snapshot-exporter.service"));
    assert!(values(&exporter, "Service", "ReadWritePaths")
        .contains(&"/var/lib/outbe-ocomp/cas-v1/objects"));
    let supervisor = parse_unit(&unit_dir().join("outbe-ocomp-supervisor.service"));
    assert!(values(&supervisor, "Service", "ReadWritePaths")
        .contains(&"/var/lib/outbe-ocomp/cas-v1/objects"));
    assert!(values(&supervisor, "Service", "ReadWritePaths")
        .contains(&"/var/lib/outbe-ocomp/worker-inbox-v1"));
}

#[test]
fn role_configuration_provisions_narrow_cross_process_artifact_access() {
    let expected_memberships = [
        ("outbe-ocomp-supervisor", "outbe-ocomp-artifacts"),
        ("outbe-ocomp-export", "outbe-ocomp-artifacts"),
        ("outbe-ocomp-worker", "outbe-ocomp-artifacts"),
    ];
    let sysusers = parse_records(&unit_dir().join("outbe-ocomp.sysusers"));
    for (user, group) in expected_memberships {
        assert!(
            sysusers.iter().any(|record| {
                record.first().map(String::as_str) == Some("m")
                    && record.get(1).map(String::as_str) == Some(user)
                    && record.get(2).map(String::as_str) == Some(group)
            }),
            "{user} must receive read access through {group}"
        );
    }

    let expected_directories = [
        ("/var/lib/outbe-ocomp/cas-v1/objects", "0770"),
        ("/var/lib/outbe-ocomp/cas-v1/refs", "0770"),
        ("/var/lib/outbe-ocomp/exporter-v1", "0750"),
        ("/var/lib/outbe-ocomp/supervisor-v1", "0700"),
        ("/var/lib/outbe-ocomp/worker-inbox-v1", "0770"),
    ];
    let tmpfiles = parse_records(&unit_dir().join("outbe-ocomp.tmpfiles"));
    for (path, mode) in expected_directories {
        assert!(
            tmpfiles.iter().any(|record| {
                record.first().map(String::as_str) == Some("d")
                    && record.get(1).map(String::as_str) == Some(path)
                    && record.get(2).map(String::as_str) == Some(mode)
                    && record.get(4).map(String::as_str) == Some("outbe-ocomp-artifacts")
            }),
            "{path} must be provisioned with mode {mode} and the artifact group"
        );
    }

    for name in [
        "outbe-ocomp-snapshot-exporter.service",
        "outbe-ocomp-supervisor.service",
        "outbe-ocomp-worker@.service",
    ] {
        let unit = parse_unit(&unit_dir().join(name));
        assert_eq!(
            values(&unit, "Service", "Group"),
            vec!["outbe-ocomp-artifacts"],
            "{name} must use the shared artifact group as its primary group"
        );
        assert_eq!(values(&unit, "Service", "UMask"), vec!["0027"]);
    }

    let supervisor = parse_unit(&unit_dir().join("outbe-ocomp-supervisor.service"));
    assert_eq!(
        values(&supervisor, "Service", "ReadOnlyPaths"),
        vec!["/var/lib/outbe-ocomp/exporter-v1"]
    );
    assert_eq!(
        values(&supervisor, "Service", "EnvironmentFile"),
        vec!["/etc/outbe/outbe-ocomp.env"]
    );
    let worker = parse_unit(&unit_dir().join("outbe-ocomp-worker@.service"));
    assert_eq!(
        values(&worker, "Service", "EnvironmentFile"),
        vec!["/etc/outbe/outbe-ocomp.env"]
    );
    let exporter = parse_unit(&unit_dir().join("outbe-ocomp-snapshot-exporter.service"));
    assert_eq!(
        values(&exporter, "Service", "EnvironmentFile"),
        vec![
            "/etc/outbe/outbe-ocomp.env",
            "/etc/outbe/outbe-ocomp-export.env"
        ],
        "only snapshot-exporter receives Mongo projection credentials"
    );
}
