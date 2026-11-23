use super::*;

#[test]
fn chain_help_keeps_dev_without_advertising_ethereum_networks() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    for arguments in [
        vec!["outbe-chain", "node", "--help"],
        vec!["outbe-chain", "db", "path", "--help"],
    ] {
        let error = <OutbeCli as clap::Parser>::try_parse_from(arguments)
            .expect_err("help must exit without starting a node");
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
        let help = error.to_string();
        let chain_help = help
            .split_once("--chain <CHAIN_OR_PATH>")
            .expect("generated help must include the chain argument")
            .1
            .split("\n      --")
            .next()
            .unwrap();
        assert!(chain_help.contains("dev"), "{chain_help}");
        for alias in ["mainnet", "sepolia", "holesky", "hoodi"] {
            assert!(!chain_help.contains(alias), "{chain_help}");
        }
    }
}

#[test]
fn rejected_chain_diagnostics_do_not_suggest_ethereum_networks() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    for alias in ["mainnet", "sepolia", "holesky", "hoodi"] {
        let error = <OutbeCli as clap::Parser>::try_parse_from([
            "outbe-chain",
            "db",
            "path",
            "--chain",
            alias,
        ])
        .expect_err("upstream genesis must still pass mandatory Outbe validation");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        let rendered = error.to_string();
        assert!(rendered.contains("unknown Outbe chain ID"), "{rendered}");
        assert!(rendered.contains("[possible values: dev]"), "{rendered}");
    }
}

#[test]
fn outbe_rpc_module_validator_accepts_outbe_namespace() {
    use reth_rpc_server_types::{RpcModuleSelection, RpcModuleValidator as _};

    let selection = super::OutbeRpcModuleValidator::parse_selection("eth,net,web3,outbe")
        .expect("outbe namespace should be accepted");
    let RpcModuleSelection::Selection(modules) = selection else {
        panic!("explicit module list should parse as selection");
    };
    assert!(modules.iter().any(|module| module.as_str() == "outbe"));
}

#[test]
fn outbe_rpc_module_validator_rejects_unknown_namespace() {
    use reth_rpc_server_types::RpcModuleValidator as _;

    let err = super::OutbeRpcModuleValidator::parse_selection("eth,outbee")
        .expect_err("typoed custom namespace must be rejected");
    assert!(err.contains("Unknown RPC module: 'outbee'"));
}

#[test]
fn database_cli_requires_an_explicit_chain_instead_of_parsing_mainnet() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    let error = <OutbeCli as clap::Parser>::try_parse_from(["outbe-chain", "db", "path"])
        .expect_err("database commands must require an explicit Outbe ChainSpec");

    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    let rendered = error.to_string();
    assert!(rendered.contains("required argument"), "{rendered}");
    assert!(rendered.contains("chain"), "{rendered}");
    assert!(!rendered.contains("mainnet"), "{rendered}");
}

#[test]
fn node_cli_also_requires_an_explicit_chain() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    let error = <OutbeCli as clap::Parser>::try_parse_from(["outbe-chain", "node"])
        .expect_err("node execution must require an explicit Outbe ChainSpec");

    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert!(error.to_string().contains("chain"));
}

#[test]
fn reth_mainnet_alias_is_not_outbe_mainnet_676() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    let error = <OutbeCli as clap::Parser>::try_parse_from([
        "outbe-chain",
        "db",
        "path",
        "--chain",
        "mainnet",
    ])
    .expect_err("Ethereum mainnet must not satisfy mandatory Outbe ChainSpec validation");

    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    let rendered = error.to_string();
    assert!(rendered.contains("unknown Outbe chain ID 1"), "{rendered}");
    assert!(
        !rendered.contains("unknown Outbe chain ID 676"),
        "{rendered}"
    );
}

/// Verifies that Outbe's `ConsensusArgs` flags (`--upstream`, `--testnet.unix-time-offset-secs`)
/// do not collide with Reth v2.11.0 RPC/IPC flags (`--http*`, `--ipcdisable`, `--ipcpath`,
/// `--auth-ipc.path`) and that `--chain` validates a real `tee_genesis::run` genesis through
/// `OutbeChainSpecParser`.
#[test]
fn node_cli_flag_matrix_parses_http_upstream_time_offset_and_ipc_settings() {
    install_cli_defaults_for_test();
    type OutbeCli = reth_ethereum::cli::interface::Cli<
        super::OutbeChainSpecParser,
        outbe_consensus::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    let temp = tempfile::tempdir().expect("tempdir for IPC path check");
    let genesis_path = temp.path().join("genesis.json");
    crate::tee_genesis::run(&[
        "outbe-chain".into(),
        "tee".into(),
        "genesis".into(),
        "--input".into(),
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/assets/native-genesis.json"
        )
        .into(),
        "--output".into(),
        genesis_path.to_str().expect("utf8 genesis path").into(),
        "--mode".into(),
        "no-attestation-dev".into(),
    ])
    .expect("generate valid Outbe genesis fixture");
    let genesis_str = genesis_path.to_str().expect("utf8 genesis path");
    let custom_ipc = temp.path().join("outbe-seam.ipc");
    let custom_ipc_str = custom_ipc.to_str().expect("utf8 ipc path");
    let custom_auth_ipc = temp.path().join("outbe-auth.ipc");
    let custom_auth_ipc_str = custom_auth_ipc.to_str().expect("utf8 auth ipc path");

    struct Case<'a> {
        http: bool,
        upstream: Option<&'a str>,
        time_offset: Option<i64>,
        ipc_disable: bool,
        ipc_path: Option<&'a str>,
        auth_ipc_path: Option<&'a str>,
    }

    let cases = [
        Case {
            http: false,
            upstream: None,
            time_offset: None,
            ipc_disable: false,
            ipc_path: Some(custom_ipc_str),
            auth_ipc_path: None,
        },
        Case {
            http: true,
            upstream: Some("http://127.0.0.1:18545"),
            time_offset: Some(60),
            ipc_disable: true,
            ipc_path: None,
            auth_ipc_path: Some(custom_auth_ipc_str),
        },
        Case {
            http: true,
            upstream: None,
            time_offset: Some(-30),
            ipc_disable: false,
            ipc_path: Some(custom_ipc_str),
            auth_ipc_path: Some(custom_auth_ipc_str),
        },
        Case {
            http: false,
            upstream: Some("http://127.0.0.1:28545"),
            time_offset: None,
            ipc_disable: true,
            ipc_path: None,
            auth_ipc_path: None,
        },
    ];

    for case in cases {
        let mut argv: Vec<String> = vec![
            "outbe-chain".into(),
            "node".into(),
            "--chain".into(),
            genesis_str.into(),
        ];
        if case.http {
            argv.extend([
                "--http".into(),
                "--http.addr".into(),
                "127.0.0.1".into(),
                "--http.port".into(),
                "0".into(),
                "--http.api".into(),
                "eth,net,web3,outbe".into(),
            ]);
        }
        if let Some(upstream) = case.upstream {
            argv.extend(["--upstream".into(), upstream.into()]);
        }
        if let Some(offset) = case.time_offset {
            argv.push(format!("--testnet.unix-time-offset-secs={offset}"));
        }
        if case.ipc_disable {
            argv.push("--ipcdisable".into());
        }
        if let Some(ipc) = case.ipc_path {
            argv.extend(["--ipcpath".into(), ipc.into()]);
        }
        if let Some(auth_ipc) = case.auth_ipc_path {
            argv.extend(["--auth-ipc.path".into(), auth_ipc.into()]);
        }

        let cli = <OutbeCli as clap::Parser>::try_parse_from(&argv)
            .unwrap_or_else(|err| panic!("flag matrix {:?} failed to parse: {err}", argv));
        let reth_ethereum::cli::interface::Commands::Node(node) = cli.command else {
            panic!("expected node command");
        };
        assert_eq!(node.rpc.http, case.http);
        assert_eq!(node.ext.upstream.as_deref(), case.upstream);
        assert_eq!(node.ext.testnet_unix_time_offset_secs, case.time_offset);
        assert_eq!(node.rpc.ipcdisable, case.ipc_disable);
        if let Some(expected_ipc) = case.ipc_path {
            assert_eq!(node.rpc.ipcpath, expected_ipc);
        }
        if let Some(expected_auth_ipc) = case.auth_ipc_path {
            assert_eq!(node.rpc.auth_ipc_path(), expected_auth_ipc);
        } else {
            let default_rpc = reth_ethereum::node::core::args::RpcServerArgs::default();
            assert_eq!(node.rpc.auth_ipc_path(), default_rpc.auth_ipc_path());
        }
    }
}
