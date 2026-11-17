//! Read-only status inputs.

use std::path::PathBuf;

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Also inspect the binding selected by this directory's committed NodeHost manifest.
    /// Omit for network status without accessing local files.
    #[arg(long)]
    pub(super) node_data_dir: Option<PathBuf>,
    /// Emit warning once an unsafe lease is this many blocks from freeze.
    #[arg(long, default_value_t = 600, requires = "node_data_dir")]
    pub(super) warning_blocks: u64,
    /// Emit critical once an unsafe lease is this many blocks from freeze.
    #[arg(long, default_value_t = 120, requires = "node_data_dir")]
    pub(super) critical_blocks: u64,
}
