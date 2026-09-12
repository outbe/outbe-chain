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

#[test]
fn only_node_execution_requires_the_crs() {
    install_cli_defaults_for_test();
    type FixtureCli = reth_ethereum::cli::interface::Cli<
        ExplicitFixtureChainSpecParser,
        outbe_engine::args::ConsensusArgs,
        super::OutbeRpcModuleValidator,
    >;

    let node =
        <FixtureCli as clap::Parser>::try_parse_from(["outbe-chain", "node", "--chain", "mainnet"])
            .expect("explicit fixture node chain");
    assert!(super::command_requires_crs(&node.command));
    let mut node_initialized = false;
    super::initialize_crs_for_command(&node.command, || {
        node_initialized = true;
        Ok(())
    })
    .unwrap();
    assert!(node_initialized);

    let database = <FixtureCli as clap::Parser>::try_parse_from([
        "outbe-chain",
        "db",
        "path",
        "--chain",
        "mainnet",
    ])
    .expect("explicit fixture database chain");
    assert!(!super::command_requires_crs(&database.command));
    let mut database_initialized = false;
    super::initialize_crs_for_command(&database.command, || {
        database_initialized = true;
        Ok(())
    })
    .unwrap();
    assert!(!database_initialized);
}
