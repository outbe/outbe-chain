use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use eyre::{ensure, WrapErr as _};
use outbe_chain_constants::{
    GenesisProtocolParametersV1, CHAIN_CONSTANTS_ADDRESS, CHAIN_CONSTANTS_MARKER_CODE,
    GENESIS_CONFIG_KEY,
};

#[derive(Debug, Parser)]
#[command(name = "outbe-chain-constants")]
struct ConstantsCli {
    #[command(subcommand)]
    command: ConstantsCommand,
}

#[derive(Debug, Subcommand)]
enum ConstantsCommand {
    /// Resolve config.outbeProtocol and materialize its immutable state record.
    Genesis(ConstantsGenesisArgs),
}

#[derive(Debug, clap::Args)]
struct ConstantsGenesisArgs {
    /// Seeded genesis JSON containing optional config.outbeProtocol overrides.
    #[arg(long)]
    input: PathBuf,
    /// New genesis JSON with the immutable resolved constants account.
    #[arg(long)]
    output: PathBuf,
}

pub(crate) fn run(arguments: &[String]) -> eyre::Result<()> {
    let mut command_arguments = vec![arguments[0].clone()];
    command_arguments.extend_from_slice(&arguments[2..]);
    match ConstantsCli::parse_from(command_arguments).command {
        ConstantsCommand::Genesis(args) => generate(&args),
    }
}

fn generate(args: &ConstantsGenesisArgs) -> eyre::Result<()> {
    ensure!(
        args.input != args.output,
        "--input and --output must differ"
    );
    require_new_output(&args.output)?;

    let input = fs::read(&args.input)
        .wrap_err_with(|| format!("read constants genesis input {}", args.input.display()))?;
    let mut genesis: serde_json::Value =
        serde_json::from_slice(&input).wrap_err("decode constants genesis input JSON")?;
    let resolved = materialize(&mut genesis)?;
    let mut output = serde_json::to_vec_pretty(&genesis)?;
    output.push(b'\n');
    write_new_file(&args.output, &output)?;

    println!(
        "wrote immutable chain constants: hash={}, output={}",
        resolved.parameters_hash(),
        args.output.display()
    );
    Ok(())
}

fn materialize(genesis: &mut serde_json::Value) -> eyre::Result<GenesisProtocolParametersV1> {
    let config = genesis
        .get("config")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre::eyre!("input genesis config must be a JSON object"))?;
    let resolved = GenesisProtocolParametersV1::resolve(config.get(GENESIS_CONFIG_KEY))?;

    let alloc = genesis
        .get_mut("alloc")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| eyre::eyre!("input genesis alloc must be a JSON object"))?;
    let address = format!("{CHAIN_CONSTANTS_ADDRESS:#x}");
    ensure!(
        !alloc
            .keys()
            .any(|existing| normalized_address(existing) == normalized_address(&address)),
        "input genesis already contains the chain constants account {address}"
    );

    let storage = resolved
        .genesis_storage_words()
        .into_iter()
        .map(|(slot, value)| {
            (
                format!("{slot:#066x}"),
                serde_json::json!(format!("{value:#066x}")),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    alloc.insert(
        address,
        serde_json::json!({
            "balance": "0x0",
            "code": format!("0x{}", hex::encode(CHAIN_CONSTANTS_MARKER_CODE)),
            "storage": storage,
        }),
    );
    Ok(resolved)
}

fn normalized_address(value: &str) -> String {
    value
        .strip_prefix("0x")
        .unwrap_or(value)
        .trim_start_matches('0')
        .to_ascii_lowercase()
}

fn require_new_output(path: &Path) -> eyre::Result<()> {
    ensure!(
        !path.exists(),
        "refusing to overwrite constants genesis output: {}",
        path.display()
    );
    ensure!(
        path.parent().is_some_and(Path::exists),
        "constants genesis output parent does not exist: {}",
        path.display()
    );
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> eyre::Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("create constants genesis output {}", path.display()))?;
    output
        .write_all(bytes)
        .and_then(|()| output.sync_all())
        .wrap_err_with(|| format!("write constants genesis output {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_chain_constants::{
        DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS, SLOT_INSTALLED,
        SLOT_METADOSIS_FORMING_PERIOD_SECONDS,
    };
    use serde_json::json;

    #[test]
    fn absent_overrides_materialize_complete_defaults() {
        let mut genesis = json!({ "config": {}, "alloc": {} });
        let resolved = materialize(&mut genesis).unwrap();
        assert_eq!(
            resolved.metadosis_forming_period_seconds,
            DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS
        );
        let account = constants_account(&genesis);
        assert_eq!(account["code"], "0xef");
        assert_eq!(
            storage_word(account, SLOT_METADOSIS_FORMING_PERIOD_SECONDS),
            DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS
        );
        assert_eq!(storage_word(account, SLOT_INSTALLED), 1);
    }

    #[test]
    fn explicit_overrides_are_resolved_before_storage_is_written() {
        let mut genesis = json!({
            "config": {
                "outbeProtocol": {
                    "metadosis": { "formingPeriodSeconds": 60 },
                    "ocomp": { "computeVoteWindowBlocks": 120 }
                }
            },
            "alloc": {}
        });
        let resolved = materialize(&mut genesis).unwrap();
        assert_eq!(resolved.metadosis_forming_period_seconds, 60);
        assert_eq!(
            storage_word(
                constants_account(&genesis),
                SLOT_METADOSIS_FORMING_PERIOD_SECONDS
            ),
            60
        );
    }

    #[test]
    fn invalid_config_and_existing_account_are_rejected_without_mutation() {
        let mut invalid = json!({
            "config": { "outbeProtocol": { "metadosis": { "formingPeriodSeconds": 0 } } },
            "alloc": {}
        });
        let before = invalid.clone();
        assert!(materialize(&mut invalid).is_err());
        assert_eq!(invalid, before);

        let address = format!("{CHAIN_CONSTANTS_ADDRESS:#x}");
        let mut existing = json!({ "config": {}, "alloc": { address: { "code": "0xef" } } });
        assert!(materialize(&mut existing).is_err());
    }

    fn constants_account(genesis: &serde_json::Value) -> &serde_json::Value {
        let address = format!("{CHAIN_CONSTANTS_ADDRESS:#x}");
        &genesis["alloc"][address]
    }

    fn storage_word(account: &serde_json::Value, ordinal: u64) -> u64 {
        let key = format!("0x{ordinal:064x}");
        let value = account["storage"][key].as_str().unwrap();
        u64::from_str_radix(value.trim_start_matches("0x"), 16).unwrap()
    }
}
