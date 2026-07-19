//! Node-state probes: datadir moves and node-log inspection used by the
//! recovery/promotion scenarios.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{bail, Result, WrapErr};

use crate::internal::shell::Sh;

use super::Localnet;

impl Localnet {
    /// Audit every scenario log after its assertions and before teardown. The
    /// unsupported-update flow may emit only its exact compatibility fatal;
    /// unrelated fatals and all panic/DKG-share/VRF/SGX/projection alarms fail
    /// the scenario even when its functional steps passed.
    pub fn audit_unexpected_logs(&self, allow_unsupported_update_fatal: bool) -> Result<()> {
        let mut logs = Vec::new();
        collect_logs(&self.cfg.dir, &mut logs)?;
        let mut findings = Vec::new();
        for path in logs {
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
            for (index, line) in content.lines().enumerate() {
                if unexpected_log_line(line, allow_unsupported_update_fatal) {
                    findings.push(format!("{}:{}: {}", path.display(), index + 1, line));
                    if findings.len() == 20 {
                        break;
                    }
                }
            }
        }
        if !findings.is_empty() {
            bail!(
                "unexpected fatal/alarm log records:\n{}",
                findings.join("\n")
            );
        }
        Ok(())
    }

    /// Relocate a datadir under the data dir (warm promotion reuses synced state).
    pub fn move_datadir(&self, from_rel: &str, to_rel: &str) -> Result<()> {
        let from = self.cfg.dir.join(from_rel);
        let to = self.cfg.dir.join(to_rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&from, &to) {
            Ok(()) => Ok(()),
            Err(_) if self.cfg.sudo => {
                Sh::new(&self.cfg).sudo_best_effort(
                    "mv",
                    &[&from.display().to_string(), &to.display().to_string()],
                );
                Ok(())
            }
            Err(e) => Err(e).wrap_err("move datadir"),
        }
    }

    fn node_log(&self, node: &str) -> String {
        let path = self.cfg.dir.join(node).join("node.log");
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Whether validator `index`'s owned node process has already exited.
    pub fn validator_exited(&mut self, index: usize) -> bool {
        match self.validators.get_mut(&index) {
            Some(guard) => guard.exited(),
            None => true,
        }
    }

    /// Whether validator `index`'s log contains `needle` (`e2e_joiner_log_has`).
    pub fn log_has(&self, index: usize, needle: &str) -> bool {
        self.node_log(&format!("validator-{index}"))
            .contains(needle)
    }

    /// Count of log LINES containing `needle` (matches shell `grep -c`).
    pub fn log_count(&self, index: usize, needle: &str) -> usize {
        self.node_log(&format!("validator-{index}"))
            .lines()
            .filter(|l| l.contains(needle))
            .count()
    }

    /// Whether validator `index`'s enclave log contains `needle`.
    pub fn enclave_log_has(&self, index: usize, needle: &str) -> bool {
        let path = self.cfg.validator_dir(index).join("enclave.log");
        fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
    }

    /// The `--consensus.keys-dir` for validator `index` (persisted-share restart).
    pub fn keys_dir(&self, index: usize) -> String {
        self.cfg
            .validator_dir(index)
            .join("keys")
            .display()
            .to_string()
    }

    /// Whether a durable DKG share file exists in validator `index`'s keys dir
    /// (`e2e_assert "DKG share persisted to keys-dir"`, s4:28-29).
    pub fn has_share_file(&self, index: usize) -> bool {
        let dir = self.cfg.validator_dir(index).join("keys");
        fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(Result::ok).any(|e| {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    (n.contains("dkg") && n.contains("share")) || n == "dkg_share.hex"
                })
            })
            .unwrap_or(false)
    }
}

fn collect_logs(dir: &Path, logs: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("scan {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_logs(&path, logs)?;
        } else if path.extension().is_some_and(|extension| extension == "log") {
            logs.push(path);
        }
    }
    Ok(())
}

fn unexpected_log_line(line: &str, allow_unsupported_update_fatal: bool) -> bool {
    let line = line.to_ascii_lowercase();
    if allow_unsupported_update_fatal && line.contains("cannot activate protocol version") {
        return false;
    }
    line.contains("fatal")
        || line.contains("panic")
        || (line.contains("dkg")
            && line.contains("share")
            && (line.contains("reveal") || line.contains("disclos")))
        || (line.contains("vrf") && line.contains("alarm"))
        || line.contains("eagain")
        || line.contains("resource temporarily unavailable")
        || (line.contains("projection") && line.contains("fatal"))
}

#[cfg(test)]
mod tests {
    use super::unexpected_log_line;

    #[test]
    fn classifies_forbidden_operational_records() {
        for line in [
            "thread panicked at actor.rs",
            "fatal consensus supervisor exit",
            "DKG share reveal detected",
            "VRF alarm: proof mismatch",
            "EAGAIN while entering enclave",
            "Resource temporarily unavailable",
            "projection-fatal checkpoint error",
        ] {
            assert!(unexpected_log_line(line, false), "missed {line}");
        }
        assert!(!unexpected_log_line("committee finalized height=10", false));
    }

    #[test]
    fn allows_only_the_expected_binary_compatibility_fatal() {
        let expected = "fatal: cannot activate protocol version v3.0: binary supports v2.2";
        assert!(unexpected_log_line(expected, false));
        assert!(!unexpected_log_line(expected, true));
        assert!(unexpected_log_line("fatal: projection exited", true));
    }
}
