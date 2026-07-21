//! Node-state probes: datadir moves and node-log inspection used by the
//! recovery/promotion scenarios.

use std::fs;
use std::path::{Path, PathBuf};

use eyre::{bail, Result, WrapErr};
use serde_json::{json, Value};

use crate::internal::proc::{first_hex, run_capture};
use crate::internal::shell::Sh;

use super::Localnet;

impl Localnet {
    /// Audit every scenario log after its assertions and before teardown. The
    /// unsupported-update flow may emit only its exact compatibility fatal;
    /// unrelated fatals and all panic/DKG-share/VRF/SGX/projection alarms fail
    /// the scenario even when its functional steps passed.
    pub fn audit_unexpected_logs(
        &self,
        unsupported_version: Option<u64>,
        expected_dkg_reveal: Option<&str>,
    ) -> Result<LogAudit> {
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
        let mut expected_reveal_by_validator = vec![0_usize; self.cfg.validators];
        let mut counts = LogCounts {
            runtime_log_files: logs.len(),
            ..LogCounts::default()
        };
        for path in logs {
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
            for (index, line) in content.lines().enumerate() {
                if expected_fragment.as_deref().is_some_and(|fragment| {
                    accept_expected_update_fatal(&path, line, fragment, &mut expected_by_validator)
                }) {
                    counts.expected_update_fatal += 1;
                    continue;
                }
                if expected_dkg_reveal.is_some_and(|public_key| {
                    accept_expected_dkg_reveal(
                        &path,
                        line,
                        public_key,
                        &mut expected_reveal_by_validator,
                    )
                }) {
                    counts.expected_dkg_reveal += 1;
                    continue;
                }
                counts.observe(line);
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
        if expected_dkg_reveal.is_some() {
            for (validator, count) in expected_reveal_by_validator.into_iter().enumerate() {
                if count == 0 {
                    findings.push(format!(
                        "validator-{validator}/node.log: expected exact DKG share reveal is absent"
                    ));
                }
            }
        }
        Ok(LogAudit { counts, findings })
    }

    pub(crate) fn scenario_id(&self) -> usize {
        self.cfg.scenario
    }

    pub(crate) fn scenario_dir(&self) -> &Path {
        &self.cfg.dir
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

    /// Consensus BLS public key derived from this validator's provisioned
    /// signing key, in the exact lowercase hex form used by DKG reveal alarms.
    pub fn consensus_public_key(&self, index: usize) -> Result<String> {
        let signing_key = self.cfg.validator_dir(index).join("signing-key.hex");
        let output = run_capture(
            &self.cfg.bin_keygen,
            &["show-pubkey", "--key", &signing_key.display().to_string()],
        )?;
        first_hex(&output, 96).ok_or_else(|| eyre::eyre!("no BLS public key from keygen"))
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

#[derive(Debug, Default)]
struct LogCounts {
    runtime_log_files: usize,
    fatal: usize,
    panic: usize,
    dkg_share_reveal: usize,
    vrf_alarm: usize,
    sgx_resource_exhaustion: usize,
    projection_fatal: usize,
    expected_update_fatal: usize,
    expected_dkg_reveal: usize,
}

impl LogCounts {
    fn observe(&mut self, line: &str) {
        let line = line.to_ascii_lowercase();
        self.fatal += usize::from(line.contains("fatal"));
        self.panic += usize::from(line.contains("panic"));
        self.dkg_share_reveal += usize::from(
            line.contains("dkg")
                && line.contains("share")
                && (line.contains("reveal") || line.contains("disclos")),
        );
        self.vrf_alarm += usize::from(line.contains("vrf") && line.contains("alarm"));
        self.sgx_resource_exhaustion += usize::from(
            line.contains("eagain") || line.contains("resource temporarily unavailable"),
        );
        self.projection_fatal += usize::from(line.contains("projection") && line.contains("fatal"));
    }

    fn json(&self) -> Value {
        json!({
            "runtime_log_files": self.runtime_log_files,
            "fatal": self.fatal,
            "panic": self.panic,
            "dkg_share_reveal": self.dkg_share_reveal,
            "vrf_alarm": self.vrf_alarm,
            "sgx_resource_exhaustion": self.sgx_resource_exhaustion,
            "projection_fatal": self.projection_fatal,
            "expected_update_fatal": self.expected_update_fatal,
            "expected_dkg_reveal": self.expected_dkg_reveal,
        })
    }
}

/// Structured result retained as scenario evidence before logs are removed.
#[derive(Debug)]
pub struct LogAudit {
    counts: LogCounts,
    findings: Vec<String>,
}

impl LogAudit {
    pub(crate) fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "clean": self.is_clean(),
            "counts": self.counts.json(),
            "findings": self.findings,
        })
    }

    pub(crate) fn ensure_clean(&self) -> Result<()> {
        if !self.findings.is_empty() {
            bail!(
                "unexpected fatal/alarm log records:\n{}",
                self.findings.join("\n")
            );
        }
        Ok(())
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

fn accept_expected_update_fatal(
    path: &Path,
    line: &str,
    expected_fragment: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_update_fatal(line, expected_fragment) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn accept_expected_dkg_reveal(
    path: &Path,
    line: &str,
    expected_public_key: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_dkg_reveal(line, expected_public_key) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn exact_expected_dkg_reveal(line: &str, expected_public_key: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("dkg: a validator's individual share was revealed")
        .count()
        == 1
        // `node.log` keeps tracing's ANSI formatting around the `=` separator,
        // while `reth.log` is plain text. Match the field name and its exact
        // value independently so both representations prove the same reveal.
        && line.contains("revealed_validator")
        && line.contains(&expected_public_key.to_ascii_lowercase())
        && !line.contains("fatal")
        && !line.contains("panic")
        && !(line.contains("vrf") && line.contains("alarm"))
        && !line.contains("eagain")
        && !line.contains("resource temporarily unavailable")
        && !(line.contains("projection") && line.contains("fatal"))
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

    use super::{
        accept_expected_dkg_reveal, accept_expected_update_fatal, exact_expected_dkg_reveal,
        exact_expected_update_fatal, is_runtime_log, unexpected_log_line, LogCounts,
    };

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
    fn evidence_counts_are_explicit_and_category_specific() {
        let mut counts = LogCounts::default();
        counts.observe("fatal projection supervisor exit");
        counts.observe("VRF alarm: proof mismatch");
        let json = counts.json();
        assert_eq!(json["runtime_log_files"], 0);
        assert_eq!(json["fatal"], 1);
        assert_eq!(json["projection_fatal"], 1);
        assert_eq!(json["vrf_alarm"], 1);
        assert_eq!(json["panic"], 0);
        assert_eq!(json["dkg_share_reveal"], 0);
        assert_eq!(json["sgx_resource_exhaustion"], 0);
        assert_eq!(json["expected_update_fatal"], 0);
        assert_eq!(json["expected_dkg_reveal"], 0);
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

    #[test]
    fn allows_only_the_exact_expected_dkg_reveal() {
        let key = "91782b96da4ceae23d5adfa62ec55ef41827d43c8b624035972bc0a086f743266168a73e42e5b6c14c119dcf94d39588";
        let expected = format!(
            "WARN outbe::dkg: DKG: a validator's individual share was REVEALED (offline during the ceremony); rotate revealed_validator={key}"
        );
        assert!(exact_expected_dkg_reveal(&expected, key));
        assert!(exact_expected_dkg_reveal(
            &expected.replace(
                "revealed_validator=",
                "revealed_validator\u{1b}[0m\u{1b}[2m=\u{1b}[0m"
            ),
            key
        ));
        assert!(!exact_expected_dkg_reveal(
            &expected.replace(key, &"0".repeat(96)),
            key
        ));
        assert!(!exact_expected_dkg_reveal(
            &format!("fatal {expected}"),
            key
        ));

        let mut seen = [0_usize; 4];
        assert!(accept_expected_dkg_reveal(
            Path::new("scenario-1/validator-2/node.log"),
            &expected,
            key,
            &mut seen,
        ));
        assert_eq!(seen, [0, 0, 1, 0]);
    }

    #[test]
    fn allows_expected_update_fatal_in_reth_log_but_counts_node_log_evidence() {
        let fragment =
            "cannot activate protocol version v3.0 (50331648): binary supports at most v";
        let fatal = "payload builder error=fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v0.1 (1)";
        let mut counts = [0, 0];

        assert!(accept_expected_update_fatal(
            Path::new("validator-1/logs/54322345/reth.log"),
            fatal,
            fragment,
            &mut counts,
        ));
        assert_eq!(counts, [0, 0]);
        assert!(accept_expected_update_fatal(
            Path::new("validator-1/node.log"),
            fatal,
            fragment,
            &mut counts,
        ));
        assert_eq!(counts, [0, 1]);
    }
}
