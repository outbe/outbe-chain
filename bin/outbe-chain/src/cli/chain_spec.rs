use crate::*;

#[derive(Debug, Clone, Default)]
pub(crate) struct OutbeChainSpecParser;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OutbeRpcModuleValidator;

impl RpcModuleValidator for OutbeRpcModuleValidator {
    fn parse_selection(s: &str) -> Result<RpcModuleSelection, String> {
        let selection = s
            .parse::<RpcModuleSelection>()
            .map_err(|error| format!("Failed to parse RPC modules: {error}"))?;

        if let RpcModuleSelection::Selection(modules) = &selection {
            for module in modules {
                let RethRpcModule::Other(name) = module else {
                    continue;
                };
                if name != "outbe" {
                    return Err(format!("Unknown RPC module: '{name}'"));
                }
            }
        }

        Ok(selection)
    }
}

impl ChainSpecParser for OutbeChainSpecParser {
    type ChainSpec = ChainSpec<OutbeHeader>;

    const SUPPORTED_CHAINS: &'static [&'static str] =
        reth_ethereum::cli::chainspec::SUPPORTED_CHAINS;

    fn default_value() -> Option<&'static str> {
        None
    }

    fn parse(s: &str) -> eyre::Result<Arc<Self::ChainSpec>> {
        let chain_spec: Arc<Self::ChainSpec> =
            reth_ethereum::cli::chainspec::chain_value_parser(s)?
                .as_ref()
                .clone()
                .map_header(OutbeHeader::new)
                .into();
        validate_outbe_chain_spec(chain_spec.as_ref())?;
        outbe_consensus::proof::init_consensus_chain_id(chain_spec.chain().id())
            .map_err(|error| eyre::eyre!("invalid consensus chain identity: {error}"))?;
        Ok(chain_spec)
    }
}

pub(crate) fn validate_outbe_chain_spec(chain_spec: &ChainSpec<OutbeHeader>) -> eyre::Result<()> {
    let chain_id = chain_spec.chain().id();
    eyre::ensure!(
        outbe_primitives::chain::network_for_chain_id(chain_id).is_some(),
        "unknown Outbe chain ID {chain_id}"
    );
    outbe_evm::tee_attestation_activation::TeeAttestationChainSpecStateV1::from_chain_spec(
        chain_spec,
    )
    .activation()
    .map_err(|error| eyre::eyre!("invalid mandatory teeAttestationV1 ChainSpec: {error}"))?;
    outbe_node::ocomp::fork::require_startup_ocomp_fork_install(chain_spec)?;
    outbe_chain_constants::initialize(
        chain_spec
            .genesis
            .config
            .extra_fields
            .get(outbe_chain_constants::GENESIS_CONFIG_KEY),
    )
    .map_err(|error| eyre::eyre!("invalid config.outbeProtocol: {error}"))?;
    Ok(())
}
