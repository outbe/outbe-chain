//! Command-owned lifecycle inputs. Global relay credentials stay with the caller.
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct UpgradeCandidateArgs {
    #[arg(long)]
    pub(super) candidate_enclave_socket: String,
    #[arg(long)]
    pub(super) node_data_dir: PathBuf,
}

#[derive(clap::Args)]
pub struct RenewArgs {
    /// Production enclave sidecar endpoint.
    #[arg(long)]
    pub(super) enclave_socket: String,
    /// Resolved chain-specific node data directory containing NodeHost state.
    #[arg(long)]
    pub(super) node_data_dir: PathBuf,
    /// Persistent Reth P2P secret file.
    #[arg(long)]
    pub(super) reth_p2p_secret_key: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Resolved chain-specific node data directory containing NodeHost state.
    #[arg(long)]
    pub(super) node_data_dir: PathBuf,
    /// Emit warning once an unsafe lease is this many blocks from freeze.
    #[arg(long, default_value_t = 600)]
    pub(super) warning_blocks: u64,
    /// Emit critical once an unsafe lease is this many blocks from freeze.
    #[arg(long, default_value_t = 120)]
    pub(super) critical_blocks: u64,
}

#[derive(clap::Args)]
pub struct UpgradePrepareArgs {
    #[command(flatten)]
    pub(super) candidate: UpgradeCandidateArgs,
    #[arg(long)]
    pub(super) active_tee_dir: PathBuf,
    #[arg(long)]
    pub(super) candidate_tee_dir: PathBuf,
    #[arg(long)]
    pub(super) reth_p2p_secret_key: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct UpgradeProvisionArgs {
    #[command(flatten)]
    pub(super) candidate: UpgradeCandidateArgs,
    #[arg(long)]
    pub(super) reth_p2p_secret_key: Option<PathBuf>,
    #[arg(long)]
    pub(super) genesis: PathBuf,
    #[arg(long)]
    pub(super) binding_id: String,
    #[arg(long)]
    pub(super) valid_until: u64,
    #[arg(long, default_value_t = 300)]
    pub(super) timeout_secs: u64,
    #[arg(long)]
    pub(super) legacy_direct_dev_source: bool,
    #[arg(long)]
    pub(super) new_attempt: bool,
}

#[derive(clap::Args)]
pub struct UpgradeSubmitArgs {
    #[command(flatten)]
    pub(super) candidate: UpgradeCandidateArgs,
    #[arg(long)]
    pub(super) reth_p2p_secret_key: Option<PathBuf>,
    #[arg(long)]
    pub(super) binding_id: String,
    #[arg(long)]
    pub(super) valid_until: u64,
}
