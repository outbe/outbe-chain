use super::*;

pub(super) fn install_cli_defaults_for_test() {
    let _ = super::outbe_default_txpool_values().try_init();
}

#[derive(Clone, Debug, Default)]
struct ExplicitFixtureChainSpecParser;

impl reth_cli::chainspec::ChainSpecParser for ExplicitFixtureChainSpecParser {
    type ChainSpec = reth_chainspec::ChainSpec<outbe_primitives::OutbeHeader>;

    const SUPPORTED_CHAINS: &'static [&'static str] = &["mainnet"];

    fn default_value() -> Option<&'static str> {
        None
    }

    fn parse(value: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        Ok(reth_ethereum::cli::chainspec::chain_value_parser(value)?
            .as_ref()
            .clone()
            .map_header(outbe_primitives::OutbeHeader::new)
            .into())
    }
}

type FixtureCli = reth_ethereum::cli::interface::Cli<
    ExplicitFixtureChainSpecParser,
    outbe_consensus::args::ConsensusArgs,
    super::OutbeRpcModuleValidator,
>;

#[test]
fn only_node_execution_requires_the_crs() {
    install_cli_defaults_for_test();
    let node =
        <FixtureCli as clap::Parser>::try_parse_from(["outbe-chain", "node", "--chain", "mainnet"])
            .expect("explicit fixture node chain");
    assert!(super::command_requires_crs(&node.command));

    let database = <FixtureCli as clap::Parser>::try_parse_from([
        "outbe-chain",
        "db",
        "path",
        "--chain",
        "mainnet",
    ])
    .expect("explicit fixture database chain");
    assert!(!super::command_requires_crs(&database.command));
}

#[test]
fn node_parses_optional_consensus_metrics_address() {
    install_cli_defaults_for_test();
    let default =
        <FixtureCli as clap::Parser>::try_parse_from(["outbe-chain", "node", "--chain", "mainnet"])
            .expect("fixture node without consensus metrics");
    let configured = <FixtureCli as clap::Parser>::try_parse_from([
        "outbe-chain",
        "node",
        "--chain",
        "mainnet",
        "--consensus.metrics-addr",
        "127.0.0.1:9091",
    ])
    .expect("fixture node with consensus metrics");
    for (cli, expected) in [
        (default, None),
        (configured, Some("127.0.0.1:9091".parse().unwrap())),
    ] {
        let reth_ethereum::cli::interface::Commands::Node(node) = cli.command else {
            panic!("expected node command");
        };
        assert_eq!(node.ext.metrics_address, expected);
    }
}
