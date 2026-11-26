use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use eyre::{bail, Context};

use super::{
    preflight::{parse_bridge_cidr, parse_uplink_section},
    *,
};
use crate::shell_quote;

/// Apply options for a real (not dry-run), non-forced apply with the default uplink.
fn default_options() -> ApplyOptions<'static> {
    ApplyOptions {
        uplink_override: None,
        dry_run: false,
        force: false,
        artifacts: Path::new("/tmp/artifacts"),
    }
}

#[test]
fn validate_iface_name_accepts_valid_and_rejects_injection() {
    for ok in [
        "obr0",
        "eth0",
        "enp1s0f0",
        "ens3.100",
        "br-01_a",
        "a12345678901234",
    ] {
        assert!(
            validate_iface_name(ok, "test").is_ok(),
            "should accept {ok}"
        );
    }
    for bad in [
        "",
        "a123456789012345",
        "-obr0",
        ".obr0",
        "obr0; rm -rf /",
        "obr0\" drop",
        "obr0\neth0",
        "obr0*",
        "obr0/1",
        "$(id)",
    ] {
        assert!(
            validate_iface_name(bad, "test").is_err(),
            "should reject {bad:?}"
        );
    }
}

#[test]
fn parse_bridge_cidr_and_subnet_membership() {
    let sample = "4: obr0    inet 10.77.0.1/24 scope global obr0\\       valid_lft forever preferred_lft forever\n";
    let (addr, subnet) = parse_bridge_cidr(sample, "obr0").unwrap();
    assert_eq!(addr, Ipv4Addr::new(10, 77, 0, 1));
    assert_eq!(subnet.cidr_string(), "10.77.0.0/24");
    assert!(subnet.contains(Ipv4Addr::new(10, 77, 0, 68)));
    assert!(!subnet.contains(Ipv4Addr::new(10, 77, 1, 68)));
}

#[test]
fn parse_uplink_section_detects_default_route_and_override() {
    let route = "default via 162.19.107.254 dev eno1 proto static\n";
    assert_eq!(parse_uplink_section(route).unwrap(), "eno1");
    assert_eq!(parse_uplink_section("override enp2s0\n").unwrap(), "enp2s0");
    assert!(parse_uplink_section("").is_err());
}

#[test]
fn render_nat_ruleset_is_atomic_has_marker_and_rejects_invalid_inputs() {
    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let rules = render_nat_ruleset("obr0", subnet, "eno1").unwrap();
    assert!(rules.starts_with(&format!(
        "{OWNERSHIP_MARKER}\ntable ip outbe_up_nat\ndelete table ip outbe_up_nat\n"
    )));
    assert!(rules.contains("iifname \"obr0\" oifname \"eno1\" ip saddr 10.77.0.0/24 masquerade"));
    assert!(rules.contains("iifname \"obr0\" oifname \"eno1\" ip saddr 10.77.0.0/24 accept"));
    assert!(rules.contains(
        "iifname \"eno1\" oifname \"obr0\" ip daddr 10.77.0.0/24 ct state established,related accept"
    ));
    assert!(!rules.contains("flush ruleset"));
    assert!(!rules.contains("policy drop"));
    assert!(!rules.contains("hook input"));

    assert!(render_nat_ruleset("obr0", subnet, "obr0").is_err());
    assert!(render_nat_ruleset("obr0;drop", subnet, "eno1").is_err());
    assert!(render_nat_ruleset("obr0", subnet, "eno1\" bad").is_err());
}

#[test]
fn json_inspect_classifies_policy_accept_with_explicit_drop_as_blocks_or_unknown() {
    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let json = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "inet", "name": "firewall"}},
        {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 10, "policy": "accept"}},
        {"rule": {"family": "inet", "table": "firewall", "chain": "forward", "expr": [
          {"match": {"op": "==", "left": {"meta": {"key": "iifname"}}, "right": "obr0"}},
          {"drop": null}
        ]}}
      ]
    }"#;
    let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
    assert_eq!(res.foreign_forward_chains.len(), 1);
    assert_eq!(res.blocks_or_unknown_chains.len(), 1);
    assert_eq!(
        res.blocks_or_unknown_chains[0].class,
        ForwardChainClass::BlocksOrUnknown
    );
    assert!(res.blocks_or_unknown_chains[0]
        .reasons
        .iter()
        .any(|r| r.contains("drop")));
}

#[test]
fn json_inspect_finds_policy_drop_forward_chain_after_brace_in_comment() {
    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let json = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "inet", "name": "firewall"}},
        {"chain": {"family": "inet", "table": "firewall", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "accept"}},
        {"rule": {"family": "inet", "table": "firewall", "chain": "input", "comment": "}", "expr": [{"accept": null}]}},
        {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 0, "policy": "drop"}}
      ]
    }"#;
    let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
    assert_eq!(res.foreign_forward_chains.len(), 1);
    assert_eq!(res.blocks_or_unknown_chains.len(), 1);
    assert_eq!(res.blocks_or_unknown_chains[0].table, "firewall");
    assert_eq!(res.blocks_or_unknown_chains[0].chain, "forward");
    assert_eq!(res.blocks_or_unknown_chains[0].policy, "drop");
}

#[test]
fn json_inspect_detects_existing_foreign_masquerade_and_ignores_hairpin() {
    let subnet = Ipv4Subnet::from_addr_prefix(Ipv4Addr::new(10, 77, 0, 1), 24).unwrap();
    let json = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "ip", "name": "outbe_mvp"}},
        {"chain": {"family": "ip", "table": "outbe_mvp", "name": "postrouting", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
        {"rule": {"family": "ip", "table": "outbe_mvp", "chain": "postrouting", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"snat": {"addr": "198.51.100.10"}}
        ]}},
        {"rule": {"family": "ip", "table": "outbe_mvp", "chain": "postrouting", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
          {"snat": {"addr": "198.51.100.10"}}
        ]}}
      ]
    }"#;
    let res = inspect_nft_json(json, "obr0", subnet, "eno1").unwrap();
    assert_eq!(res.existing_egress.len(), 1);
    assert_eq!(res.existing_egress[0].family, "ip");
    assert_eq!(res.existing_egress[0].table, "outbe_mvp");
    assert_eq!(res.existing_egress[0].chain, "postrouting");
}

#[test]
fn parse_persistent_sysctl_respects_directory_precedence_masking_and_sysctl_conf() {
    // 1. /etc/sysctl.d overrides /usr/lib/sysctl.d for the same basename.
    let lines = "\
FILE 0 /etc/sysctl.d/50-default.conf
SET /etc/sysctl.d/50-default.conf:net.ipv4.ip_forward = 1
FILE 3 /usr/lib/sysctl.d/50-default.conf
SET /usr/lib/sysctl.d/50-default.conf:net.ipv4.ip_forward = 0
";
    let state = parse_persistent_sysctl(lines, "90-outbe-up-nat.conf");
    assert_eq!(state.effective_value, Some(1));
    assert_eq!(
        state.effective_source.as_deref(),
        Some("/etc/sysctl.d/50-default.conf")
    );
    assert!(!state.overridden_by_later_zero);

    // 2. Whole-file mask without ip_forward key in /etc hides /usr/lib setting of 1.
    let masked = "\
FILE 0 /etc/sysctl.d/50-vendor.conf
FILE 3 /usr/lib/sysctl.d/50-vendor.conf
SET /usr/lib/sysctl.d/50-vendor.conf:net.ipv4.ip_forward = 1
";
    let state_masked = parse_persistent_sysctl(masked, "90-outbe-up-nat.conf");
    assert_eq!(state_masked.effective_value, None);
    assert!(!state_masked.overridden_by_later_zero);

    // 3. Later 99-admin.conf = 0 overrides 90-outbe-up-nat.conf.
    let later_zero = "\
FILE 0 /etc/sysctl.d/99-admin.conf
SET /etc/sysctl.d/99-admin.conf:net.ipv4.ip_forward = 0
";
    let state_later = parse_persistent_sysctl(later_zero, "90-outbe-up-nat.conf");
    assert_eq!(state_later.effective_value, Some(0));
    assert_eq!(
        state_later.effective_source.as_deref(),
        Some("/etc/sysctl.d/99-admin.conf")
    );
    assert!(state_later.overridden_by_later_zero);

    // 4. /etc/sysctl.conf = 0 is applied last by procps `sysctl --system` and overrides 90-.
    let conf_zero = "\
FILE 0 /etc/sysctl.d/90-vm-host.conf
SET /etc/sysctl.d/90-vm-host.conf:net.ipv4.ip_forward = 1
CONF /etc/sysctl.conf
SET /etc/sysctl.conf:net.ipv4.ip_forward = 0
";
    let state_conf = parse_persistent_sysctl(conf_zero, "90-outbe-up-nat.conf");
    assert_eq!(state_conf.effective_value, Some(0));
    assert_eq!(
        state_conf.effective_source.as_deref(),
        Some("/etc/sysctl.conf")
    );
    assert!(state_conf.overridden_by_later_zero);

    let empty = parse_persistent_sysctl("", "90-outbe-up-nat.conf");
    assert_eq!(empty.effective_value, None);
    assert!(!empty.overridden_by_later_zero);
}

struct Progress;

impl Progress {
    fn new() -> Self {
        Self
    }
}

impl StageReporter for Progress {
    fn stage(&mut self, _message: &str) {}
}

struct StubHostRunner {
    bin_dir: PathBuf,
}

impl HostRunner for StubHostRunner {
    fn run_raw(&mut self, _host: &str, command: &str) -> Result<String> {
        let base_path = std::env::var("PATH").unwrap_or_default();
        let path_env = format!("{}:{base_path}", self.bin_dir.display());
        let out = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("PATH", path_env)
            .output()
            .wrap_err("run stubbed command")?;
        if !out.status.success() {
            bail!(
                "stubbed command failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn write_shim(bin_dir: &Path, name: &str, body: &str) {
    let path = bin_dir.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct StubEnv {
    _temp: tempfile::TempDir,
    bin_dir: PathBuf,
    etc_dir: PathBuf,
    log_file: PathBuf,
    ip_fwd_file: PathBuf,
    nat_table_file: PathBuf,
    paths: HostNatPaths,
    target: HostTarget,
}

impl StubEnv {
    fn new(initial_ip_forward: &str, initial_nft_json: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        let state_dir = temp.path().join("state");
        let etc_dir = temp.path().join("etc");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&etc_dir).unwrap();

        let log_file = state_dir.join("calls.log");
        let ip_fwd_file = state_dir.join("ip_forward");
        let nft_json_file = state_dir.join("nft.json");
        let nat_table_file = state_dir.join("nat_table");

        fs::write(&log_file, "").unwrap();
        fs::write(&ip_fwd_file, format!("{initial_ip_forward}\n")).unwrap();
        fs::write(&nft_json_file, initial_nft_json).unwrap();

        let log_q = shell_quote(&log_file.to_string_lossy());
        let ip_fwd_q = shell_quote(&ip_fwd_file.to_string_lossy());
        let nft_json_q = shell_quote(&nft_json_file.to_string_lossy());
        let nat_table_q = shell_quote(&nat_table_file.to_string_lossy());

        write_shim(
            &bin_dir,
            "sudo",
            "if [ \"${1:-}\" = \"-n\" ]; then shift; fi\nexec \"$@\"",
        );
        write_shim(
            &bin_dir,
            "ip",
            &format!(
                "echo \"ip $*\" >> {log_q}\n\
                 case \"$*\" in\n\
                   *\"addr show dev obr0\"*) echo '4: obr0 inet 10.77.0.1/24 scope global obr0' ;;\n\
                   *\"route show default\"*) echo 'default via 162.19.107.254 dev eno1 proto static' ;;\n\
                   *\"link show dev\"*) exit 0 ;;\n\
                   *) exit 1 ;;\n\
                 esac"
            ),
        );
        write_shim(
            &bin_dir,
            "sysctl",
            &format!(
                "echo \"sysctl $*\" >> {log_q}\n\
                 case \"$1\" in\n\
                   -n) cat {ip_fwd_q} ;;\n\
                   -w) val=${{2#*=}}; printf '%s\\n' \"$val\" > {ip_fwd_q} ;;\n\
                   *) exit 1 ;;\n\
                 esac"
            ),
        );
        write_shim(
            &bin_dir,
            "systemctl",
            &format!("echo \"systemctl $*\" >> {log_q}"),
        );
        write_shim(
            &bin_dir,
            "nft",
            &format!(
                "echo \"nft $*\" >> {log_q}\n\
                 case \"$1\" in\n\
                   -j) cat {nft_json_q} ;;\n\
                   list) [ -f {nat_table_q} ] && cat {nat_table_q} || echo 'table ip outbe_up_nat {{}}' ;;\n\
                   -f) if [ \"$2\" = \"-\" ]; then cat > {nat_table_q}; else cp \"$2\" {nat_table_q}; fi ;;\n\
                   delete) rm -f {nat_table_q} ;;\n\
                   *) exit 1 ;;\n\
                 esac"
            ),
        );

        let paths = HostNatPaths {
            rules_path: etc_dir.join("outbe-up/nat.nft"),
            service_path: etc_dir.join("systemd/system/outbe-up-nat.service"),
            sysctl_path: etc_dir.join("sysctl.d/90-outbe-up-nat.conf"),
            sysctl_scan_dirs: vec![
                etc_dir.join("sysctl.d"),
                temp.path().join("usr/lib/sysctl.d"),
            ],
            sysctl_conf_path: etc_dir.join("sysctl.conf"),
        };
        let target = HostTarget {
            host: "vmm-test".to_owned(),
            bridge: "obr0".to_owned(),
            founder_ips: vec![Ipv4Addr::new(10, 77, 0, 68), Ipv4Addr::new(10, 77, 0, 69)],
        };

        Self {
            _temp: temp,
            bin_dir,
            etc_dir,
            log_file,
            ip_fwd_file,
            nat_table_file,
            paths,
            target,
        }
    }

    fn runner(&self) -> StubHostRunner {
        StubHostRunner {
            bin_dir: self.bin_dir.clone(),
        }
    }

    fn calls(&self) -> String {
        fs::read_to_string(&self.log_file).unwrap()
    }
}

#[test]
fn orchestration_existing_foreign_masquerade_is_noop_unless_forced() {
    let foreign_masq = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "ip", "name": "nat"}},
        {"chain": {"family": "ip", "table": "nat", "name": "POSTROUTING", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
        {"rule": {"family": "ip", "table": "nat", "chain": "POSTROUTING", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
          {"masquerade": null}
        ]}}
      ]
    }"#;
    let env = StubEnv::new("1", foreign_masq);
    let mut runner = env.runner();
    let mut progress = Progress::new();

    let outcome = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::NoOpExistingEgress);
    assert!(!env.paths.rules_path.exists());
    assert!(!env.paths.service_path.exists());
    assert!(!env.paths.sysctl_path.exists());
    let log = env.calls();
    assert!(!log.contains("nft -f"));
    assert!(!log.contains("sysctl -w"));
    assert!(!log.contains("systemctl"));
}

#[test]
fn orchestration_refuses_foreign_blocks_or_unknown_forward_chain_without_mutation() {
    let foreign_drop = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "inet", "name": "firewall"}},
        {"chain": {"family": "inet", "table": "firewall", "name": "forward", "type": "filter", "hook": "forward", "prio": 10, "policy": "accept"}},
        {"rule": {"family": "inet", "table": "firewall", "chain": "forward", "expr": [
          {"match": {"op": "==", "left": {"meta": {"key": "iifname"}}, "right": "obr0"}},
          {"drop": null}
        ]}}
      ]
    }"#;
    let env = StubEnv::new("0", foreign_drop);
    let mut runner = env.runner();
    let mut progress = Progress::new();

    let err = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("firewall::forward"), "{msg}");
    assert!(msg.contains("cannot be verified statically"), "{msg}");
    assert!(!env.paths.rules_path.exists());
    assert!(!env.paths.service_path.exists());
    assert!(!env.paths.sysctl_path.exists());
    let log = env.calls();
    assert!(!log.contains("nft -f"));
    assert!(!log.contains("sysctl -w"));
    assert!(!log.contains("systemctl"));
}

#[test]
fn orchestration_apply_persists_ip_forward_and_remove_preserves_runtime_and_foreign_files() {
    let clean_host_fw = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "inet", "name": "outbe_host_fw"}},
        {"chain": {"family": "inet", "table": "outbe_host_fw", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "drop"}}
      ]
    }"#;
    // Runtime ip_forward is already 1, but no persistent sysctl file sets it.
    let env = StubEnv::new("1", clean_host_fw);
    let mut runner = env.runner();
    let mut progress = Progress::new();

    // 1. Apply writes marked rules, unit, AND persistent sysctl drop-in even though runtime is 1.
    let outcome1 = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(outcome1, ApplyOutcome::Applied);
    assert!(env.paths.rules_path.is_file());
    assert!(env.paths.service_path.is_file());
    assert!(env.paths.sysctl_path.is_file());
    assert!(fs::read_to_string(&env.paths.sysctl_path)
        .unwrap()
        .contains(OWNERSHIP_MARKER));
    // Runtime ip_forward was already 1, so `sysctl -w` was not called.
    assert!(!env.calls().contains("sysctl -w"));

    // 2. Second apply is idempotent.
    let outcome2 = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(outcome2, ApplyOutcome::Applied);

    // 3. Remove deletes our marked files & table, and NEVER writes ip_forward=0.
    run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
    assert!(!env.paths.rules_path.exists());
    assert!(!env.paths.service_path.exists());
    assert!(!env.paths.sysctl_path.exists());
    assert!(!env.nat_table_file.exists());
    assert_eq!(fs::read_to_string(&env.ip_fwd_file).unwrap().trim(), "1");
    assert!(!env.calls().contains("ip_forward=0"));

    // 4. Foreign sysctl file at our path is untouched by remove, and ip_forward is not written.
    fs::create_dir_all(env.etc_dir.join("sysctl.d")).unwrap();
    let foreign_content = "# admin custom sysctl\nnet.ipv4.ip_forward = 1\n";
    fs::write(&env.paths.sysctl_path, foreign_content).unwrap();
    fs::write(&env.log_file, "").unwrap();
    run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
    assert_eq!(
        fs::read_to_string(&env.paths.sysctl_path).unwrap(),
        foreign_content
    );
    assert!(!env.calls().contains("sysctl -w"));
}

#[test]
fn orchestration_partial_tcp443_masquerade_is_not_noop_and_requires_force() {
    let tcp443_only = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "ip", "name": "partial_nat"}},
        {"chain": {"family": "ip", "table": "partial_nat", "name": "postrouting", "type": "nat", "hook": "postrouting", "prio": 100, "policy": "accept"}},
        {"rule": {"family": "ip", "table": "partial_nat", "chain": "postrouting", "expr": [
          {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": {"prefix": {"addr": "10.77.0.0", "len": 24}}}},
          {"match": {"op": "==", "left": {"meta": {"key": "oifname"}}, "right": "eno1"}},
          {"match": {"op": "==", "left": {"payload": {"protocol": "tcp", "field": "dport"}}, "right": 443}},
          {"masquerade": null}
        ]}}
      ]
    }"#;
    let env = StubEnv::new("1", tcp443_only);
    let mut runner = env.runner();
    let mut progress = Progress::new();

    let err = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("partial_nat::postrouting"), "{msg}");
    assert!(msg.contains("tcp.dport"), "{msg}");
    assert!(msg.contains("--force"), "{msg}");
    assert!(!env.paths.rules_path.exists());
    assert!(!env.calls().contains("nft -f"));
}

#[test]
fn orchestration_mask_without_key_writes_dropin_and_later_overrides_refuse() {
    let clean_host_fw = r#"{
      "nftables": [
        {"metainfo": {"json_schema_version": 1}},
        {"table": {"family": "inet", "name": "outbe_host_fw"}},
        {"chain": {"family": "inet", "table": "outbe_host_fw", "name": "input", "type": "filter", "hook": "input", "prio": 0, "policy": "drop"}}
      ]
    }"#;
    let env = StubEnv::new("1", clean_host_fw);
    let mut runner = env.runner();
    let mut progress = Progress::new();

    let usr_lib_sysctl = &env.paths.sysctl_scan_dirs[1];
    let etc_sysctl = &env.paths.sysctl_scan_dirs[0];
    fs::create_dir_all(usr_lib_sysctl).unwrap();
    fs::create_dir_all(etc_sysctl).unwrap();

    // 1. Mask without key: /etc/sysctl.d/50-vendor.conf hides /usr/lib/sysctl.d/50-vendor.conf=1.
    fs::write(
        usr_lib_sysctl.join("50-vendor.conf"),
        "net.ipv4.ip_forward = 1\n",
    )
    .unwrap();
    fs::write(
        etc_sysctl.join("50-vendor.conf"),
        "# masks vendor file without setting ip_forward\nkernel.printk = 3 4 1 3\n",
    )
    .unwrap();

    let outcome = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap();
    assert_eq!(outcome, ApplyOutcome::Applied);
    assert!(
        env.paths.sysctl_path.is_file(),
        "masked vendor file must not suppress writing 90-outbe-up-nat.conf"
    );

    // Clean up our drop-in before testing refusal cases.
    run_remove_on_host(&env.target, &mut runner, &env.paths, false, &mut progress).unwrap();
    fs::write(&env.log_file, "").unwrap();

    // 2. Later 99-admin.conf = 0 refuses without writing 90-outbe-up-nat.conf.
    fs::write(
        etc_sysctl.join("99-admin.conf"),
        "net.ipv4.ip_forward = 0\n",
    )
    .unwrap();
    let err_99 = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap_err();
    assert!(err_99.to_string().contains("99-admin.conf"), "{err_99}");
    assert!(!env.paths.sysctl_path.exists());
    assert!(!env.calls().contains("nft -f"));

    // 3. /etc/sysctl.conf = 0 overrides 90-vm-host.conf = 1 and refuses without mutation.
    fs::remove_file(etc_sysctl.join("99-admin.conf")).unwrap();
    fs::write(
        etc_sysctl.join("90-vm-host.conf"),
        "net.ipv4.ip_forward = 1\n",
    )
    .unwrap();
    fs::write(&env.paths.sysctl_conf_path, "net.ipv4.ip_forward = 0\n").unwrap();
    fs::write(&env.log_file, "").unwrap();
    let err_conf = run_apply_on_host(
        &env.target,
        &mut runner,
        &env.paths,
        &default_options(),
        &mut progress,
    )
    .unwrap_err();
    assert!(err_conf.to_string().contains("sysctl.conf"), "{err_conf}");
    assert!(!env.paths.sysctl_path.exists());
    assert!(!env.calls().contains("nft -f"));
}
