//! Local staged disk listing and cleanup (`outbe-up disk clean`).

use std::{fs, path::PathBuf};

use eyre::{bail, Result};
use outbe_operator_node_role::disk_tree::cleanup_stale_staging_dirs;
use outbe_operator_vm::local::LocalVmStore;
use serde::Serialize;

use super::{open_store, plural_directory};

pub struct CleanArgs {
    pub staging_name: Option<String>,
    pub stale: bool,
    pub list: bool,
    pub state_dir: Option<PathBuf>,
    pub fix_permissions: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
struct CleanStaleOutput {
    staging_root: PathBuf,
    removed: Vec<PathBuf>,
}

pub fn clean(args: CleanArgs) -> Result<()> {
    let store = open_store(args.state_dir.as_deref(), args.fix_permissions)?;
    if let Some(staging_name) = args.staging_name {
        return clean_named_staging(&store, &staging_name, args.json);
    }
    if args.stale {
        return clean_stale_staging(&store, args.json);
    }
    if args.list {
        return list_staged_disks(&store, args.json);
    }
    bail!("one of --staging-name, --stale, or --list must be specified");
}

fn clean_named_staging(store: &LocalVmStore, staging_name: &str, json: bool) -> Result<()> {
    let out = store.clean_staging_dir(staging_name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if out.removed {
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
    Ok(())
}

fn clean_stale_staging(store: &LocalVmStore, json: bool) -> Result<()> {
    let staging_root = store.staging_root();
    let both_exist =
        fs::symlink_metadata(store.root()).is_ok() && fs::symlink_metadata(&staging_root).is_ok();
    let removed = if both_exist {
        store.ensure_staging_root()?;
        cleanup_stale_staging_dirs(&staging_root)?
    } else {
        Vec::new()
    };
    let out = CleanStaleOutput {
        staging_root,
        removed,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    if out.removed.is_empty() {
        println!(
            "No stale staging directories found in {}",
            out.staging_root.display()
        );
    } else {
        println!(
            "Removed {} stale staging director{} in {}",
            out.removed.len(),
            plural_directory(out.removed.len()),
            out.staging_root.display()
        );
    }
    Ok(())
}

fn list_staged_disks(store: &LocalVmStore, json: bool) -> Result<()> {
    let disks = store.list_staged_disks()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&disks)?);
        return Ok(());
    }
    if disks.is_empty() {
        println!("No staged disks in {}", store.staging_root().display());
        return Ok(());
    }
    println!(
        "{:<24} {:<14} {:<64} PATH",
        "STAGING_NAME", "SIZE_BYTES", "SHA256"
    );
    for disk in &disks {
        if let Some(err) = &disk.error {
            println!("{:<24} ERROR: {err}", disk.staging_name);
            continue;
        }
        let size = disk
            .size_bytes
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let path = disk
            .path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{:<24} {:<14} {:<64} {}",
            disk.staging_name,
            size,
            disk.sha256.as_deref().unwrap_or("-"),
            path,
        );
    }
    Ok(())
}
