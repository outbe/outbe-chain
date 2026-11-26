//! Remote shell commands and output parsing for the legacy SSH VMM transport.

use eyre::{bail, Context, Result};

use super::{http::serial_logs_target, validate_vm_id};
use crate::shell_quote;

/// Build the remote shell `curl --unix-socket` command for one VMM pRPC call (legacy SSH transport).
pub fn build_prpc_curl_command(vmm_home: &str, method: &str) -> String {
    let sock_expr = remote_vmm_path(vmm_home, "run/vmm.sock");
    format!(
        "curl -sS --connect-timeout 5 --max-time 60 --unix-socket {sock_expr} -H 'Content-Type: application/json' -w '\\n%{{http_code}}' -X POST --data-binary @- {}",
        shell_quote(&format!("http://localhost/prpc/{method}?json"))
    )
}

/// Build the remote shell command to read the serial log tail safely (legacy SSH transport).
pub fn build_serial_log_command(vmm_home: &str, vm_id: &str, lines: usize) -> Result<String> {
    validate_vm_id(vm_id)?;
    let log_expr = remote_vmm_path(vmm_home, &format!("vm/{vm_id}/serial.log"));
    let sock_expr = remote_vmm_path(vmm_home, "run/vmm.sock");
    let url = format!("http://localhost{}", serial_logs_target(vm_id, lines));
    let url_quoted = shell_quote(&url);
    Ok(format!(
        "if [ -f {log_expr} ]; then tail -n {lines} {log_expr}; else curl -fsS --unix-socket {sock_expr} {url_quoted} 2>/dev/null || true; fi"
    ))
}

/// Build a shell expression for `<vmm_home>/<relative>`, expanding a leading `~/`.
pub fn remote_vmm_path(vmm_home: &str, relative: &str) -> String {
    let trimmed_home = vmm_home.trim_end_matches('/');
    let trimmed_rel = relative.trim_start_matches('/');
    let joined = if trimmed_rel.is_empty() {
        trimmed_home.to_owned()
    } else {
        format!("{trimmed_home}/{trimmed_rel}")
    };
    if let Some(rest) = joined.strip_prefix("~/") {
        format!("\"$HOME\"/{}", shell_quote(rest))
    } else if joined == "~" {
        "\"$HOME\"".to_owned()
    } else {
        shell_quote(&joined)
    }
}

pub fn parse_curl_http_response<'a>(method: &str, raw: &'a str) -> Result<&'a str> {
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    let (body, status_str) = match trimmed.rsplit_once('\n') {
        Some((body, code)) => (body, code.trim()),
        None => ("", trimmed.trim()),
    };
    let status: u16 = status_str
        .parse()
        .wrap_err_with(|| format!("VMM pRPC {method} returned non-HTTP output: {trimmed}"))?;
    if !(200..300).contains(&status) {
        bail!("VMM pRPC {method} returned HTTP {status}: {}", body.trim());
    }
    Ok(body)
}
