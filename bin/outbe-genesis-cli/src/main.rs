//! CLI entry point for `outbe-genesis-cli`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use outbe_genesis_cli::create_genesis::{
    run_create_genesis_with_min_stake, run_seed_cli_with_min_stake, SeedCliArgs,
};

#[derive(Debug, Parser)]
#[command(
    name = "outbe-genesis-cli",
    about = "Create and seed Outbe genesis.json and validator launch bundles"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to `network.yaml` when invoked directly without a subcommand.
    #[arg(value_name = "CONFIG")]
    config: Option<PathBuf>,

    /// Output genesis path when invoked directly with `<CONFIG>`.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Minimum stake in COEN base units (`unit`, default: `10000000000000000000000000` / `10_000_000` COEN).
    #[arg(long)]
    min_stake: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a complete Outbe `genesis.json` and launch bundle from `network.yaml`.
    Create {
        /// Path to `network.yaml`.
        config: PathBuf,
        /// Output genesis path (default: `output` from the yaml, else `./genesis.json`).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Minimum stake in COEN base units (`unit`, default: `10000000000000000000000000` / `10_000_000` COEN).
        #[arg(long)]
        min_stake: Option<String>,
    },
    /// Seed `genesis.json` with precompile storage entries from a JSON seed profile.
    Seed {
        /// Path to `genesis.json`.
        #[arg(long)]
        genesis: PathBuf,
        /// Path to seed config JSON.
        #[arg(long)]
        seed: PathBuf,
        /// Path to `validators.json` for genesis validator set.
        #[arg(long)]
        validators: Option<PathBuf>,
        /// Output path for patched `genesis.json`.
        #[arg(long)]
        output: PathBuf,
        /// Directory containing contract code/state files referenced from `seed["contracts"]`.
        #[arg(long)]
        contracts_dir: Option<PathBuf>,
        /// Directory holding `metacanon.md` and `canon.md` to seed into the Governance precompile.
        #[arg(long)]
        canon_dir: Option<PathBuf>,
        /// Override the seeded active worldwide-day (`YYYYMMDD`).
        #[arg(long)]
        worldwide_day: Option<u32>,
        /// Do not seed an already-OFFERING WorldwideDay.
        #[arg(long)]
        fresh_metadosis: bool,
        /// Minimum stake in COEN base units (`unit`, default: `10000000000000000000000000` / `10_000_000` COEN).
        #[arg(long)]
        min_stake: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Commands::Create {
            config,
            output,
            min_stake,
        }) => run_create_genesis_with_min_stake(&config, output.as_deref(), min_stake.as_deref())
            .map(|_| ()),
        Some(Commands::Seed {
            genesis,
            seed,
            validators,
            output,
            contracts_dir,
            canon_dir,
            worldwide_day,
            fresh_metadosis,
            min_stake,
        }) => run_seed_cli_with_min_stake(
            &SeedCliArgs {
                genesis,
                seed,
                validators,
                output,
                contracts_dir,
                canon_dir,
                worldwide_day,
                fresh_metadosis,
            },
            min_stake.as_deref(),
        ),
        None => {
            match cli.config {
                Some(config) => run_create_genesis_with_min_stake(
                    &config,
                    cli.output.as_deref(),
                    cli.min_stake.as_deref(),
                )
                .map(|_| ()),
                None => {
                    eprintln!("error: provide a subcommand (`create` or `seed`) or a path to `network.yaml`");
                    return ExitCode::from(2);
                }
            }
        }
    };

    if let Err(err) = result {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
