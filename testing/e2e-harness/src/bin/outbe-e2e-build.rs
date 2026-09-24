use std::path::PathBuf;

use clap::Parser;
use eyre::Result;
use outbe_e2e_harness::artifacts::{build_lane_with_upgrades, BuildLane};

#[derive(Debug, Parser)]
#[command(name = "outbe-e2e-build")]
#[command(about = "Build and attest one exact E2E runtime artifact set")]
struct Cli {
    /// Declarative E2E lane to build.
    #[arg(long, value_enum)]
    lane: BuildLane,

    /// Repository root. Defaults to the workspace containing this crate.
    #[arg(long)]
    repo: Option<PathBuf>,

    /// Output manifest consumed by `outbe-e2e --artifact-manifest`.
    #[arg(long)]
    output: PathBuf,

    /// Maximum Cargo build parallelism.
    #[arg(long, default_value_t = 4)]
    jobs: usize,

    /// Build and attest node 0.3 plus independently signable enclaves 0.2 and 0.3.
    #[arg(long)]
    enclave_upgrades: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo = cli.repo.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("e2e-harness belongs to the workspace")
            .to_path_buf()
    });
    build_lane_with_upgrades(&repo, cli.lane, cli.jobs, &cli.output, cli.enclave_upgrades)
}
