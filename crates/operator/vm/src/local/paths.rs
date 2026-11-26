//! Default local paths (state dir, VMM home, VMM socket) and `~` expansion.

use std::path::{Path, PathBuf};

use eyre::{ensure, Result};

/// Expand a leading `~` or `~/` using `$HOME`.
pub fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is not set"))?;
    ensure!(!home.is_empty(), "HOME is empty");
    Ok(PathBuf::from(home))
}

/// Default local state directory: `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up`.
///
/// An empty or relative `XDG_STATE_HOME` is ignored.
pub fn default_state_dir() -> Result<PathBuf> {
    let xdg_state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    match xdg_state_home {
        Some(xdg_path) => Ok(xdg_path.join("outbe-up")),
        None => Ok(home_dir()?.join(".local/state/outbe-up")),
    }
}

/// Default local dstack VMM home directory (`~/.dstack-vmm`).
pub fn default_vmm_home() -> Result<PathBuf> {
    Ok(home_dir()?.join(".dstack-vmm"))
}

/// Default local dstack VMM Unix socket path (`~/.dstack-vmm/run/vmm.sock`).
pub fn default_vmm_socket() -> Result<PathBuf> {
    Ok(default_vmm_home()?.join("run/vmm.sock"))
}

/// Resolve `(vmm_socket, vmm_home)` from optional CLI overrides.
pub fn resolve_vmm_paths(
    vmm_socket: Option<&Path>,
    vmm_home: Option<&Path>,
) -> Result<(PathBuf, PathBuf)> {
    match (vmm_socket, vmm_home) {
        (Some(sock), Some(home)) => Ok((expand_tilde(sock)?, expand_tilde(home)?)),
        (None, Some(home)) => {
            let home = expand_tilde(home)?;
            Ok((home.join("run/vmm.sock"), home))
        }
        (Some(sock), None) => {
            let sock = expand_tilde(sock)?;
            let derived_home = vmm_home_from_socket(&sock)?;
            Ok((sock, derived_home))
        }
        (None, None) => Ok((default_vmm_socket()?, default_vmm_home()?)),
    }
}

/// Derive the VMM home from its socket path: the parent of a `run/` directory, else the socket's
/// own directory.
fn vmm_home_from_socket(sock: &Path) -> Result<PathBuf> {
    let parent = sock
        .parent()
        .ok_or_else(|| eyre::eyre!("VMM socket path has no parent: {}", sock.display()))?;
    if parent.file_name().and_then(|s| s.to_str()) == Some("run") {
        return Ok(parent.parent().unwrap_or(parent).to_path_buf());
    }
    Ok(parent.to_path_buf())
}
