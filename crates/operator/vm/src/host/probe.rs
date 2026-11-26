//! Read-only local command probes and systemd unit property queries.

use std::process::{Command, Output};

/// Run `program args...` and return its stdout when it exits with success.
pub(super) fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    successful_stdout(Command::new(program).args(args).output().ok()?)
}

/// Run `program args...` directly as root, or through `sudo -n` otherwise, and return its stdout
/// when it exits with success.
pub(super) fn run_privileged_capture(program: &str, args: &[&str]) -> Option<String> {
    let out = if rustix::process::geteuid().as_raw() == 0 {
        Command::new(program).args(args).output().ok()?
    } else {
        Command::new("sudo")
            .arg("-n")
            .arg(program)
            .args(args)
            .output()
            .ok()?
    };
    successful_stdout(out)
}

fn successful_stdout(out: Output) -> Option<String> {
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Default)]
pub(super) struct SystemdUnitProps {
    pub(super) load_state: String,
    pub(super) active_state: String,
    pub(super) unit_file_state: String,
    pub(super) main_pid: Option<u32>,
    pub(super) exec_start: String,
    pub(super) exec_reload: String,
}

impl SystemdUnitProps {
    pub(super) fn is_loaded(&self) -> bool {
        self.load_state == "loaded"
    }

    pub(super) fn is_enabled(&self) -> bool {
        matches!(
            self.unit_file_state.as_str(),
            "enabled" | "enabled-runtime" | "static"
        )
    }

    pub(super) fn is_active(&self) -> bool {
        self.active_state == "active"
    }
}

pub(super) fn query_systemd_unit(unit: &str) -> SystemdUnitProps {
    let Some(raw) = run_capture(
        "systemctl",
        &[
            "show",
            "-p",
            "LoadState,ActiveState,UnitFileState,MainPID,ExecStart,ExecReload",
            unit,
        ],
    ) else {
        return SystemdUnitProps::default();
    };
    parse_systemd_show_props(&raw)
}

pub(super) fn parse_systemd_show_props(raw: &str) -> SystemdUnitProps {
    let mut props = SystemdUnitProps::default();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let val = v.trim();
        match k.trim() {
            "LoadState" => props.load_state = val.to_owned(),
            "ActiveState" => props.active_state = val.to_owned(),
            "UnitFileState" => props.unit_file_state = val.to_owned(),
            "MainPID" => {
                props.main_pid = val.parse::<u32>().ok().filter(|&pid| pid > 0);
            }
            "ExecStart" => props.exec_start = val.to_owned(),
            "ExecReload" => props.exec_reload = val.to_owned(),
            _ => {}
        }
    }
    props
}

/// Extract command tokens from a systemd `ExecStart=` value (handling both structured
/// `{ path=... ; argv[]=... ; ... }` output from `systemctl show` and raw command lines).
pub(crate) fn extract_systemd_exec_argv(exec_start: &str) -> Vec<String> {
    let trimmed = exec_start.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let cmd_str = if let Some(pos) = trimmed.find("argv[]=") {
        let rest = &trimmed[pos + "argv[]=".len()..];
        rest.split(" ; ").next().unwrap_or(rest).trim()
    } else {
        trimmed
    };
    cmd_str.split_whitespace().map(str::to_owned).collect()
}
