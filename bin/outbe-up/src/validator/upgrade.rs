use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_systemd::{founding_units, FoundingConfig};
use outbe_software_bundle::{sha256_file, verify_and_extract, SoftwareManifest};

use crate::support::{process, progress::Progress};

use super::{assets, install, journal::Journal, maintenance, sync, VALIDATOR_TARGET};

const REQUIRED_BINARIES: &[&str] = &[
    "outbe-up",
    "outbe-chain",
    "outbe-cli",
    "outbe-keygen",
    "outbe-feeder",
    "outbe-radicle",
    "outbe-hyperlane-validator",
];

pub async fn run(software_bundle: &Path, skip_dkg_guard: bool) -> Result<()> {
    let _lock = process::LifecycleLock::acquire()?;
    let journal = Journal::load()?;
    let was_active = process::systemctl_is_active(VALIDATOR_TARGET);
    if was_active {
        maintenance::local_guard(&journal, skip_dkg_guard).await?;
    }
    let enabled_on_boot = process::services_enabled();
    let bundle = software_bundle
        .canonicalize()
        .wrap_err("software bundle does not exist")?;
    let staging = tempfile::tempdir().wrap_err("create upgrade staging directory")?;
    let manifest = verify_and_extract(&bundle, staging.path())?;
    validate_manifest(&manifest)?;
    let new_outbe_up = staging.path().join("bin/outbe-up");

    if was_active {
        process::systemctl(&["stop", VALIDATOR_TARGET])?;
        ensure!(
            !process::systemctl_is_active(VALIDATOR_TARGET),
            "validator target did not stop before upgrade"
        );
    }

    let apply = process::run(
        &new_outbe_up,
        &[
            "internal",
            "apply-software-upgrade",
            "--software-bundle",
            path_text(&bundle)?,
        ],
    );
    if let Err(error) = apply {
        bail!("software installation failed; validator remains stopped: {error:#}");
    }

    if was_active {
        let updated = Journal::load()?;
        let mut progress = Progress::new();
        if let Err(error) = sync::start_and_wait(&updated, &mut progress, enabled_on_boot).await {
            let _ = process::systemctl(&["stop", VALIDATOR_TARGET]);
            bail!("upgraded validator failed health checks and was stopped: {error:#}");
        }
    }
    println!(
        "validator software upgraded to {} ({})",
        manifest.release_tag, manifest.commit
    );
    Ok(())
}

/// Apply a previously verified software bundle while services are stopped.
/// This is an internal entry point intentionally executed from the new outbe-up binary.
pub fn apply(software_bundle: &Path, founder_index: Option<usize>) -> Result<()> {
    process::require_root()?;
    ensure!(
        !process::systemctl_is_active(VALIDATOR_TARGET),
        "refusing to replace binaries while {VALIDATOR_TARGET} is active"
    );
    let root = Path::new(process::ROOT);
    let staging = tempfile::Builder::new()
        .prefix(".upgrade-")
        .tempdir_in(root)
        .wrap_err("create same-filesystem upgrade staging directory")?;
    let manifest = verify_and_extract(software_bundle, staging.path())?;
    validate_manifest(&manifest)?;
    let bundle_sha256 = sha256_file(software_bundle)?;

    let installed = install_binaries(root, staging.path(), &manifest)?;
    let units = match founder_index {
        Some(index) => founding_units(FoundingConfig {
            base_dir: process::ROOT,
            validator_index: index,
        })
        .into_iter()
        .map(|unit| (unit.name, unit.content))
        .collect(),
        None => assets::systemd_units()?,
    };
    if let Err(error) = install::install_units(root, units) {
        restore_binaries(root, &installed)?;
        return Err(error).wrap_err("install upgraded systemd units");
    }
    process::systemctl(&["daemon-reload"])?;

    if founder_index.is_none() {
        if let Err(error) = atomic_copy(
            &root.join("outbe-up"),
            Path::new("/usr/local/bin/outbe-up"),
            0o755,
        ) {
            restore_binaries(root, &installed)?;
            return Err(error).wrap_err("install upgraded outbe-up command");
        }
        let mut journal = Journal::load()?;
        journal.software_bundle_sha256 = Some(bundle_sha256);
        journal.software_commit = Some(manifest.commit.clone());
        journal.software_release_tag = Some(manifest.release_tag.clone());
        journal.save()?;
    }
    println!(
        "installed software release {} ({})",
        manifest.release_tag, manifest.commit
    );
    Ok(())
}

pub(crate) fn validate_manifest(manifest: &SoftwareManifest) -> Result<()> {
    ensure!(
        manifest.target == "x86_64-unknown-linux-gnu",
        "unsupported software target: {}",
        manifest.target
    );
    let names = bundle_binaries(manifest)?;
    for required in REQUIRED_BINARIES {
        ensure!(
            names.contains(*required),
            "software bundle is missing bin/{required}"
        );
    }
    Ok(())
}

fn bundle_binaries(manifest: &SoftwareManifest) -> Result<BTreeSet<&str>> {
    let mut names = BTreeSet::new();
    for file in &manifest.files {
        let path = Path::new(&file.path);
        let Ok(relative) = path.strip_prefix("bin") else {
            continue;
        };
        ensure!(
            relative.components().count() == 1,
            "nested binary path is not supported: {}",
            file.path
        );
        let name = relative
            .to_str()
            .ok_or_else(|| eyre::eyre!("non-UTF-8 binary path"))?;
        ensure!(names.insert(name), "duplicate software binary: {name}");
    }
    Ok(names)
}

struct InstalledBinaries {
    names: Vec<String>,
    previously_present: BTreeSet<String>,
}

fn install_binaries(
    root: &Path,
    extracted: &Path,
    manifest: &SoftwareManifest,
) -> Result<InstalledBinaries> {
    let names = bundle_binaries(manifest)?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let modes = manifest
        .files
        .iter()
        .filter_map(|file| file.path.strip_prefix("bin/").map(|name| (name, file.mode)))
        .collect::<BTreeMap<_, _>>();
    let backup_tmp = root.join(format!(".previous.tmp.{}", std::process::id()));
    remove_path(&backup_tmp)?;
    fs::create_dir(&backup_tmp)?;
    fs::set_permissions(&backup_tmp, fs::Permissions::from_mode(0o700))?;
    process::chown(&backup_tmp, 0, 0)?;

    let mut previously_present = BTreeSet::new();
    let mut prepared = Vec::new();
    for name in &names {
        let source = extracted.join("bin").join(name);
        let destination = root.join(name);
        let metadata = fs::symlink_metadata(&source)
            .wrap_err_with(|| format!("missing staged binary: {name}"))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "invalid staged binary: {name}"
        );
        if let Ok(current) = fs::symlink_metadata(&destination) {
            ensure!(
                current.is_file() && !current.file_type().is_symlink(),
                "refusing non-regular installed binary: {}",
                destination.display()
            );
            fs::copy(&destination, backup_tmp.join(name))?;
            fs::set_permissions(backup_tmp.join(name), fs::Permissions::from_mode(0o755))?;
            previously_present.insert(name.clone());
        }
        let temporary = root.join(format!(".{name}.upgrade.{}", std::process::id()));
        if temporary.exists() {
            fs::remove_file(&temporary)?;
        }
        fs::copy(&source, &temporary)?;
        fs::set_permissions(
            &temporary,
            fs::Permissions::from_mode(*modes.get(name.as_str()).unwrap_or(&0o755)),
        )?;
        process::chown(&temporary, 0, 0)?;
        prepared.push((name.clone(), temporary, destination));
    }

    publish_backup(root, &backup_tmp)?;
    let mut replaced = Vec::new();
    for (name, temporary, destination) in prepared {
        if let Err(error) = fs::rename(&temporary, &destination) {
            let installed = InstalledBinaries {
                names: replaced,
                previously_present: previously_present.clone(),
            };
            restore_binaries(root, &installed)?;
            return Err(error).wrap_err_with(|| format!("replace binary {name}"));
        }
        replaced.push(name);
    }
    Ok(InstalledBinaries {
        names,
        previously_present,
    })
}

fn publish_backup(root: &Path, backup_tmp: &Path) -> Result<()> {
    let previous = root.join(".previous");
    let old = root.join(format!(".previous.old.{}", std::process::id()));
    remove_path(&old)?;
    if previous.exists() {
        fs::rename(&previous, &old)?;
    }
    if let Err(error) = fs::rename(backup_tmp, &previous) {
        if old.exists() {
            let _ = fs::rename(&old, &previous);
        }
        return Err(error).wrap_err("publish previous software backup");
    }
    remove_path(&old)?;
    Ok(())
}

fn restore_binaries(root: &Path, installed: &InstalledBinaries) -> Result<()> {
    let previous = root.join(".previous");
    for name in &installed.names {
        let destination = root.join(name);
        if installed.previously_present.contains(name) {
            atomic_copy(&previous.join(name), &destination, 0o755)?;
        } else if destination.exists() {
            fs::remove_file(destination)?;
        }
    }
    Ok(())
}

fn atomic_copy(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| eyre::eyre!("destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let name = destination
        .file_name()
        .ok_or_else(|| eyre::eyre!("destination has no file name"))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.upgrade.{}", std::process::id()));
    if temporary.exists() {
        fs::remove_file(&temporary)?;
    }
    fs::copy(source, &temporary)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    process::chown(&temporary, 0, 0)?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)?
        }
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| eyre::eyre!("path is not UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_software_bundle::SoftwareFile;

    fn manifest(files: &[&str]) -> SoftwareManifest {
        SoftwareManifest {
            version: 1,
            commit: "a".repeat(40),
            release_tag: "test".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            files: files
                .iter()
                .map(|path| SoftwareFile {
                    path: (*path).into(),
                    size: 1,
                    sha256: "b".repeat(64),
                    mode: 0o755,
                })
                .collect(),
        }
    }

    #[test]
    fn upgrade_requires_every_runtime_binary() {
        let complete = REQUIRED_BINARIES
            .iter()
            .map(|name| format!("bin/{name}"))
            .collect::<Vec<_>>();
        let refs = complete.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(validate_manifest(&manifest(&refs)).is_ok());
        assert!(validate_manifest(&manifest(&refs[1..])).is_err());
    }
}
