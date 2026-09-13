use super::*;

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
        outbe_engine::args::ConsensusArgs,
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
        outbe_engine::args::ConsensusArgs,
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
        outbe_engine::args::ConsensusArgs,
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
