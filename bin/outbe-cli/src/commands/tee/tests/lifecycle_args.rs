use super::RecordingRpc;
use clap::{CommandFactory, Parser};
use std::path::PathBuf;

fn matches(command: &str, options: &[&str]) -> eyre::Result<clap::ArgMatches> {
    let mut root = crate::Cli::command().try_get_matches_from(
        ["outbe-cli", "tee", command]
            .into_iter()
            .chain(options.iter().copied()),
    )?;
    let (_, mut tee) = root
        .remove_subcommand()
        .ok_or_else(|| eyre::eyre!("missing tee command"))?;
    let (_, command) = tee
        .remove_subcommand()
        .ok_or_else(|| eyre::eyre!("missing lifecycle command"))?;
    Ok(command)
}

#[test]
fn lifecycle_commands_preserve_required_paths_and_optional_node_signer() -> eyre::Result<()> {
    let renew = matches(
        "renew",
        &[
            "--enclave-socket",
            "sidecar.sock",
            "--node-data-dir",
            "node",
        ],
    )?;
    assert_eq!(
        renew.get_one::<String>("enclave_socket").unwrap(),
        "sidecar.sock"
    );
    assert_eq!(
        renew.get_one::<PathBuf>("node_data_dir").unwrap(),
        &PathBuf::from("node")
    );
    assert!(renew.get_one::<PathBuf>("reth_p2p_secret_key").is_none());
    for command in [
        "renew",
        "status",
        "upgrade-prepare",
        "upgrade-provision",
        "upgrade-submit",
    ] {
        let error = crate::Cli::command()
            .try_get_matches_from(["outbe-cli", "tee", command])
            .unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(error.to_string().contains("--node-data-dir"));
    }
    Ok(())
}

#[test]
fn renewal_status_preserves_default_margins_and_explicit_overrides() -> eyre::Result<()> {
    let defaults = matches("status", &["--node-data-dir", "node"])?;
    assert_eq!(defaults.get_one::<u64>("warning_blocks"), Some(&600));
    assert_eq!(defaults.get_one::<u64>("critical_blocks"), Some(&120));
    let explicit = matches(
        "status",
        &[
            "--node-data-dir",
            "node",
            "--warning-blocks",
            "5",
            "--critical-blocks",
            "7",
        ],
    )?;
    // Margin ordering is validated by the status service, after loading the manifest.
    assert_eq!(explicit.get_one::<u64>("critical_blocks"), Some(&7));
    Ok(())
}

#[test]
fn provisioning_preserves_timeout_flags_and_late_binding_validation() -> eyre::Result<()> {
    let required = [
        "--candidate-enclave-socket",
        "candidate.sock",
        "--node-data-dir",
        "node",
        "--genesis",
        "genesis.json",
        "--binding-id",
        "invalid",
        "--valid-until",
        "0",
    ];
    let defaults = matches("upgrade-provision", &required)?;
    assert_eq!(defaults.get_one::<u64>("timeout_secs"), Some(&300));
    assert!(!defaults.get_flag("legacy_direct_dev_source"));
    assert!(!defaults.get_flag("new_attempt"));
    assert_eq!(defaults.get_one::<String>("binding_id").unwrap(), "invalid");
    let options = required
        .into_iter()
        .chain([
            "--timeout-secs",
            "9",
            "--new-attempt",
            "--legacy-direct-dev-source",
        ])
        .collect::<Vec<_>>();
    let explicit = matches("upgrade-provision", &options)?;
    assert_eq!(explicit.get_one::<u64>("timeout_secs"), Some(&9));
    assert!(explicit.get_flag("new_attempt"));
    assert!(explicit.get_flag("legacy_direct_dev_source"));
    Ok(())
}

#[test]
fn joining_and_finalizing_keep_their_distinct_timeout_defaults() -> eyre::Result<()> {
    let join = matches(
        "join",
        &[
            "--enclave-socket",
            "sidecar.sock",
            "--genesis",
            "genesis.json",
            "--binding-id",
            "invalid",
            "--valid-until",
            "0",
        ],
    )?;
    assert_eq!(join.get_one::<u64>("timeout_secs"), Some(&60));
    assert!(join.get_one::<PathBuf>("node_data_dir").is_none());
    let finalize = matches("upgrade-finalize", &["--node-data-dir", "node"])?;
    assert_eq!(finalize.get_one::<u64>("timeout_secs"), Some(&300));
    Ok(())
}

#[tokio::test]
async fn signer_errors_precede_manifest_rpc_and_binding_validation() {
    let cases = [
        (
            "renew",
            vec![
                "--enclave-socket",
                "missing.sock",
                "--node-data-dir",
                "missing",
            ],
            "tee renew requires the global --private-key EVM signer",
        ),
        (
            "upgrade-submit",
            vec![
                "--candidate-enclave-socket",
                "missing.sock",
                "--node-data-dir",
                "missing",
                "--binding-id",
                "invalid",
                "--valid-until",
                "0",
            ],
            "tee upgrade-submit requires the global --private-key EVM signer",
        ),
        (
            "upgrade-provision",
            vec![
                "--candidate-enclave-socket",
                "missing.sock",
                "--node-data-dir",
                "missing",
                "--genesis",
                "missing.json",
                "--binding-id",
                "invalid",
                "--valid-until",
                "0",
            ],
            "upgrade-provision requires --private-key",
        ),
    ];
    for (command, options, expected) in cases {
        let cli =
            crate::Cli::try_parse_from(["outbe-cli", "tee", command].into_iter().chain(options))
                .unwrap();
        let crate::Commands::Tee { cmd } = cli.command else {
            panic!("expected tee command")
        };
        let rpc = RecordingRpc::new([]);
        assert_eq!(
            cmd.run(&rpc, cli.private_key.as_deref())
                .await
                .unwrap_err()
                .to_string(),
            expected
        );
        rpc.assert_done();
        assert!(rpc.recorded_calls().is_empty());
    }
}

#[test]
fn lifecycle_help_contract() {
    let mut cli = crate::Cli::command();
    cli.build();
    let tee = cli.find_subcommand_mut("tee").unwrap();
    let help = tee
        .get_subcommands_mut()
        .map(|command| {
            (
                command.get_name().to_owned(),
                command.render_long_help().to_string(),
            )
        })
        .collect::<Vec<_>>();
    // The acceptance run compares this complete public help output before and after refactoring.
    println!(
        "TEE_HELP_CONTRACT={}",
        serde_json::to_string(&help).unwrap()
    );
    assert!(help
        .iter()
        .filter(|(name, _)| name != "help")
        .all(|(_, text)| text.contains("--rpc-url") && text.contains("--private-key")));
}
