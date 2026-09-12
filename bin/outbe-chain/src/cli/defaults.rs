/// Ceiling for advised gas price: one COEN per gas, already far above anything
/// this chain charges, so a fee spike can never advise an unpayable number.
const OUTBE_MAX_SUGGESTED_GAS_PRICE: u64 = 1_000_000_000_000_000_000;

/// Reth suggests a one gwei tip while its oracle has no sampled block to learn
/// from. Keep the cold-start floor tiny in raw native units and cap sampled
/// advice at one 18-decimal COEN per gas.
pub(crate) fn apply_outbe_gas_price_oracle_defaults<
    C: reth_cli::chainspec::ChainSpecParser,
    Ext,
    SubCmd,
>(
    command: &mut reth_ethereum::cli::interface::Commands<C, Ext, SubCmd>,
) where
    Ext: clap::Args + std::fmt::Debug,
    SubCmd: clap::Subcommand + std::fmt::Debug,
{
    if let reth_ethereum::cli::interface::Commands::Node(node) = command {
        node.rpc.gas_price_oracle.default_suggested_fee = Some(alloy_primitives::U256::from(
            alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE,
        ));
        node.rpc.gas_price_oracle.max_price = OUTBE_MAX_SUGGESTED_GAS_PRICE;
    }
}

pub(crate) fn command_requires_crs<C: reth_cli::chainspec::ChainSpecParser, Ext, SubCmd>(
    command: &reth_ethereum::cli::interface::Commands<C, Ext, SubCmd>,
) -> bool
where
    Ext: clap::Args + std::fmt::Debug,
    SubCmd: clap::Subcommand + std::fmt::Debug,
{
    matches!(command, reth_ethereum::cli::interface::Commands::Node(_))
}

pub(crate) fn initialize_crs_for_command<C, Ext, SubCmd>(
    command: &reth_ethereum::cli::interface::Commands<C, Ext, SubCmd>,
    initialize: impl FnOnce() -> eyre::Result<()>,
) -> eyre::Result<()>
where
    C: reth_cli::chainspec::ChainSpecParser,
    Ext: clap::Args + std::fmt::Debug,
    SubCmd: clap::Subcommand + std::fmt::Debug,
{
    if command_requires_crs(command) {
        initialize()?;
    }
    Ok(())
}
