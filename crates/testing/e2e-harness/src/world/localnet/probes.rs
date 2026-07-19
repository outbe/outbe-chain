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
    pub fn audit_unexpected_logs(&self, unsupported_version: Option<u64>) -> Result<()> {
        let mut logs = Vec::new();
        collect_logs(&self.cfg.dir, &mut logs)?;
        let mut findings = Vec::new();
        let expected_fragment = unsupported_version.map(|version| {
            format!(
                "cannot activate protocol version v{}.{} ({version}): binary supports at most v",
                version >> 24,
                version & 0x00ff_ffff
            )
        });
        let mut expected_by_validator = vec![0_usize; self.cfg.validators];
        for path in logs {
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
            for (index, line) in content.lines().enumerate() {
                if expected_fragment
                    .as_deref()
                    .is_some_and(|fragment| exact_expected_update_fatal(line, fragment))
                {
                    if let Some(validator) = validator_node_log_index(&path, self.cfg.validators) {
                        expected_by_validator[validator] += 1;
                        continue;
                    }
                }
                if unexpected_log_line(line) {
                    findings.push(format!("{}:{}: {}", path.display(), index + 1, line));
                    if findings.len() == 20 {
                        break;
                    }
                }
            }
        }
        if expected_fragment.is_some() {
            for (validator, count) in expected_by_validator.into_iter().enumerate() {
                if count == 0 {
                    findings.push(format!(
                        "validator-{validator}/node.log: expected unsupported-version fatal is absent"
                    ));
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
        } else if is_runtime_log(&path) {
            logs.push(path);
        }
    }
    Ok(())
}

fn is_runtime_log(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("node.log" | "enclave.log" | "reth.log")
    )
}

fn validator_node_log_index(path: &Path, validators: usize) -> Option<usize> {
    if path.file_name()?.to_str()? != "node.log" {
        return None;
    }
    let validator = path.parent()?.file_name()?.to_str()?;
    let index = validator.strip_prefix("validator-")?.parse().ok()?;
    (index < validators).then_some(index)
}

fn exact_expected_update_fatal(line: &str, expected_fragment: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("fatal").count() == 1
        && line.matches("cannot activate protocol version").count() == 1
        && line.contains(expected_fragment)
        && !contains_nonfatal_alarm(&line)
}

fn contains_nonfatal_alarm(line: &str) -> bool {
    line.contains("panic")
        || (line.contains("dkg")
            && line.contains("share")
            && (line.contains("reveal") || line.contains("disclos")))
        || (line.contains("vrf") && line.contains("alarm"))
        || line.contains("eagain")
        || line.contains("resource temporarily unavailable")
        || (line.contains("projection") && line.contains("fatal"))
}

fn unexpected_log_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("fatal") || contains_nonfatal_alarm(&line)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{exact_expected_update_fatal, is_runtime_log, unexpected_log_line};

    #[test]
    fn audits_runtime_logs_without_reading_binary_database_logs() {
        for path in [
            "validator-0/node.log",
            "validator-0/enclave.log",
            "validator-0/logs/54322345/reth.log",
        ] {
            assert!(is_runtime_log(Path::new(path)), "missed {path}");
        }
        assert!(!is_runtime_log(Path::new(
            "validator-0/data/rocksdb/000011.log"
        )));
    }

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
            assert!(unexpected_log_line(line), "missed {line}");
        }
        assert!(!unexpected_log_line("committee finalized height=10"));
    }

    #[test]
    fn allows_only_the_expected_binary_compatibility_fatal() {
        let fragment =
            "cannot activate protocol version v3.0 (50331648): binary supports at most v";
        let expected = "fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v2.3 (33554435)";
        assert!(exact_expected_update_fatal(expected, fragment));
        assert!(!exact_expected_update_fatal(
            &format!("{expected}; fatal: projection exited"),
            fragment
        ));
        assert!(!exact_expected_update_fatal(
            "fatal: cannot activate protocol version v4.0 (67108864): binary supports at most v2.3 (33554435)",
            fragment
        ));
        assert!(unexpected_log_line("fatal: projection exited"));
    }
}
