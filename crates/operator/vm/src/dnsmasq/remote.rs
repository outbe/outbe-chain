//! Privileged host commands that read and compare-and-swap the dnsmasq hosts file.

use std::path::Path;

use eyre::{bail, Result};
use sha2::{Digest, Sha256};

use crate::shell_quote;

pub(crate) const PRESENT_SENTINEL: &str = "OUTBE_UP_PRESENT";
pub(crate) const MISSING_SENTINEL: &str = "OUTBE_UP_MISSING";

/// Build a privileged read command that distinguishes an absent file from a read failure.
pub fn build_privileged_read_command(path: &Path) -> String {
    let quoted = shell_quote(&path.to_string_lossy());
    let inner = format!(
        "if [ -e {quoted} ]; then printf '{PRESENT_SENTINEL}\\n' && cat {quoted}; else printf '{MISSING_SENTINEL}\\n'; fi"
    );
    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -ec {script}; else sudo -n sh -ec {script}; fi",
        script = shell_quote(&inner)
    )
}

/// Parse the output of a sentinel-framed read command, failing closed on unexpected output.
pub fn parse_sentinel_read(raw: &str, path: &Path) -> Result<Option<String>> {
    if let Some(rest) = raw.strip_prefix(PRESENT_SENTINEL) {
        let body = rest.strip_prefix('\n').unwrap_or(rest);
        return Ok(Some(body.to_owned()));
    }
    if raw.trim() == MISSING_SENTINEL {
        return Ok(None);
    }
    bail!(
        "unexpected remote read response for {} (refusing to assume empty)",
        path.display()
    );
}

/// Build a locked compare-and-swap command that updates `hosts_file` from stdin and sends SIGHUP to dnsmasq.
pub fn build_host_reserve_write_command(
    hosts_file: &Path,
    pid_file: &Path,
    lock_file: &Path,
    expected_sha256: Option<&str>,
) -> String {
    let hosts_quoted = shell_quote(&hosts_file.to_string_lossy());
    let lock_quoted = shell_quote(&lock_file.to_string_lossy());
    let lock_dir_quoted = shell_quote(
        &lock_file
            .parent()
            .unwrap_or_else(|| Path::new("/run/outbe-up"))
            .to_string_lossy(),
    );
    let tmp_template = shell_quote(&format!("{}.outbe-up.XXXXXX", hosts_file.display()));
    let pid_quoted = shell_quote(&pid_file.to_string_lossy());

    let cas_check = match expected_sha256 {
        Some(expected) => {
            let expected_quoted = shell_quote(expected);
            format!(
                "if [ ! -e {hosts_quoted} ]; then echo 'hosts file disappeared during update' >&2; exit 1; fi; \
                 actual=$(sha256sum {hosts_quoted} | awk '{{print $1}}'); \
                 if [ \"$actual\" != {expected_quoted} ]; then echo 'hosts file changed concurrently; retry' >&2; exit 1; fi"
            )
        }
        None => format!(
            "if [ -e {hosts_quoted} ]; then echo 'hosts file was created concurrently; retry' >&2; exit 1; fi"
        ),
    };

    let inner = format!(
        "uid=$(id -u); gid=$(id -g); \
         if [ -L {lock_dir_quoted} ]; then echo 'lock directory must not be a symlink' >&2; exit 1; fi; \
         if [ ! -e {lock_dir_quoted} ]; then install -d -m 0700 -o \"$uid\" -g \"$gid\" {lock_dir_quoted}; fi; \
         if [ ! -d {lock_dir_quoted} ] || [ -L {lock_dir_quoted} ]; then echo 'lock directory is not a directory' >&2; exit 1; fi; \
         if [ \"$(stat -c '%u %a' {lock_dir_quoted})\" != \"$uid 700\" ]; then echo 'lock directory has unsafe owner or permissions' >&2; exit 1; fi; \
         if [ -e {lock_quoted} ] || [ -L {lock_quoted} ]; then \
           if [ ! -f {lock_quoted} ] || [ -L {lock_quoted} ] || [ \"$(stat -c '%u' {lock_quoted})\" != \"$uid\" ]; then \
             echo 'lock entry must be a regular file owned by current uid' >&2; exit 1; \
           fi; \
         fi; \
         exec 9>>{lock_quoted}; flock -x 9; \
         {cas_check}; \
         tmp=$(mktemp {tmp_template}); \
         trap 'rm -f \"$tmp\"' EXIT; \
         cat > \"$tmp\"; \
         chmod 0644 \"$tmp\"; \
         mv -f \"$tmp\" {hosts_quoted}; \
         trap - EXIT; \
         if [ -f {pid_quoted} ]; then kill -HUP \"$(cat {pid_quoted})\"; else echo 'dnsmasq pid file missing: '{pid_quoted} >&2; exit 1; fi"
    );

    format!(
        "if [ \"$(id -u)\" -eq 0 ]; then sh -ec {script}; else sudo -n sh -ec {script}; fi",
        script = shell_quote(&inner)
    )
}

/// Compute lowercase hex SHA-256 for CAS checks on dnsmasq hosts content.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
