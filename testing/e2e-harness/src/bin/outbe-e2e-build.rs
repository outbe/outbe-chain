use std::path::PathBuf;

use clap::Parser;
use eyre::Result;
use outbe_e2e_harness::artifacts::{
    build_lane, refresh_harness_lane, BuildLane, HarnessRefreshOptions,
};

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
    #[arg(long, default_value_t = 8)]
    jobs: usize,

    /// Reuse and verify byte-identical runtime artifacts from this manifest.
    #[arg(long)]
    reuse_runtime_manifest: Option<PathBuf>,

    /// Authenticate an existing dirty baseline using its saved git diff --binary HEAD.
    #[arg(long, requires = "reuse_runtime_manifest")]
    prior_source_diff: Option<PathBuf>,

    /// Refresh the interpreted validator genesis seeder and its layout-test evidence.
    #[arg(long, requires_all = ["reuse_runtime_manifest", "prior_source_diff"])]
    refresh_genesis_tooling: bool,
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
    if let Some(prior) = cli.reuse_runtime_manifest {
        refresh_harness_lane(
            &repo,
            cli.lane,
            cli.jobs,
            HarnessRefreshOptions {
                prior_manifest: &prior,
                prior_source_diff: cli.prior_source_diff.as_deref(),
                refresh_genesis_tooling: cli.refresh_genesis_tooling,
            },
            &cli.output,
        )
    } else {
        build_lane(&repo, cli.lane, cli.jobs, &cli.output)
    }
}
