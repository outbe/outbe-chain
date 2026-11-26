//! Local single-VM data disk preparation (`outbe-up disk prepare`).

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
        prepare_joining_validator_tree_cancellable,
        prepare_joining_validator_tree_from_reader_cancellable, FounderKeyVerify, StagedDiskTree,
    },
    progress::Progress,
    NodeRole,
};
use outbe_operator_vm::{
    disk::{build_disk_image_cancellable, sha256_sparse_file},
    local::{validate_vm_name, LocalVmStore},
};
use serde::Serialize;
use tokio::signal::unix::{signal, SignalKind};

use super::{open_store, plural_directory};

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

enum PrepareTarget {
    Output(PathBuf),
    Staging {
        store: LocalVmStore,
        staging_name: String,
    },
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

fn validate_prepare_args(args: &PrepareArgs) -> Result<()> {
    if let Some(device) = &args.device {
        // TODO(S3-probe): Support raw block devices after the S3 hardware probe confirms
        // dstack VMM attachment and guest mount semantics (owner decision R-3 in
        // /home/ubuntu/tdx/15-design-outbe-up-split.md §§6 & 7/S3).
        bail!(
            "block device target ({}) is not yet supported, see S3 probe (owner decision R-3)",
            device.display()
        );
    }
    ensure!(args.size_gib > 0, "disk size must be positive");
    match args.role {
        NodeRole::Founder => validate_founder_args(args),
        NodeRole::JoiningValidator => validate_joining_validator_args(args),
        NodeRole::FullNode => validate_full_node_args(args),
    }
}

fn validate_founder_args(args: &PrepareArgs) -> Result<()> {
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
    Ok(())
}

fn validate_joining_validator_args(args: &PrepareArgs) -> Result<()> {
    ensure!(
        !args.skip_key_verify,
        "--skip-key-verify is only valid for --role founder"
    );
    ensure!(
        args.bundle.is_some(),
        "--bundle is required for --role joining-validator"
    );
    let Some(source) = args.source.as_deref() else {
        ensure!(
            args.config.is_some(),
            "--config (or --source -) is required for --role joining-validator"
        );
        return Ok(());
    };
    ensure!(
        source == Path::new("-"),
        "joining-validator --source only supports `-` (tar stream on stdin); pass --config for local config-based preparation"
    );
    ensure!(
        args.config.is_none() && args.keys_from.is_none(),
        "cannot combine --source - with --config or --keys-from"
    );
    Ok(())
}

fn validate_full_node_args(args: &PrepareArgs) -> Result<()> {
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
    Ok(())
}

fn resolve_prepare_target(args: &PrepareArgs) -> Result<(PrepareTarget, PathBuf)> {
    match (args.output.clone(), args.staging_name.clone()) {
        (Some(output), None) => {
            ensure_output_absent(&output)?;
            let parent = output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            fs::create_dir_all(&parent)?;
            Ok((PrepareTarget::Output(output), parent))
        }
        (None, Some(staging_name)) => {
            validate_vm_name(&staging_name)?;
            let store = open_store(args.state_dir.as_deref(), args.fix_permissions)?;
            let staging_root = store.ensure_staging_root()?;
            Ok((
                PrepareTarget::Staging {
                    store,
                    staging_name,
                },
                staging_root,
            ))
        }
        (Some(_), Some(_)) => Err(eyre::eyre!("cannot combine --output with --staging-name")),
        (None, None) => Err(eyre::eyre!(
            "one of --output, --staging-name, or --device must be specified"
        )),
    }
}

fn spawn_cancel_watcher() -> Result<(Arc<AtomicBool>, tokio::task::JoinHandle<()>)> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;
    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let cancel_signal = Arc::clone(&cancel);
    let signal_task = tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => { cancel_signal.store(true, Ordering::SeqCst); }
            _ = sigterm.recv() => { cancel_signal.store(true, Ordering::SeqCst); }
        }
    });
    Ok((cancel, signal_task))
}

fn stage_role_tree(
    args: &PrepareArgs,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    match args.role {
        NodeRole::Founder => stage_founder_tree(args, staging_parent, cancel),
        NodeRole::JoiningValidator => stage_joining_validator_tree(args, staging_parent, cancel),
        NodeRole::FullNode => {
            let bundle = args
                .bundle
                .as_deref()
                .expect("validated above: full-node requires --bundle");
            let config = args
                .config
                .as_deref()
                .expect("validated above: full-node requires --config");
            prepare_full_node_tree_cancellable(bundle, config, None, staging_parent, cancel)
        }
    }
}

fn stage_founder_tree(
    args: &PrepareArgs,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
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
        prepare_founder_tree_from_reader_cancellable(io::stdin(), staging_parent, verify, cancel)
    } else {
        prepare_founder_tree_cancellable(source, staging_parent, verify, cancel)
    }
}

fn stage_joining_validator_tree(
    args: &PrepareArgs,
    staging_parent: &Path,
    cancel: &AtomicBool,
) -> Result<StagedDiskTree> {
    let bundle = args
        .bundle
        .as_deref()
        .expect("validated above: joining-validator requires --bundle");
    if args.source.as_deref() == Some(Path::new("-")) {
        return prepare_joining_validator_tree_from_reader_cancellable(
            io::stdin(),
            staging_parent,
            FounderKeyVerify::Bundle(bundle),
            cancel,
        );
    }
    let config = args
        .config
        .as_deref()
        .expect("validated above: joining-validator requires --config");
    prepare_joining_validator_tree_cancellable(
        bundle,
        config,
        args.keys_from.as_deref(),
        staging_parent,
        cancel,
    )
}

fn cleanup_leftover_tmp_publish_dirs(staging_dir: &Path, current_uid: u32) {
    let Ok(entries) = fs::read_dir(staging_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_tmp = entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with(".data.img.tmp."));
        if !is_tmp {
            continue;
        }
        let path = entry.path();
        if fs::symlink_metadata(&path)
            .is_ok_and(|meta| meta.file_type().is_dir() && meta.uid() == current_uid)
        {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

fn publish_staged_disk(
    store: &LocalVmStore,
    staging_name: String,
    tree_path: &Path,
    disk_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(PathBuf, Option<String>)> {
    let staging_dir = store.ensure_staging_dir(&staging_name)?;
    let final_path = store.staged_disk_path(&staging_name)?;
    let current_uid = rustix::process::getuid().as_raw();
    if let Ok(meta) = fs::symlink_metadata(&final_path) {
        ensure!(
            meta.file_type().is_file(),
            "refusing to overwrite non-regular staged disk path: {}",
            final_path.display()
        );
        ensure!(
            meta.uid() == current_uid,
            "staged disk {} is owned by uid {}, expected current uid {current_uid}",
            final_path.display(),
            meta.uid()
        );
    }
    // Remove leftover `.data.img.tmp.*` dirs from a prior crashed prepare (safe under lock).
    cleanup_leftover_tmp_publish_dirs(&staging_dir, current_uid);

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
    build_disk_image_cancellable(tree_path, &tmp_img, disk_bytes, cancel)?;
    fs::rename(&tmp_img, &final_path).wrap_err_with(|| {
        format!(
            "atomically publish staged disk image to {}",
            final_path.display()
        )
    })?;
    if let Ok(dir_fd) = fs::File::open(&staging_dir) {
        let _ = dir_fd.sync_all();
    }
    Ok((final_path, Some(staging_name)))
}

fn run_prepare_blocking(
    args: PrepareArgs,
    target: PrepareTarget,
    staging_parent: PathBuf,
    cancel: &AtomicBool,
) -> Result<PrepareOutput> {
    let mut progress = (!args.json).then(Progress::new);
    let _staging_lock = match &target {
        PrepareTarget::Output(_) => None,
        PrepareTarget::Staging {
            store,
            staging_name,
        } => Some(store.lock_staging(staging_name)?),
    };

    if let Some(p) = &mut progress {
        p.stage(&format!("staging and validating {} data tree", args.role));
    }
    let staged = stage_role_tree(&args, &staging_parent, cancel)?;

    if let Some(p) = &mut progress {
        p.stage(&format!(
            "building {} GiB raw GPT+ext4 data disk for {}",
            args.size_gib, args.role
        ));
    }
    let disk_bytes = u64::from(args.size_gib) * GIB;
    let (final_output, staging_name_out) = match target {
        PrepareTarget::Output(output_path) => {
            build_disk_image_cancellable(staged.tree_path(), &output_path, disk_bytes, cancel)?;
            (output_path, None)
        }
        PrepareTarget::Staging {
            store,
            staging_name,
        } => publish_staged_disk(&store, staging_name, staged.tree_path(), disk_bytes, cancel)?,
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
        size_gib: args.size_gib,
        sha256,
        validator_address,
        hyperlane_address,
        key_verified,
        keygen_source,
    })
}

fn print_prepare_summary(summary: &PrepareOutput) {
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

pub async fn prepare(args: PrepareArgs) -> Result<()> {
    validate_prepare_args(&args)?;
    let (target, staging_parent) = resolve_prepare_target(&args)?;

    if args.cleanup_stale {
        let removed = cleanup_stale_staging_dirs(&staging_parent)?;
        if !args.json && !removed.is_empty() {
            eprintln!(
                "removed {} stale staging director{} in {}",
                removed.len(),
                plural_directory(removed.len()),
                staging_parent.display()
            );
        }
    }

    let (cancel, signal_task) = spawn_cancel_watcher()?;
    let cancel_worker = Arc::clone(&cancel);
    let json = args.json;
    let work_result = tokio::task::spawn_blocking(move || {
        run_prepare_blocking(args, target, staging_parent, &cancel_worker)
    })
    .await
    .wrap_err("disk prepare worker panicked")?;

    signal_task.abort();
    let summary = work_result?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        print_prepare_summary(&summary);
    }
    Ok(())
}
