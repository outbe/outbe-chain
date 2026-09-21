//! Offline snapshot commands, dispatched before node startup.

use clap::Parser;

mod create;

#[cfg(test)]
mod validate;

#[derive(Parser)]
#[command(name = "outbe-chain snapshot")]
pub(crate) struct SnapshotCli {
    #[command(subcommand)]
    pub command: SnapshotCommand,
}

#[derive(clap::Subcommand)]
pub(crate) enum SnapshotCommand {
    /// Package native files while outbe-chain, OCOMP and CE writers are stopped.
    Create(create::CreateArgs),
}

pub(crate) fn run(args: &[String]) -> eyre::Result<()> {
    let cli = SnapshotCli::parse_from(
        std::iter::once(args[0].clone()).chain(args.iter().skip(2).cloned()),
    );
    match cli.command {
        SnapshotCommand::Create(args) => create::run(args),
    }
}
