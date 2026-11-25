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
    os::unix::fs::{MetadataExt, PermissionsExt},
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
        prepare_founder_tree_from_reader_cancellable, prepare_full_node_tree_cancellable,
        prepare_joining_validator_tree_cancellable, FounderKeyVerify,
    },
    progress::Progress,
    NodeRole,
};
use outbe_operator_vm::{
    disk::{build_disk_image_cancellable, sha256_sparse_file},
    local::{default_state_dir, expand_tilde, validate_vm_name, LocalVmStore},
};
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
    pub staging_name: Option<String>,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
    pub device: Option<PathBuf>,
    pub json: bool,
}

pub struct CleanArgs {
    pub staging_name: Option<String>,
    pub stale: bool,
    pub list: bool,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct PrepareOutput {
    role: NodeRole,
    output: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    staging_name: Option<String>,
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

#[derive(Debug, Serialize)]
struct CleanStaleOutput {
    staging_root: PathBuf,
    removed: Vec<PathBuf>,
}

fn open_store(state_dir: Option<&Path>, fix_permissions: bool) -> Result<LocalVmStore> {
    let dir = match state_dir {
        Some(path) => expand_tilde(path)?,
        None => default_state_dir()?,
    };
    Ok(LocalVmStore::new(dir).with_fix_permissions(fix_permissions))
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

    ensure!(args.size_gib > 0, "disk size must be positive");

    // Validate role-specific flag combinations before touching disk or installing signal handlers.
    match args.role {
        NodeRole::Founder => {
            ensure!(
                args.config.is_none() && args.keys_from.is_none(),
                "founder role does not accept --config or --keys-from"
            );
            let source = args
                .source
                .as_deref()
                .ok_or_else(|| eyre::eyre!("--source is required for --role founder"))?;
            ensure!(
                !(args.bundle.is_some() && args.skip_key_verify),
                "cannot combine --bundle with --skip-key-verify for --role founder"
            );
            if source == Path::new("-") {
                ensure!(
                    args.bundle.is_some() || args.skip_key_verify,
                    "reading founder tar from stdin (`--source -`) requires --bundle <operator-bundle.tar.zst> or --skip-key-verify"
                );
            }
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

    enum Target {
        Output(PathBuf),
        Staging {
            store: LocalVmStore,
            staging_name: String,
        },
    }

    let (target, staging_parent) = match (args.output.clone(), args.staging_name.clone()) {
        (Some(output), None) => {
            ensure_output_absent(&output)?;
            let parent = output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            fs::create_dir_all(&parent)?;
            (Target::Output(output), parent)
        }
        (None, Some(staging_name)) => {
            validate_vm_name(&staging_name)?;
            let store = open_store(args.state_dir.as_deref(), args.fix_permissions)?;
            let staging_root = store.ensure_staging_root()?;
            (
                Target::Staging {
                    store,
                    staging_name,
                },
                staging_root,
            )
        }
        (Some(_), Some(_)) => {
            return Err(eyre::eyre!("cannot combine --output with --staging-name"))
        }
        (None, None) => {
            return Err(eyre::eyre!(
                "one of --output, --staging-name, or --device must be specified"
            ))
        }
    };

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
    let json = args.json;
    let role = args.role;
    let size_gib = args.size_gib;

    let work_result = tokio::task::spawn_blocking(move || -> Result<PrepareOutput> {
        let mut progress = if json { None } else { Some(Progress::new()) };

        let _staging_lock = match &target {
            Target::Output(_) => None,
            Target::Staging {
                store,
                staging_name,
            } => Some(store.lock_staging(staging_name)?),
        };

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
                if source == Path::new("-") {
                    prepare_founder_tree_from_reader_cancellable(
                        io::stdin(),
                        &staging_parent,
                        verify,
                        &cancel_worker,
                    )?
                } else {
                    prepare_founder_tree_cancellable(
                        source,
                        &staging_parent,
                        verify,
                        &cancel_worker,
                    )?
                }
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
        let (final_output, staging_name_out) = match target {
            Target::Output(output_path) => {
                build_disk_image_cancellable(
                    staged.tree_path(),
                    &output_path,
                    disk_bytes,
                    &cancel_worker,
                )?;
                (output_path, None)
            }
            Target::Staging {
                store,
                staging_name,
            } => {
                let staging_dir = store.ensure_staging_dir(&staging_name)?;
                let final_path = store.staged_disk_path(&staging_name)?;
                if let Ok(meta) = fs::symlink_metadata(&final_path) {
                    ensure!(
                        meta.file_type().is_file(),
                        "refusing to overwrite non-regular staged disk path: {}",
                        final_path.display()
                    );
                    let current_uid = rustix::process::getuid().as_raw();
                    ensure!(
                        meta.uid() == current_uid,
                        "staged disk {} is owned by uid {}, expected current uid {current_uid}",
                        final_path.display(),
                        meta.uid()
                    );
                }
                // Remove any leftover `.data.img.tmp.*` temp dirs inside `staging/<name>/` from
                // a prior crashed prepare (safe because `lock_staging(name)` is held exclusively).
                let current_uid = rustix::process::getuid().as_raw();
                if let Ok(entries) = fs::read_dir(&staging_dir) {
                    for entry in entries.flatten() {
                        let fname = entry.file_name();
                        if fname
                            .to_str()
                            .is_some_and(|s| s.starts_with(".data.img.tmp."))
                        {
                            let p = entry.path();
                            if let Ok(m) = fs::symlink_metadata(&p) {
                                if m.file_type().is_dir() && m.uid() == current_uid {
                                    let _ = fs::remove_dir_all(&p);
                                }
                            }
                        }
                    }
                }
                // Build inside `staging/<name>/.data.img.tmp.<rand>/` under the already-held
                // `lock_staging(name)` so `disk clean --stale` (which sweeps `.outbe-*` directly
                // under `staging/`) can never delete an in-flight prepare's publish directory.
                let publish_work = tempfile::Builder::new()
                    .prefix(".data.img.tmp.")
                    .tempdir_in(&staging_dir)
                    .wrap_err_with(|| {
                        format!(
                            "create temporary staging publish dir in {}",
                            staging_dir.display()
                        )
                    })?;
                fs::set_permissions(publish_work.path(), fs::Permissions::from_mode(0o700))?;
                let tmp_img = publish_work.path().join("data.img");
                build_disk_image_cancellable(
                    staged.tree_path(),
                    &tmp_img,
                    disk_bytes,
                    &cancel_worker,
                )?;
                fs::rename(&tmp_img, &final_path).wrap_err_with(|| {
                    format!(
                        "atomically publish staged disk image to {}",
                        final_path.display()
                    )
                })?;
                if let Ok(dir_fd) = fs::File::open(&staging_dir) {
                    let _ = dir_fd.sync_all();
                }
                (final_path, Some(staging_name))
            }
        };

        let staged_role = staged.role;
        let validator_address = staged.validator_address.map(|addr| format!("{addr:#x}"));
        let hyperlane_address = staged.hyperlane_address.map(|addr| format!("{addr:#x}"));
        let key_verified = match staged_role {
            NodeRole::Founder | NodeRole::JoiningValidator => Some(staged.key_verified),
            NodeRole::FullNode => None,
        };
        let keygen_source = staged.keygen_source.clone();
        // Drop extracted temporary key tree immediately after building the raw disk image.
        drop(staged);

        let sha256 = sha256_sparse_file(&final_output)?;

        if let Some(p) = &mut progress {
            p.done(&format!(
                "wrote {} VM data disk to {}",
                staged_role,
                final_output.display()
            ));
        }

        Ok(PrepareOutput {
            role: staged_role,
            output: final_output,
            staging_name: staging_name_out,
            size_gib,
            sha256,
            validator_address,
            hyperlane_address,
            key_verified,
            keygen_source,
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
        if let Some(name) = &summary.staging_name {
            println!("Staging name:      {name}");
        }
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

pub fn clean(args: CleanArgs) -> Result<()> {
    let store = open_store(args.state_dir.as_deref(), args.fix_permissions)?;

    if let Some(staging_name) = args.staging_name {
        let out = store.clean_staging_dir(&staging_name)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else if out.removed {
            println!(
                "Removed staged disk `{}` ({})",
                out.staging_name,
                out.path.display()
            );
        } else {
            println!(
                "Staged disk `{}` is already absent ({})",
                out.staging_name,
                out.path.display()
            );
        }
        return Ok(());
    }

    if args.stale {
        let staging_root = store.staging_root();
        let removed = if fs::symlink_metadata(store.root()).is_ok()
            && fs::symlink_metadata(&staging_root).is_ok()
        {
            store.ensure_staging_root()?;
            cleanup_stale_staging_dirs(&staging_root)?
        } else {
            Vec::new()
        };
        let out = CleanStaleOutput {
            staging_root,
            removed,
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else if out.removed.is_empty() {
            println!(
                "No stale staging directories found in {}",
                out.staging_root.display()
            );
        } else {
            println!(
                "Removed {} stale staging director{} in {}",
                out.removed.len(),
                if out.removed.len() == 1 { "y" } else { "ies" },
                out.staging_root.display()
            );
        }
        return Ok(());
    }

    if args.list {
        let disks = store.list_staged_disks()?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&disks)?);
        } else if disks.is_empty() {
            println!("No staged disks in {}", store.staging_root().display());
        } else {
            println!(
                "{:<24} {:<14} {:<64} PATH",
                "STAGING_NAME", "SIZE_BYTES", "SHA256"
            );
            for disk in &disks {
                if let Some(err) = &disk.error {
                    println!("{:<24} ERROR: {err}", disk.staging_name);
                } else {
                    println!(
                        "{:<24} {:<14} {:<64} {}",
                        disk.staging_name,
                        disk.size_bytes
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                        disk.sha256.as_deref().unwrap_or("-"),
                        disk.path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "-".to_owned()),
                    );
                }
            }
        }
        return Ok(());
    }

    bail!("one of --staging-name, --stale, or --list must be specified");
}
