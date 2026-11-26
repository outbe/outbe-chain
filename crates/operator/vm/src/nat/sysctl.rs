//! Persistent `net.ipv4.ip_forward` resolution from the preflight sysctl scan.

use std::{collections::BTreeMap, path::Path};

use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PersistentSysctlState {
    pub effective_value: Option<u8>,
    pub effective_source: Option<String>,
    /// True when `effective_value == Some(0)` and `effective_source` is ordered after
    /// `90-outbe-up-nat.conf` (e.g. `99-*.conf` or `/etc/sysctl.conf`), so writing our
    /// 90- drop-in would still be overridden back to 0 on reboot.
    pub overridden_by_later_zero: bool,
}

/// Compute the effective persistent `net.ipv4.ip_forward` value following procps `sysctl --system`
/// and `systemd-sysctl` precedence:
/// 1. Whole-file same-basename masking across `/etc/sysctl.d` > `/run/sysctl.d` >
///    `/usr/local/lib/sysctl.d` > `/usr/lib/sysctl.d` > `/lib/sysctl.d` (a higher-priority file
///    without `net.ipv4.ip_forward` or symlinked to `/dev/null` hides lower-priority files of
///    the same basename).
/// 2. Winning basenames are applied in lexicographical order.
/// 3. `/etc/sysctl.conf` is applied last (matching procps-ng `sysctl --system` on Ubuntu).
pub(crate) fn parse_persistent_sysctl(
    section: &str,
    our_dropin_basename: &str,
) -> PersistentSysctlState {
    SysctlScan::parse(section).effective_state(our_dropin_basename)
}

/// The `FILE`, `CONF`, and `SET` records of one preflight sysctl scan.
#[derive(Default)]
struct SysctlScan {
    /// Map basename -> (dir_idx, full_path) where smaller dir_idx = higher priority.
    winner_by_basename: BTreeMap<String, (usize, String)>,
    sysctl_conf_path: Option<String>,
    assignments_by_file: BTreeMap<String, u8>,
}

/// The last persistent assignment that wins, and whether it applies after our drop-in.
struct EffectiveAssignment {
    value: u8,
    source: String,
    after_dropin: bool,
}

impl SysctlScan {
    fn parse(section: &str) -> Self {
        let mut scan = Self::default();
        for line in section.lines().map(str::trim).filter(|l| !l.is_empty()) {
            scan.record_line(line);
        }
        scan
    }

    fn record_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("FILE ") {
            if let Some((dir_idx, path)) = parse_file_record(rest) {
                self.record_file(dir_idx, path);
            }
        } else if let Some(path) = line.strip_prefix("CONF ") {
            self.sysctl_conf_path = Some(path.trim().to_owned());
        } else if let Some((path, value)) = line.strip_prefix("SET ").and_then(parse_set_record) {
            self.assignments_by_file.insert(path.to_owned(), value);
        }
    }

    /// Keep the file from the highest-priority directory for each basename.
    fn record_file(&mut self, dir_idx: usize, path: &str) {
        let Some(basename) = Path::new(path).file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let already_won = self
            .winner_by_basename
            .get(basename)
            .is_some_and(|(existing_idx, _)| *existing_idx <= dir_idx);
        if !already_won {
            self.winner_by_basename
                .insert(basename.to_owned(), (dir_idx, path.to_owned()));
        }
    }

    fn effective_state(&self, our_dropin_basename: &str) -> PersistentSysctlState {
        let effective = self
            .sysctl_conf_assignment()
            .or_else(|| self.last_dropin_assignment(our_dropin_basename));
        let overridden_by_later_zero = effective
            .as_ref()
            .is_some_and(|e| e.value == 0 && e.after_dropin);
        PersistentSysctlState {
            effective_value: effective.as_ref().map(|e| e.value),
            effective_source: effective.map(|e| e.source),
            overridden_by_later_zero,
        }
    }

    /// The winning drop-in files apply in basename order, so the last assignment wins.
    fn last_dropin_assignment(&self, our_dropin_basename: &str) -> Option<EffectiveAssignment> {
        self.winner_by_basename
            .iter()
            .rev()
            .find_map(|(basename, (_idx, winning_path))| {
                let value = *self.assignments_by_file.get(winning_path)?;
                Some(EffectiveAssignment {
                    value,
                    source: winning_path.clone(),
                    after_dropin: basename.as_str() > our_dropin_basename,
                })
            })
    }

    /// `/etc/sysctl.conf` applies after every drop-in.
    fn sysctl_conf_assignment(&self) -> Option<EffectiveAssignment> {
        let conf_path = self.sysctl_conf_path.as_ref()?;
        let value = *self.assignments_by_file.get(conf_path)?;
        Some(EffectiveAssignment {
            value,
            source: conf_path.clone(),
            after_dropin: true,
        })
    }
}

/// Parse `<dir_idx> <path>` from a `FILE` record.
fn parse_file_record(rest: &str) -> Option<(usize, &str)> {
    let (idx_str, path) = rest.split_once(' ')?;
    let dir_idx = idx_str.parse::<usize>().ok()?;
    Some((dir_idx, path))
}

/// Parse `<path>:<key> = <0|1>` from a `SET` record. Other values are ignored.
fn parse_set_record(rest: &str) -> Option<(&str, u8)> {
    let (path, rhs) = rest.split_once(':')?;
    let (_lhs, val_part) = rhs.split_once('=')?;
    let val_token = val_part.split(['#', ';']).next().unwrap_or("").trim();
    let value = val_token.parse::<u8>().ok()?;
    (value <= 1).then_some((path, value))
}
