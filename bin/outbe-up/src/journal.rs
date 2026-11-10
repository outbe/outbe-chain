use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{config::OperatorConfig, process::STATE_ROOT};

const JOURNAL_VERSION: u32 = 1;
const JOURNAL_NAME: &str = "validator-state.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub bundle_sha256: String,
    pub config_sha256: String,
    pub config: OperatorConfig,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub binding_id: Option<String>,
    #[serde(default)]
    pub valid_until: Option<u64>,
    #[serde(default)]
    pub tee_join_complete: bool,
    #[serde(default)]
    pub stake_transaction: Option<String>,
    #[serde(default)]
    pub confirm_ready_transaction: Option<String>,
    #[serde(default)]
    pub snapshot_sha256: Option<String>,
    #[serde(default)]
    pub snapshot_restore_in_progress: Option<String>,
    #[serde(default)]
    pub snapshot_finalized_height: Option<u64>,
}

impl Journal {
    pub fn new(bundle_sha256: String, config_sha256: String, config: OperatorConfig) -> Self {
        Self {
            version: JOURNAL_VERSION,
            bundle_sha256,
            config_sha256,
            config,
            installed: false,
            binding_id: None,
            valid_until: None,
            tee_join_complete: false,
            stake_transaction: None,
            confirm_ready_transaction: None,
            snapshot_sha256: None,
            snapshot_restore_in_progress: None,
            snapshot_finalized_height: None,
        }
    }

    pub fn load() -> Result<Self> {
        let path = path();
        let bytes = fs::read(&path)
            .wrap_err_with(|| format!("validator is not prepared: {} is absent", path.display()))?;
        let journal: Self = serde_json::from_slice(&bytes).wrap_err("invalid lifecycle journal")?;
        ensure!(
            journal.version == JOURNAL_VERSION,
            "unsupported lifecycle journal version {}",
            journal.version
        );
        Ok(journal)
    }

    pub fn save(&self) -> Result<()> {
        let root = Path::new(STATE_ROOT);
        fs::create_dir_all(root).wrap_err("failed to create lifecycle state directory")?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let temporary = root.join(format!(".{JOURNAL_NAME}.{}", std::process::id()));
        if temporary.exists() {
            fs::remove_file(&temporary).wrap_err("failed to remove stale lifecycle journal")?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .wrap_err("failed to create lifecycle journal")?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path()).wrap_err("failed to publish lifecycle journal")?;
        Ok(())
    }
}

pub fn path() -> PathBuf {
    Path::new(STATE_ROOT).join(JOURNAL_NAME)
}
