//! Local single-VM data disk preparation (`outbe-up disk prepare`).
//!
//! ## Signal handling and staging cleanup
//!
//! `SIGINT` and `SIGTERM` flip a cooperative cancellation flag that terminates child process
//! groups (`outbe-keygen`, `gzip`, `sfdisk`, `mke2fs`, `debugfs`) and unwinds all staging
//! `TempDir` guards (`.outbe-prepare-*`, `.outbe-disk-*`, `.outbe-vm-disk-*`) before exiting
//! non-zero without publishing the target image.
//!
//! An uncatchable `SIGKILL` (or sudden host power loss) cannot run in-process cleanup; pass
//! `--cleanup-stale` to remove leftover current-UID staging directories in the output directory.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    disk_tree::{
        cleanup_stale_staging_dirs, prepare_founder_tree_cancellable,
        prepare_full_node_tree_cancellable, prepare_joining_validator_tree_cancellable,
        FounderKeyVerify,
    },
    progress::Progress,
    NodeRole,
};
use outbe_operator_vm::disk::{build_disk_image_cancellable, sha256_sparse_file};
use serde::Serialize;
use tokio::signal::unix::{signal, SignalKind};

const GIB: u64 = 1024 * 1024 * 1024;

pub struct PrepareArgs {
    pub role: NodeRole,
    pub source: Option<PathBuf>,
    pub bundle: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub keys_from: Option<PathBuf>,
    pub skip_key_verify: bool,
    pub cleanup_stale: bool,
    pub size_gib: u32,
    pub output: Option<PathBuf>,
    pub device: Option<PathBuf>,
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct PrepareOutput {
    role: NodeRole,
    output: PathBuf,
    size_gib: u32,
    sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    validator_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hyperlane_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keygen_source: Option<String>,
}

fn ensure_output_absent(output: &Path) -> Result<()> {
    match fs::symlink_metadata(output) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).wrap_err_with(|| format!("failed to stat {}", output.display())),
        Ok(_) => Err(eyre::eyre!(
            "output disk image already exists: {}",
            output.display()
        )),
    }
}

pub async fn prepare(args: PrepareArgs) -> Result<()> {
    if let Some(device) = args.device {
        // TODO(S3-probe): Support raw block devices after the S3 hardware probe confirms
        // dstack VMM attachment and guest mount semantics (owner decision R-3 in
        // /home/ubuntu/tdx/15-design-outbe-up-split.md §§6 & 7/S3).
        bail!(
            "block device target ({}) is not yet supported, see S3 probe (owner decision R-3)",
            device.display()
        );
    }

    let output = args
        .output
        .clone()
        .ok_or_else(|| eyre::eyre!("either --output or --device must be specified"))?;
    ensure!(args.size_gib > 0, "disk size must be positive");
    ensure_output_absent(&output)?;

    // Validate role-specific flag combinations before touching disk or installing signal handlers.
    match args.role {
        NodeRole::Founder => {
            ensure!(
                args.config.is_none() && args.keys_from.is_none(),
                "founder role does not accept --config or --keys-from"
            );
            ensure!(
                args.source.is_some(),
                "--source is required for --role founder"
            );
            ensure!(
                !(args.bundle.is_some() && args.skip_key_verify),
                "cannot combine --bundle with --skip-key-verify for --role founder"
            );
        }
        NodeRole::JoiningValidator => {
            ensure!(
                args.source.is_none(),
                "joining-validator role accepts --bundle and --config (not --source)"
            );
            ensure!(
                !args.skip_key_verify,
                "--skip-key-verify is only valid for --role founder"
            );
            ensure!(
                args.bundle.is_some(),
                "--bundle is required for --role joining-validator"
            );
            ensure!(
                args.config.is_some(),
                "--config is required for --role joining-validator"
            );
        }
        NodeRole::FullNode => {
            ensure!(
                args.source.is_none(),
                "full-node role accepts --bundle and --config (not --source)"
            );
            ensure!(
                args.keys_from.is_none(),
                "full-node role uses zero keys on disk; --keys-from is not allowed for full-node"
            );
            ensure!(
                !args.skip_key_verify,
                "--skip-key-verify is only valid for --role founder"
            );
            ensure!(
                args.bundle.is_some(),
                "--bundle is required for --role full-node"
            );
            ensure!(
                args.config.is_some(),
                "--config is required for --role full-node"
            );
        }
    }

    let staging_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&staging_parent)?;

    if args.cleanup_stale {
        let removed = cleanup_stale_staging_dirs(&staging_parent)?;
        if !args.json && !removed.is_empty() {
            eprintln!(
                "removed {} stale staging director{} in {}",
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" },
                staging_parent.display()
            );
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let cancel_signal = Arc::clone(&cancel);
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
            }
            _ = sigterm.recv() => {
                cancel_signal.store(true, Ordering::SeqCst);
            }
        }
    });

    let cancel_worker = Arc::clone(&cancel);
    let output_for_worker = output.clone();
    let json = args.json;
    let role = args.role;
    let size_gib = args.size_gib;

    let work_result = tokio::task::spawn_blocking(move || -> Result<PrepareOutput> {
        let mut progress = if json { None } else { Some(Progress::new()) };
        if let Some(p) = &mut progress {
            p.stage(&format!("staging and validating {role} data tree"));
        }

        let staged = match role {
            NodeRole::Founder => {
                let source = args
                    .source
                    .as_deref()
                    .expect("validated above: founder requires --source");
                let verify = if args.skip_key_verify {
                    FounderKeyVerify::Skip
                } else if let Some(bundle) = args.bundle.as_deref() {
                    FounderKeyVerify::Bundle(bundle)
                } else {
                    FounderKeyVerify::Auto
                };
                prepare_founder_tree_cancellable(source, &staging_parent, verify, &cancel_worker)?
            }
            NodeRole::JoiningValidator => {
                let bundle = args
                    .bundle
                    .as_deref()
                    .expect("validated above: joining-validator requires --bundle");
                let config = args
                    .config
                    .as_deref()
                    .expect("validated above: joining-validator requires --config");
                prepare_joining_validator_tree_cancellable(
                    bundle,
                    config,
                    args.keys_from.as_deref(),
                    &staging_parent,
                    &cancel_worker,
                )?
            }
            NodeRole::FullNode => {
                let bundle = args
                    .bundle
                    .as_deref()
                    .expect("validated above: full-node requires --bundle");
                let config = args
                    .config
                    .as_deref()
                    .expect("validated above: full-node requires --config");
                prepare_full_node_tree_cancellable(
                    bundle,
                    config,
                    None,
                    &staging_parent,
                    &cancel_worker,
                )?
            }
        };

        if let Some(p) = &mut progress {
            p.stage(&format!(
                "building {size_gib} GiB raw GPT+ext4 data disk for {role}"
            ));
        }
        let disk_bytes = u64::from(size_gib) * GIB;
        build_disk_image_cancellable(
            staged.tree_path(),
            &output_for_worker,
            disk_bytes,
            &cancel_worker,
        )?;
        let sha256 = sha256_sparse_file(&output_for_worker)?;

        let key_verified = match staged.role {
            NodeRole::Founder | NodeRole::JoiningValidator => Some(staged.key_verified),
            NodeRole::FullNode => None,
        };

        if let Some(p) = &mut progress {
            p.done(&format!(
                "wrote {} VM data disk to {}",
                staged.role,
                output_for_worker.display()
            ));
        }

        Ok(PrepareOutput {
            role: staged.role,
            output: output_for_worker,
            size_gib,
            sha256,
            validator_address: staged.validator_address.map(|addr| format!("{addr:#x}")),
            hyperlane_address: staged.hyperlane_address.map(|addr| format!("{addr:#x}")),
            key_verified,
            keygen_source: staged.keygen_source,
        })
    })
    .await
    .wrap_err("disk prepare worker panicked")?;

    signal_task.abort();
    let summary = work_result?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Role:              {}", summary.role);
        println!("Output:            {}", summary.output.display());
        println!("Size:              {} GiB", summary.size_gib);
        println!("SHA-256:           {}", summary.sha256);
        if let Some(addr) = &summary.validator_address {
            println!("Validator address: {addr}");
        }
        if let Some(addr) = &summary.hyperlane_address {
            println!("Hyperlane signer:  {addr}");
        }
        if let Some(verified) = summary.key_verified {
            println!("BLS key verified:  {verified}");
        }
        if let Some(source) = &summary.keygen_source {
            println!("Keygen source:     {source}");
        }
    }

    Ok(())
}
