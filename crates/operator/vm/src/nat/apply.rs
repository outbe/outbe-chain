//! Apply and remove orchestration for host NAT on one target host.

use std::{net::Ipv4Addr, path::Path};

use eyre::{bail, ensure, Context, Result};

use super::{
    commands::{
        build_apply_command, build_ownership_probe_command, build_preflight_command,
        build_remove_command, render_nat_ruleset, render_systemd_unit,
    },
    preflight::{parse_owned_files_section, parse_preflight_output, PreflightReport},
    ApplyOutcome, FileOwnership, ForwardChainClass, HostNatPaths, HostRunner, HostTarget,
    ManagedFileStates, NftInspection, StageReporter, NAT_TABLE_FAMILY, NAT_TABLE_NAME,
    OWNERSHIP_MARKER,
};
use crate::shell_quote;

/// Operator options for one host NAT apply run.
#[derive(Clone, Copy, Debug)]
pub struct ApplyOptions<'a> {
    /// Use this uplink interface instead of the IPv4 default-route interface.
    pub uplink_override: Option<&'a str>,
    /// Print the plan and make no changes.
    pub dry_run: bool,
    /// Apply even when foreign egress or unverified forward chains exist.
    pub force: bool,
    /// Artifacts directory that the guest verification hint names.
    pub artifacts: &'a Path,
}

/// The rendered files and the remote command of one apply run.
struct ApplyPlan {
    ruleset: String,
    unit: String,
    command: String,
    write_sysctl_dropin: bool,
    enable_runtime_ip_forward: bool,
}

pub fn run_apply_on_host<R: HostRunner, P: StageReporter>(
    target: &HostTarget,
    runner: &mut R,
    paths: &HostNatPaths,
    options: &ApplyOptions<'_>,
    progress: &mut P,
) -> Result<ApplyOutcome> {
    let host = &target.host;
    progress.stage(&format!("running read-only NAT preflight on {host}"));
    let preflight_cmd = build_preflight_command(&target.bridge, options.uplink_override, paths)?;
    let raw = runner
        .run_raw(host, &preflight_cmd)
        .wrap_err_with(|| format!("run NAT preflight on {host}"))?;
    let report = parse_preflight_output(&raw, &target.bridge, &target.founder_ips)?;

    print_preflight_report(host, &report, &target.founder_ips);

    // 1. Existing full foreign egress detection: no-op unless --force.
    if egress_already_provided(&report) && !options.force {
        print_existing_egress_skip(host, &report.nft);
        return Ok(ApplyOutcome::NoOpExistingEgress);
    }

    // 1b. Partial foreign masquerade/SNAT (narrowed by extra matches): cannot decide statically.
    if !options.force {
        ensure_no_partial_egress(host, &report.nft)?;
    }

    // 2. Refuse foreign file collisions and later persistent sysctl=0 overrides before any mutation.
    let write_sysctl_dropin = check_managed_files(host, &report, paths)?;

    // 3. Refuse when any foreign forward base chain is classified blocks-or-unknown, unless --force.
    let has_blocks_or_unknown = !report.nft.blocks_or_unknown_chains.is_empty();
    if !options.force {
        ensure_forward_chains_verified(host, &report)?;
    }

    let plan = build_apply_plan(&report, paths, write_sysctl_dropin)?;
    if options.dry_run {
        print_dry_run_plan(host, &report, paths, &plan);
        return Ok(ApplyOutcome::DryRun);
    }

    apply_and_verify(host, runner, &plan, progress)?;
    if has_blocks_or_unknown {
        println!(
            "Warning: applied with --force while foreign forward chain(s) could not be verified \
             statically. Verify outbound connectivity from inside a guest VM."
        );
    }
    println!(
        "Note: the host cannot probe guest egress without guest SSH. Verify VM health with:\n  \
         outbe-up vm status --artifacts {}\n\
         To verify guest outbound connectivity on a dev VM, run an HTTPS/DNS probe from inside \
         a guest via the VMM host.",
        options.artifacts.display()
    );
    Ok(ApplyOutcome::Applied)
}

pub fn run_remove_on_host<R: HostRunner, P: StageReporter>(
    target: &HostTarget,
    runner: &mut R,
    paths: &HostNatPaths,
    dry_run: bool,
    progress: &mut P,
) -> Result<()> {
    let host = &target.host;
    let remove_cmd = build_remove_command(paths);
    if dry_run {
        let probe_cmd = build_ownership_probe_command(paths);
        let raw = runner
            .run_raw(host, &probe_cmd)
            .wrap_err_with(|| format!("probe managed file ownership on {host}"))?;
        let states = parse_owned_files_section(&raw)?;
        println!("\n--- Dry-run remove plan for {host} (no changes applied) ---");
        println!("  {}: {:?}", paths.rules_path.display(), states.rules);
        println!("  {}: {:?}", paths.service_path.display(), states.service);
        println!("  {}: {:?}", paths.sysctl_path.display(), states.sysctl);
        println!(
            "  runtime net.ipv4.ip_forward: left unchanged (remove never writes ip_forward=0)"
        );
        println!("Remote command:\n{remove_cmd}\n");
        return Ok(());
    }

    progress.stage(&format!("removing managed host NAT artifacts on {host}"));
    let output = runner
        .run_raw(host, &remove_cmd)
        .wrap_err_with(|| format!("remove host NAT on {host}"))?;
    let trimmed = output.trim();
    if !trimmed.is_empty() {
        println!("{trimmed}");
    }
    println!(
        "Note on {host}: runtime net.ipv4.ip_forward was left unchanged so other host workloads \
         are not disrupted."
    );
    Ok(())
}

/// True when forwarding is on and a foreign rule already masquerades all bridge egress.
fn egress_already_provided(report: &PreflightReport) -> bool {
    report.ip_forward == 1 && !report.nft.existing_egress.is_empty()
}

fn print_existing_egress_skip(host: &str, nft: &NftInspection) {
    for egress in &nft.existing_egress {
        println!(
            "egress already provided by {} {} {}",
            egress.family, egress.table, egress.chain
        );
    }
    println!(
        "Skipping apply on {host} (no changes made; pass --force to install `table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` anyway)."
    );
}

/// Refuse partial foreign egress when no full foreign egress exists.
fn ensure_no_partial_egress(host: &str, nft: &NftInspection) -> Result<()> {
    if !nft.existing_egress.is_empty() || nft.partial_egress.is_empty() {
        return Ok(());
    }
    let rules = nft
        .partial_egress
        .iter()
        .map(|p| {
            format!(
                "{} {}::{} ({}, narrowed by: {})",
                p.family,
                p.table,
                p.chain,
                p.kind,
                p.narrowing.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "refusing to apply NAT on {host}: partial foreign masquerade/snat rule(s) found \
         that cannot be decided statically as full egress coverage: {rules}. Inspect those \
         rules and re-run with --force to install `table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}`"
    );
}

/// Refuse writes that a foreign file or a later persistent `ip_forward=0` would defeat.
///
/// Returns true when the sysctl drop-in must be written.
fn check_managed_files(host: &str, report: &PreflightReport, paths: &HostNatPaths) -> Result<bool> {
    let sysctl = &report.persistent_sysctl;
    let write_sysctl_dropin = sysctl.effective_value != Some(1);
    if write_sysctl_dropin && sysctl.overridden_by_later_zero {
        let overriding = sysctl
            .effective_source
            .as_deref()
            .unwrap_or("unknown sysctl file");
        bail!(
            "cannot persist net.ipv4.ip_forward=1 on {host} via {}: `{overriding}` sets \
             net.ipv4.ip_forward=0 and takes precedence on reboot. Update or remove `{overriding}` \
             first",
            paths.sysctl_path.display()
        );
    }
    let foreign_collisions = foreign_collisions(&report.files, paths, write_sysctl_dropin);
    ensure!(
        foreign_collisions.is_empty(),
        "refusing to overwrite unmanaged file(s) on {host} (missing `{OWNERSHIP_MARKER}` marker): {}",
        foreign_collisions.join(", ")
    );
    Ok(write_sysctl_dropin)
}

/// List the paths that apply would write but that carry no ownership marker.
fn foreign_collisions(
    files: &ManagedFileStates,
    paths: &HostNatPaths,
    write_sysctl_dropin: bool,
) -> Vec<String> {
    [
        (files.rules, &paths.rules_path, true),
        (files.service, &paths.service_path, true),
        (files.sysctl, &paths.sysctl_path, write_sysctl_dropin),
    ]
    .into_iter()
    .filter(|(state, _, written)| *written && *state == FileOwnership::Foreign)
    .map(|(_, path, _)| path.display().to_string())
    .collect()
}

fn ensure_forward_chains_verified(host: &str, report: &PreflightReport) -> Result<()> {
    if report.nft.blocks_or_unknown_chains.is_empty() {
        return Ok(());
    }
    let chains = report
        .nft
        .blocks_or_unknown_chains
        .iter()
        .map(|c| {
            format!(
                "{} {}::{} (priority {}, policy {}, {})",
                c.family,
                c.table,
                c.chain,
                c.priority,
                c.policy,
                c.reasons.join("; ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "refusing to apply NAT on {host}: foreign forward base chain(s) cannot be verified \
         statically (nftables evaluates every forward base chain independently): {chains}. \
         `outbe-up vm host-nat` never edits foreign nftables tables. Inspect those chains and \
         re-run with --force only if they permit bridge `{}` forwarding, then verify egress \
         from a guest",
        report.bridge
    );
}

fn build_apply_plan(
    report: &PreflightReport,
    paths: &HostNatPaths,
    write_sysctl_dropin: bool,
) -> Result<ApplyPlan> {
    let ruleset = render_nat_ruleset(&report.bridge, report.subnet, &report.uplink)?;
    let unit = render_systemd_unit(&paths.rules_path);
    let enable_runtime_ip_forward = report.ip_forward == 0;
    let command = build_apply_command(
        &ruleset,
        &unit,
        write_sysctl_dropin,
        enable_runtime_ip_forward,
        paths,
    );
    Ok(ApplyPlan {
        ruleset,
        unit,
        command,
        write_sysctl_dropin,
        enable_runtime_ip_forward,
    })
}

fn print_dry_run_plan(
    host: &str,
    report: &PreflightReport,
    paths: &HostNatPaths,
    plan: &ApplyPlan,
) {
    println!("\n--- Dry-run plan for {host} (no changes applied) ---");
    println!(
        "Ruleset ({}):\n{}",
        paths.rules_path.display(),
        plan.ruleset
    );
    println!(
        "Systemd unit ({}):\n{}",
        paths.service_path.display(),
        plan.unit
    );
    if plan.write_sysctl_dropin {
        println!(
            "Sysctl drop-in ({}): will be written with `{OWNERSHIP_MARKER}` and `net.ipv4.ip_forward = 1` (runtime sysctl -w: {})",
            paths.sysctl_path.display(),
            plan.enable_runtime_ip_forward
        );
    } else {
        println!(
            "Sysctl drop-in ({}): not needed (persistent net.ipv4.ip_forward=1 already set by {})",
            paths.sysctl_path.display(),
            report
                .persistent_sysctl
                .effective_source
                .as_deref()
                .unwrap_or("existing sysctl config")
        );
    }
    println!("Remote command:\n{}\n", plan.command);
}

/// Run the apply command, then read back the live NAT table.
fn apply_and_verify<R: HostRunner, P: StageReporter>(
    host: &str,
    runner: &mut R,
    plan: &ApplyPlan,
    progress: &mut P,
) -> Result<()> {
    progress.stage(&format!(
        "applying atomic nftables table `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` on {host}"
    ));
    runner
        .run_raw(host, &plan.command)
        .wrap_err_with(|| format!("apply host NAT on {host}"))?;

    progress.stage(&format!(
        "verifying `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` on {host}"
    ));
    let verify_cmd = format!(
        "sudo -n nft list table {} {}",
        shell_quote(NAT_TABLE_FAMILY),
        shell_quote(NAT_TABLE_NAME)
    );
    let live_table = runner.run_raw(host, &verify_cmd).wrap_err_with(|| {
        format!("verify `{NAT_TABLE_FAMILY} {NAT_TABLE_NAME}` table on {host}")
    })?;
    println!(
        "\nVerified live table on {host}:\n{}",
        live_table.trim_end()
    );
    Ok(())
}

fn print_preflight_report(host: &str, report: &PreflightReport, founder_ips: &[Ipv4Addr]) {
    let ips = founder_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let persistent = &report.persistent_sysctl;
    println!("\nNAT preflight on {host}:");
    println!(
        "  bridge:            {} (host {}, subnet {})",
        report.bridge,
        report.bridge_addr,
        report.subnet.cidr_string()
    );
    println!("  founder IPs:       {ips} (all within bridge subnet)");
    println!("  uplink:            {}", report.uplink);
    println!(
        "  ip_forward:        runtime={}, persistent={} ({})",
        report.ip_forward,
        persistent
            .effective_value
            .map_or_else(|| "unset".to_owned(), |v| v.to_string()),
        persistent.effective_source.as_deref().unwrap_or("none")
    );
    print_egress_lines(&report.nft);
    print_forward_chain_lines(&report.nft);
    if report.nft.docker_or_libvirt_chains.is_empty() {
        println!("  docker/libvirt:    none detected");
    } else {
        println!(
            "  docker/libvirt:    {}",
            report.nft.docker_or_libvirt_chains.join(", ")
        );
    }
}

fn print_egress_lines(nft: &NftInspection) {
    if nft.existing_egress.is_empty() {
        println!("  existing egress:   none detected");
    }
    for egress in &nft.existing_egress {
        println!(
            "  existing egress:   {} {} {} ({})",
            egress.family, egress.table, egress.chain, egress.kind
        );
    }
    for partial in &nft.partial_egress {
        println!(
            "  partial egress:    {} {} {} ({}, narrowed by: {} — partial, cannot decide statically)",
            partial.family,
            partial.table,
            partial.chain,
            partial.kind,
            partial.narrowing.join(", ")
        );
    }
}

fn print_forward_chain_lines(nft: &NftInspection) {
    if nft.foreign_forward_chains.is_empty() {
        println!("  foreign fwd:       none");
        return;
    }
    for chain in &nft.foreign_forward_chains {
        let class_label = match chain.class {
            ForwardChainClass::PureAccept => "pure-accept",
            ForwardChainClass::BlocksOrUnknown => "blocks-or-unknown",
        };
        let detail = if chain.reasons.is_empty() {
            String::new()
        } else {
            format!(" [{}]", chain.reasons.join("; "))
        };
        println!(
            "  foreign fwd:       {} {}::{} (priority {}, policy {}, {class_label}{detail})",
            chain.family, chain.table, chain.chain, chain.priority, chain.policy
        );
    }
    if !nft.blocks_or_unknown_chains.is_empty() {
        println!(
            "  fwd verdict:       cannot be decided statically (foreign forward chain(s) \
             classified blocks-or-unknown; requires --force and guest-side verification)"
        );
    }
}
