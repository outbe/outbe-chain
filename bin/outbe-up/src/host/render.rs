//! Human-readable CLI report formatting for `outbe-up host` subcommands.

use outbe_operator_vm::host::{
    HostCheckReport, HostNatLocalReport, HostPublishReport, HostSetupReport, HostStatusReport,
};

fn print_bullet_section(heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("\n{heading}:");
    for item in items {
        println!("  - {item}");
    }
}

fn print_indented_section(heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("\n{heading}:");
    for item in items {
        println!("    {item}");
    }
}

fn print_numbered_section(leading_newline: bool, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    if leading_newline {
        println!("\n{heading}:");
    } else {
        println!("{heading}:");
    }
    for (idx, item) in items.iter().enumerate() {
        println!("  {}. {item}", idx + 1);
    }
}

pub(super) fn print_status_report(report: &HostStatusReport) {
    let bridge_state = match (report.bridge.present, report.bridge.up) {
        (true, true) => "UP",
        (true, false) => "DOWN",
        (false, _) => "ABSENT",
    };
    let dhcp_state = match (report.dhcp.detected, report.dhcp.unit_active) {
        (true, true) => "active",
        (true, false) => "detected/inactive",
        (false, _) => "not detected",
    };

    println!("Host status (schema v{}):", report.version);
    println!(
        "  Bridge:    {} ({bridge_state}, cidr={}, gw={}, managed={})",
        report.bridge.name,
        report.bridge.cidr.as_deref().unwrap_or("-"),
        report
            .bridge
            .gateway_ip
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report.bridge.management
    );
    println!(
        "  DHCP:      {dhcp_state} (unit={}, hosts={}, leases={}, pool={}..{}, lease={}, reload={})",
        report.dhcp.unit_name.as_deref().unwrap_or("-"),
        report
            .dhcp
            .hosts_file
            .as_ref()
            .map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
        report
            .dhcp
            .leases_file
            .as_ref()
            .map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
        report
            .dhcp
            .pool_start
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report
            .dhcp
            .pool_end
            .map_or_else(|| "-".to_owned(), |ip| ip.to_string()),
        report.dhcp.lease_duration.as_deref().unwrap_or("-"),
        report.dhcp.reload_mode
    );
    println!(
        "  Firewall:  nft={}, tables={}, forward_allows={}, dnat_rules={}, unparsed_dnat={}, managed_nat={}",
        if report.firewall.nft_accessible {
            "accessible"
        } else {
            "unavailable"
        },
        report.firewall.tables.len(),
        report.firewall.forward_allows.len(),
        report.firewall.dnat_rules.len(),
        report.firewall.unparsed_dnat_rules.len(),
        report.firewall.managed_nat_table_present
    );
    for chain in &report.firewall.forward_chains {
        println!(
            "             forward {} {}::{} policy={} ({:?})",
            chain.family, chain.table, chain.chain, chain.policy, chain.class
        );
    }
    println!(
        "  NAT:       egress={}, uplink={}, ip_forward=runtime:{} persistent:{} ({})",
        report.nat.egress_status,
        report.nat.uplink.as_deref().unwrap_or("-"),
        report
            .nat
            .ip_forward_runtime
            .map_or_else(|| "?".to_owned(), |v| v.to_string()),
        report
            .nat
            .ip_forward_persistent
            .effective_value
            .map_or_else(|| "unset".to_owned(), |v| v.to_string()),
        report
            .nat
            .ip_forward_persistent
            .effective_source
            .as_deref()
            .unwrap_or("none")
    );
    println!(
        "  VMM:       {} ({}, version={}, default_mode={}, default_bridge={}, vms={}/{})",
        if report.vmm.reachable {
            "reachable"
        } else {
            "unreachable"
        },
        report.vmm.socket_path.display(),
        report.vmm.version.as_deref().unwrap_or("-"),
        report.vmm.default_mode.as_deref().unwrap_or("-"),
        report.vmm.default_bridge.as_deref().unwrap_or("-"),
        report
            .vmm
            .running_vms
            .map_or_else(|| "-".to_owned(), |n| n.to_string()),
        report
            .vmm
            .total_vms
            .map_or_else(|| "-".to_owned(), |n| n.to_string())
    );
    for note in &report.vmm.drift_notes {
        println!("             drift: {note}");
    }
    println!(
        "  Platform:  kvm={}/{}, vsock={}, tee={}, qgs(required={}, active={}, pccs_active={})",
        report.platform.kvm_available,
        if report.platform.kvm_accessible {
            "rw"
        } else {
            "no-rw"
        },
        report.platform.vhost_vsock_available,
        report.platform.tee_kind,
        report.platform.qgs.required,
        report.platform.qgs.unit_active,
        report.platform.qgs.pccs_unit_active
    );
    println!("  Ready:     {}", report.ready_for_vm);
    for note in &report.notes {
        println!("  Note:      {note}");
    }
}

pub(super) fn print_setup_report(report: &HostSetupReport) {
    if report.already_configured {
        println!("Host networking is already configured (zero writes performed):");
        println!(
            "  bridge: {} ({}, managed={})",
            report.status.bridge.name,
            report.status.bridge.cidr.as_deref().unwrap_or("-"),
            report.status.bridge.management
        );
        println!(
            "  dhcp:   {} (hosts={})",
            report.status.dhcp.unit_name.as_deref().unwrap_or("-"),
            report
                .status
                .dhcp
                .hosts_file
                .as_ref()
                .map_or_else(|| "-".to_owned(), |p| p.display().to_string())
        );
        println!(
            "  egress: {} (ip_forward={})",
            report.status.nat.egress_status,
            report
                .status
                .nat
                .ip_forward_runtime
                .map_or_else(|| "?".to_owned(), |v| v.to_string())
        );
        return;
    }

    println!("Host is missing required VM networking component(s):");
    for item in &report.missing_components {
        println!("  - {item}");
    }
    println!("\nRequired setup steps:");
    for (idx, step) in report.planned_steps.iter().enumerate() {
        println!("  {}. {step}", idx + 1);
    }
    if let Some(note) = &report.approval_note {
        println!("\n{note}");
    }
}

pub(super) fn print_nat_report(report: &HostNatLocalReport) {
    println!(
        "Host NAT ({}, bridge={}, outcome={}) [read-only; zero writes]:",
        report.action, report.bridge, report.outcome
    );
    println!("  {}", report.detail);
    for warning in &report.warnings {
        println!("  Warning: {warning}");
    }
    if let Some(ruleset) = &report.rendered_ruleset {
        println!("\nRecommended nftables ruleset:\n{ruleset}");
    }
    if let Some(unit) = &report.rendered_unit {
        println!("Recommended systemd unit:\n{unit}");
    }
    if let Some(sysctl) = &report.rendered_sysctl {
        println!("Recommended sysctl drop-in:\n{sysctl}");
    }
    print_numbered_section(
        false,
        "Recommended command(s)",
        &report.recommended_commands,
    );
}

pub(super) fn print_check_report(report: &HostCheckReport) {
    println!(
        "Host check for VM `{}` ({}, mac={}, ip={}, bridge={}) [read-only; zero writes]:",
        report.vm, report.vm_id, report.mac, report.ip, report.bridge
    );
    println!("  host_network_ok:    {}", report.host_network_ok);
    println!("  service_ip_ok:      {}", report.service_ip_ok);
    println!(
        "  dhcp_reserved:      {} (optional; static internal IP)",
        report.dhcp_reserved
    );
    println!("  published_ports_ok: {}", report.published_ports_ok);
    println!("  ready:              {}", report.ready);

    print_bullet_section("Conflicts", &report.conflicts);
    print_bullet_section("Missing prerequisites", &report.missing);
    if let Some(block) = &report.recommended_dhcp_block {
        let hosts = report
            .hosts_file
            .as_ref()
            .map_or_else(|| "<dhcp-hosts>".to_owned(), |p| p.display().to_string());
        println!("\nRecommended DHCP reservation for {hosts}:\n{block}");
        if let Some(reload) = &report.recommended_dhcp_reload {
            println!("Reload DHCP:\n  {reload}");
        }
    }
    print_indented_section(
        "Recommended forward accept rule(s)",
        &report.recommended_forward_rules,
    );
    print_indented_section(
        "Recommended prerouting DNAT rule(s)",
        &report.recommended_prerouting_rules,
    );
    print_numbered_section(
        true,
        "Apply firewall changes",
        &report.recommended_firewall_commands,
    );
}

pub(super) fn print_publish_report(report: &HostPublishReport) {
    println!(
        "Host publish plan for VM `{}` ({}, guest_ip={}, public_ip={}) [read-only; zero writes]:",
        report.vm, report.vm_id, report.guest_ip, report.public_ip
    );
    println!(
        "  detected firewall: {} (forward policy={}), {}, file={}",
        report.forward_table,
        report.forward_policy,
        report.nat_table,
        report.firewall_file.display()
    );
    if !report.conflicts.is_empty() {
        println!("\nCONFLICTS DETECTED:");
        for c in &report.conflicts {
            println!(
                "  - {}/{} is already DNATed to {}:{} in {}::{} (sources={:?}, comment={}, handle={:?})",
                c.proto,
                c.host_port,
                c.existing_dest_ip,
                c.existing_guest_port,
                c.existing_table,
                c.existing_chain,
                c.existing_sources,
                c.existing_comment.as_deref().unwrap_or("-"),
                c.existing_handle
            );
        }
        return;
    }

    println!(
        "\nRules for `table {} {{ chain forward {{ ... }} }}` in {}:",
        report.forward_table,
        report.firewall_file.display()
    );
    for rule in &report.forward_rules {
        println!("    {rule}");
    }
    println!(
        "\nRules for `table {} {{ chain prerouting {{ ... }} }}` in {}:",
        report.nat_table,
        report.firewall_file.display()
    );
    for rule in &report.prerouting_rules {
        println!("    {rule}");
    }
    print_numbered_section(true, "Apply", &report.apply_commands);
    print_numbered_section(true, "Remove", &report.remove_commands);
    println!("\nNo files or nftables rules were modified (read-only).");
}
