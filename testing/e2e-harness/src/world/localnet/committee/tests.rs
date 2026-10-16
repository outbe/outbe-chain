use super::{
    cargo_package_version, committee_signal_args, reap_stopped_committee,
    replacement_chain_build_args, rewrite_workspace_version, unix_time_offset_arg,
};
use clap::Parser;
use std::{collections::HashMap, process::Command};

use crate::internal::proc::ChildGuard;

#[test]
fn committee_reap_reports_abnormal_exit_without_losing_other_statuses() {
    let mut validators = HashMap::new();
    for (index, exit_code) in [(0, 0), (1, 134)] {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("exit {exit_code}")]);
        validators.insert(
            index,
            ChildGuard::spawn(format!("validator-{index}"), command).unwrap(),
        );
    }

    let error = reap_stopped_committee(&mut validators).unwrap_err();
    assert!(format!("{error:#}").contains("validator-1 PID"));
    assert!(format!("{error:#}").contains("134"));
    assert!(validators
        .values_mut()
        .all(|child| child.exit_status().unwrap().is_some()));
}

#[derive(Debug, Parser)]
struct ClockOffsetArgs {
    #[arg(long = "testnet.unix-time-offset-secs")]
    unix_time_offset_secs: Option<i64>,
}

#[test]
fn negative_clock_offset_is_one_clap_parseable_argument() {
    let offset_arg = unix_time_offset_arg(-3);
    let parsed = ClockOffsetArgs::try_parse_from(["outbe-chain", offset_arg.as_str()])
        .expect("negative clock offset must remain the option value");

    assert_eq!(parsed.unix_time_offset_secs, Some(-3));
}

#[test]
fn committee_freeze_is_one_noninteractive_signal_for_the_complete_cohort() {
    assert_eq!(
        committee_signal_args(&[101, 202, 303, 404], "STOP"),
        ["-STOP", "--", "101", "202", "303", "404"]
    );
}

#[test]
fn validator_restart_restores_exact_pre_recovery_argv() {
    let rebuilt = vec!["node".to_owned(), "--validator".to_owned()];
    let original = vec![
        "node".to_owned(),
        "--chain=/pinned/genesis.json".to_owned(),
        "--validator".to_owned(),
        "--testnet.unix-time-offset-secs=-37".to_owned(),
    ];

    assert_eq!(
        super::restore_validator_argv(rebuilt, Some(original.clone())),
        original
    );
}

#[test]
fn startup_checks_genesis_window_without_rewriting_it() {
    let genesis = serde_json::json!({
        "config": { "outbeProtocol": { "governance": { "votingWindowBlocks": 20 } } }
    });
    let before = genesis.clone();
    super::verify_genesis_governance_window(&genesis, 20).unwrap();
    assert!(super::verify_genesis_governance_window(&genesis, 6).is_err());
    assert_eq!(genesis, before);
    let missing = serde_json::json!({ "config": {} });
    assert!(super::verify_genesis_governance_window(&missing, 6).is_err());
    super::verify_genesis_governance_window(&missing, 86_400).unwrap();
}

#[test]
fn expands_protocol_version_to_cargo_semver() {
    assert_eq!(cargo_package_version("3.0").unwrap(), "3.0.0");
    assert!(cargo_package_version("3").is_err());
    assert!(cargo_package_version("3.0.1").is_err());
}

#[test]
fn replacement_binary_preserves_the_complete_e2e_chain_feature_surface() {
    assert_eq!(
        replacement_chain_build_args(),
        [
            "build",
            "--offline",
            "--release",
            "-p",
            "outbe-chain",
            "--bin",
            "outbe-chain",
            "--features",
            "e2e-test,test-protocol-overrides",
        ]
    );
}

#[test]
fn rewrites_only_the_workspace_package_version() {
    let manifest = r#"[workspace]
members = []

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
example = { version = "9.9.9" }
"#;

    let rewritten = rewrite_workspace_version(manifest, "3.0.0").unwrap();
    assert!(rewritten.contains("[workspace.package]\nversion = \"3.0.0\""));
    assert!(rewritten.contains("example = { version = \"9.9.9\" }"));
}
