//! Pinned remote `outbe-up` installation, host queries, staging helpers, and founder tar streaming.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_remote::{run_host_json, shell_quote, HostExec, Ssh};
use outbe_software_bundle::{sha256_file, verify_and_extract, verify_archive};
use walkdir::WalkDir;

use super::{
    shell_quote_tilde_path,
    types::{RemoteStagedDiskInfo, RemoteVersionOutput, RemoteVmStatusOutput},
};
use crate::network::vm_state::NetworkVmState;

#[derive(Clone, Debug)]
pub(crate) struct RemoteOutbeUp {
    bin_expr: String,
}

impl RemoteOutbeUp {
    pub(crate) fn cmd(&self, subcmd: &str) -> String {
        format!("{} {subcmd}", self.bin_expr)
    }
}

pub(crate) fn resolve_bundled_outbe_up_sha256(artifacts: &Path) -> Result<String> {
    resolve_bundled_outbe_up_sha256_from_bundle(&artifacts.join("software-bundle.tar.zst"))
}

/// Prefer the `outbe_up_sha256` recorded in `network-vm-state.json` (e.g. after `network update
/// --software-bundle` pinned a newer `outbe-up` on hosts), falling back to the artifacts' bundled
/// `software-bundle.tar.zst`.
pub(crate) fn resolve_state_or_bundled_outbe_up_sha256(
    artifacts: &Path,
    state: Option<&NetworkVmState>,
) -> Result<String> {
    if let Some(sha) = state
        .map(|st| st.outbe_up_sha256.as_str())
        .filter(|s| !s.is_empty())
    {
        return Ok(sha.to_owned());
    }
    resolve_bundled_outbe_up_sha256(artifacts)
}

pub(crate) fn resolve_bundled_outbe_up_sha256_from_bundle(bundle_path: &Path) -> Result<String> {
    let manifest = verify_archive(bundle_path).wrap_err_with(|| {
        format!(
            "verify software bundle manifest at {}",
            bundle_path.display()
        )
    })?;
    let entry = manifest
        .files
        .iter()
        .find(|f| f.path == "bin/outbe-up")
        .ok_or_else(|| {
            eyre::eyre!(
                "software bundle {} does not contain `bin/outbe-up`",
                bundle_path.display()
            )
        })?;
    Ok(entry.sha256.clone())
}

fn extract_bundled_outbe_up_bytes_from_bundle(
    bundle_path: &Path,
    expected_sha256: &str,
) -> Result<Vec<u8>> {
    let tmp = tempfile::Builder::new()
        .prefix("outbe-up-extract-")
        .tempdir()
        .wrap_err("create temp dir for outbe-up extraction")?;
    verify_and_extract(bundle_path, tmp.path())?;
    let bin_path = tmp.path().join("bin/outbe-up");
    let actual_sha = sha256_file(&bin_path)?;
    ensure!(
        actual_sha == expected_sha256,
        "extracted bin/outbe-up digest mismatch: expected {expected_sha256}, got {actual_sha}"
    );
    fs::read(&bin_path).wrap_err("read extracted bin/outbe-up")
}

fn remote_outbe_up_paths(sha256: &str) -> (String, String, RemoteOutbeUp) {
    let sha16 = &sha256[..sha256.len().min(16)];
    let dir_expr = "\"${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up/bin\"".to_owned();
    let bin_expr =
        format!("\"${{XDG_STATE_HOME:-$HOME/.local/state}}/outbe-up/bin/outbe-up-{sha16}\"");
    (dir_expr, bin_expr.clone(), RemoteOutbeUp { bin_expr })
}

pub(crate) fn ensure_pinned_outbe_up_on_host(
    exec: &dyn HostExec,
    ssh: &Ssh,
    artifacts: &Path,
    expected_sha256: &str,
    allow_install: bool,
) -> Result<RemoteOutbeUp> {
    ensure_pinned_outbe_up_on_host_from_bundle(
        exec,
        ssh,
        &artifacts.join("software-bundle.tar.zst"),
        expected_sha256,
        allow_install,
    )
}

pub(crate) fn ensure_pinned_outbe_up_on_host_from_bundle(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bundle_path: &Path,
    expected_sha256: &str,
    allow_install: bool,
) -> Result<RemoteOutbeUp> {
    let (dir_expr, bin_expr, handle) = remote_outbe_up_paths(expected_sha256);
    let version_cmd = handle.cmd("version --json");

    // An existing binary under our 0700 state directory (`<state>/outbe-up/bin/outbe-up-<sha16>`)
    // is verified via its own `version --json` (`binary_sha256`) rather than an extra host
    // `sha256sum` invocation; this is acceptable under our threat model (the host owner already
    // controls the host).
    if let Ok(ver) =
        run_host_json::<dyn HostExec, RemoteVersionOutput>(exec, ssh, &ssh.host, &version_cmd)
    {
        if ver.name == "outbe-up" && ver.binary_sha256 == expected_sha256 {
            return Ok(handle);
        }
        if !allow_install {
            bail!(
                "pinned outbe-up on {} reported binary_sha256 `{}`, expected `{expected_sha256}`",
                ssh.host,
                ver.binary_sha256
            );
        }
    } else if !allow_install {
        bail!(
            "pinned outbe-up ({}) is missing on host {}; run `outbe-net-up network deploy` first",
            &expected_sha256[..16],
            ssh.host
        );
    }

    // Install pinned binary into `<state>/outbe-up/bin/outbe-up-<sha16>` without touching `$PATH`.
    let bytes = extract_bundled_outbe_up_bytes_from_bundle(bundle_path, expected_sha256)?;
    let sha16 = &expected_sha256[..16];
    let install_cmd = format!(
        "set -eu; umask 077; dir={dir_expr}; state_dir=\"$(dirname \"$dir\")\"; mkdir -p \"$dir\"; chmod 0700 \"$state_dir\" \"$dir\"; tmp=\"$dir/.outbe-up-{sha16}.tmp.$$\"; cat > \"$tmp\"; chmod 0700 \"$tmp\"; mv -f \"$tmp\" {bin_expr}"
    );
    exec.capture_stdin(ssh, &ssh.host, &install_cmd, &bytes)
        .wrap_err_with(|| format!("install pinned outbe-up-{sha16} on {}", ssh.host))?;

    let ver: RemoteVersionOutput = run_host_json(exec, ssh, &ssh.host, &version_cmd)
        .wrap_err_with(|| {
            format!(
                "verify installed outbe-up-{sha16} via `version --json` on {}",
                ssh.host
            )
        })?;
    ensure!(
        ver.name == "outbe-up" && ver.binary_sha256 == expected_sha256,
        "installed outbe-up on {} failed SHA-256 verification: got name=`{}` sha256=`{}`, expected `outbe-up` / `{expected_sha256}` (version={})",
        ssh.host,
        ver.name,
        ver.binary_sha256,
        ver.version
    );
    Ok(handle)
}

pub(crate) fn fetch_host_vms(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
    vmm_home: &str,
) -> Result<Vec<RemoteVmStatusOutput>> {
    let cmd = bin.cmd(&format!(
        "vm status --vmm-home {} --json",
        shell_quote_tilde_path(vmm_home)
    ));
    run_host_json(exec, ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("query `outbe-up vm status --json` on {}", ssh.host))
}

pub(crate) fn fetch_host_staged_disks(
    exec: &dyn HostExec,
    ssh: &Ssh,
    bin: &RemoteOutbeUp,
) -> Result<Vec<RemoteStagedDiskInfo>> {
    let cmd = bin.cmd("disk clean --list --json");
    run_host_json(exec, ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("list staged disks on {}", ssh.host))
}

pub(crate) fn make_remote_temp_dir(exec: &dyn HostExec, ssh: &Ssh, prefix: &str) -> Result<String> {
    let cmd = format!(
        "d=$(mktemp -d /tmp/{prefix}.XXXXXX) && chmod 0700 \"$d\" && printf '%s\\n' \"$d\""
    );
    let path = exec
        .capture(ssh, &ssh.host, &cmd)
        .wrap_err_with(|| format!("create remote staging directory on {}", ssh.host))?;
    let expected_prefix = format!("/tmp/{prefix}.");
    ensure!(
        path.starts_with(&expected_prefix)
            && path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_')),
        "remote host {} returned unsafe staging path `{path}`",
        ssh.host
    );
    Ok(path)
}

pub(crate) fn cleanup_remote_dir(exec: &dyn HostExec, ssh: &Ssh, remote_dir: &str) -> Result<()> {
    ensure!(
        remote_dir.starts_with("/tmp/outbe-net-up-"),
        "refusing to remove non-staging remote path `{remote_dir}`"
    );
    exec.run_quiet(
        ssh,
        &ssh.host,
        &format!("rm -rf -- {}", shell_quote(remote_dir)),
    )
}

/// Build an in-memory deterministic POSIX tar archive of a founder data tree for streaming over
/// stdin to `outbe-up disk prepare --role founder --source - --staging-name <vm>`.
pub(crate) fn build_founder_tar_bytes(founder_tree: &Path) -> Result<Vec<u8>> {
    ensure!(
        founder_tree.is_dir(),
        "founder data tree does not exist: {}",
        founder_tree.display()
    );
    let mut paths = Vec::new();
    for entry in WalkDir::new(founder_tree).follow_links(false) {
        let entry = entry?;
        if entry.path() != founder_tree {
            paths.push(entry.into_path());
        }
    }
    paths.sort();

    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        builder.follow_symlinks(false);
        for path in paths {
            let rel = path.strip_prefix(founder_tree)?;
            append_founder_tar_entry(&mut builder, &path, rel)?;
        }
        builder.finish()?;
    }
    Ok(out)
}

fn append_founder_tar_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &Path,
    rel: &Path,
) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    ensure!(
        !ft.is_symlink(),
        "refusing symlink in founder data tree: {}",
        path.display()
    );
    let mode = meta.permissions().mode() & 0o777;
    let mut header = tar::Header::new_gnu();
    header.set_mode(mode);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    if ft.is_dir() {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, rel, std::io::empty())?;
        return Ok(());
    }
    if ft.is_file() {
        ensure!(
            mode & 0o111 == 0,
            "refusing executable file in founder data tree: {}",
            path.display()
        );
        let data = fs::read(path)?;
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, rel, data.as_slice())?;
        return Ok(());
    }
    bail!(
        "refusing non-regular entry in founder data tree: {}",
        path.display()
    );
}
