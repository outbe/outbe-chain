//! Read-only `outbe-up host check` command builder, runner, parser, and failure formatter.

use std::collections::{BTreeMap, BTreeSet};

use eyre::{Context, Result};
use outbe_operator_remote::{shell_quote, HostExec};

use super::{
    host_key,
    remote_bin::RemoteOutbeUp,
    shell_quote_tilde_path,
    types::{DesiredMember, RemoteHostCheckOutput},
};
use crate::network::{config::FounderPublishedPorts, vm_state::MemberRole};

pub(super) const HOST_CHECK_EXIT_SENTINEL: &str = "__OUTBE_EXIT__=";

pub(crate) struct PublishContext {
    multi_host: bool,
    peer_sources: Vec<String>,
}

impl PublishContext {
    pub(crate) fn from_members(members: &[DesiredMember]) -> Self {
        let distinct_hosts: BTreeSet<&str> =
            members.iter().map(|m| m.vmm_host.host.as_str()).collect();
        let peer_sources: Vec<String> = members
            .iter()
            .filter(|m| m.role != MemberRole::FullNode)
            .map(|m| format!("{}/32", m.advertise_ip))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self {
            multi_host: distinct_hosts.len() > 1,
            peer_sources,
        }
    }

    fn requires_published_ports(&self, member: &DesiredMember) -> bool {
        if member.role == MemberRole::FullNode {
            return false;
        }
        self.multi_host || member.port_offset > 0 || member.advertise_ip != member.service_ip
    }

    pub(crate) fn p2p_port_specs(&self, member: &DesiredMember) -> Result<Vec<String>> {
        if !self.requires_published_ports(member) {
            return Ok(Vec::new());
        }
        let ports = FounderPublishedPorts::from_offset(member.port_offset)?;
        Ok(ports
            .dnat_ports()
            .into_iter()
            .filter(|(_, _, _, purpose)| *purpose != "hyperlane")
            .map(|(public_port, _vm_port, proto, _purpose)| format!("{public_port}/{proto}"))
            .collect())
    }

    pub(crate) fn hyperlane_port_specs(&self, member: &DesiredMember) -> Result<Vec<String>> {
        if !self.requires_published_ports(member) {
            return Ok(Vec::new());
        }
        Ok(FounderPublishedPorts::from_offset(member.port_offset)?
            .publish_port_specs()
            .into_iter()
            .rev()
            .take(1)
            .collect())
    }
}

pub(crate) fn build_host_check_command(
    member: &DesiredMember,
    bin: &RemoteOutbeUp,
    ctx: &PublishContext,
) -> Result<String> {
    build_host_check_command_for(member, bin, &ctx.p2p_port_specs(member)?, &ctx.peer_sources)
}

pub(crate) fn build_hyperlane_host_check_command(
    member: &DesiredMember,
    bin: &RemoteOutbeUp,
    ctx: &PublishContext,
) -> Result<Option<String>> {
    let ports = ctx.hyperlane_port_specs(member)?;
    if ports.is_empty() {
        return Ok(None);
    }
    Ok(Some(build_host_check_command_for(
        member,
        bin,
        &ports,
        &[],
    )?))
}

fn build_host_check_command_for(
    member: &DesiredMember,
    bin: &RemoteOutbeUp,
    ports: &[String],
    sources: &[String],
) -> Result<String> {
    let mut sub = format!(
        "host check --vm {} --ip {} --bridge {} --vmm-home {}",
        shell_quote(&member.vm_name),
        member.service_ip,
        shell_quote(&member.bridge),
        shell_quote_tilde_path(&member.vmm_home),
    );
    if !ports.is_empty() {
        sub.push_str(&format!(" --public-ip {}", member.advertise_ip));
        for port in ports {
            sub.push_str(&format!(" --port {}", shell_quote(port)));
        }
        for src in sources {
            sub.push_str(&format!(" --source {}", shell_quote(src)));
        }
    }
    sub.push_str(" --json");
    let inner = bin.cmd(&sub);
    Ok(format!(
        "rc=0; out=$({inner}) || rc=$?; if [ -z \"$out\" ] && [ \"$rc\" -ne 0 ]; then exit \"$rc\"; fi; printf '%s\\n{HOST_CHECK_EXIT_SENTINEL}%d\\n' \"$out\" \"$rc\""
    ))
}

pub(crate) fn run_member_host_checks(
    exec: &dyn HostExec,
    member: &DesiredMember,
    bin: &RemoteOutbeUp,
    ctx: &PublishContext,
) -> Result<RemoteHostCheckOutput> {
    let command = build_host_check_command(member, bin, ctx)?;
    let mut report = run_host_check_command(exec, member, &command)?;
    if let Some(command) = build_hyperlane_host_check_command(member, bin, ctx)? {
        let hyperlane = run_host_check_command(exec, member, &command)?;
        merge_host_check(&mut report, hyperlane);
    }
    Ok(report)
}

fn run_host_check_command(
    exec: &dyn HostExec,
    member: &DesiredMember,
    command: &str,
) -> Result<RemoteHostCheckOutput> {
    let raw = exec
        .capture_raw(&member.vmm_host, &member.name, command)
        .wrap_err_with(|| {
            format!(
                "run `outbe-up host check` for `{}` on {}",
                member.vm_name, member.vmm_host.host
            )
        })?;
    let (mut report, exit_code) = parse_host_check_output(&raw, &member.vm_name)?;
    if exit_code != 0
        || !report.service_ip_ok
        || !report.host_network_ok
        || !report.published_ports_ok
    {
        report.ready = false;
    }
    Ok(report)
}

fn merge_host_check(report: &mut RemoteHostCheckOutput, extra: RemoteHostCheckOutput) {
    report.ready &= extra.ready;
    report.host_network_ok &= extra.host_network_ok;
    report.service_ip_ok &= extra.service_ip_ok;
    report.published_ports_ok &= extra.published_ports_ok;
    report.missing.extend(extra.missing);
    report.conflicts.extend(extra.conflicts);
    report
        .recommended_forward_rules
        .extend(extra.recommended_forward_rules);
    report
        .recommended_prerouting_rules
        .extend(extra.recommended_prerouting_rules);
    report
        .recommended_firewall_commands
        .extend(extra.recommended_firewall_commands);
    dedup_strings(&mut report.missing);
    dedup_strings(&mut report.conflicts);
    dedup_strings(&mut report.recommended_forward_rules);
    dedup_strings(&mut report.recommended_prerouting_rules);
    dedup_strings(&mut report.recommended_firewall_commands);
}

fn dedup_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

pub(crate) fn run_host_checks(
    members: &[DesiredMember],
    host_bins: &BTreeMap<String, RemoteOutbeUp>,
    exec: &dyn HostExec,
    ctx: &PublishContext,
) -> Result<Vec<(String, RemoteHostCheckOutput)>> {
    let mut results = Vec::with_capacity(members.len());
    for member in members {
        let hkey = host_key(&member.vmm_host, &member.vmm_home);
        let bin = &host_bins[&hkey];
        let report = run_member_host_checks(exec, member, bin, ctx)?;
        results.push((member.vmm_host.host.clone(), report));
    }
    Ok(results)
}

pub(crate) fn parse_host_check_output(
    raw: &str,
    label: &str,
) -> Result<(RemoteHostCheckOutput, i32)> {
    let trimmed = raw.trim();
    if let Some(pos) = trimmed.rfind(HOST_CHECK_EXIT_SENTINEL) {
        let json_part = trimmed[..pos].trim();
        let rc_str = trimmed[pos + HOST_CHECK_EXIT_SENTINEL.len()..].trim();
        let rc: i32 = rc_str.parse().wrap_err_with(|| {
            format!(
                "invalid exit code sentinel from `outbe-up host check` on `{label}`: `{rc_str}`"
            )
        })?;
        let report: RemoteHostCheckOutput = serde_json::from_str(json_part)
            .wrap_err_with(|| format!("parse `outbe-up host check --json` output for `{label}`"))?;
        Ok((report, rc))
    } else {
        let report: RemoteHostCheckOutput = serde_json::from_str(trimmed)
            .wrap_err_with(|| format!("parse `outbe-up host check --json` output for `{label}`"))?;
        let rc = if report.ready { 0 } else { 1 };
        Ok((report, rc))
    }
}

fn append_indented_section(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("  {heading}:\n"));
    for item in items {
        out.push_str(&format!("    {item}\n"));
    }
}

pub(crate) fn format_failed_host_checks(checks: &[(String, RemoteHostCheckOutput)]) -> String {
    let mut out = String::new();
    for (host, check) in checks.iter().filter(|(_, c)| !c.ready) {
        out.push_str(&format!(
            "\nHost {host} / VM `{}` (host_network_ok={}, service_ip_ok={}, published_ports_ok={}):\n",
            check.vm, check.host_network_ok, check.service_ip_ok, check.published_ports_ok
        ));
        for conflict in &check.conflicts {
            out.push_str(&format!("  conflict: {conflict}\n"));
        }
        for missing in &check.missing {
            out.push_str(&format!("  missing:  {missing}\n"));
        }
        append_indented_section(
            &mut out,
            "Recommended forward rules",
            &check.recommended_forward_rules,
        );
        append_indented_section(
            &mut out,
            "Recommended prerouting rules",
            &check.recommended_prerouting_rules,
        );
        append_indented_section(
            &mut out,
            "Apply firewall changes",
            &check.recommended_firewall_commands,
        );
    }
    out.trim_end().to_owned()
}
