//! Managed file rendering and the shell commands that run on the NAT host.

use std::path::Path;

use eyre::{ensure, Result};

use super::{
    HostNatPaths, Ipv4Subnet, NAT_SERVICE_NAME, NAT_TABLE_FAMILY, NAT_TABLE_NAME, OWNERSHIP_MARKER,
    SENTINEL_BRIDGE_ADDR, SENTINEL_IP_FORWARD, SENTINEL_NFT_JSON, SENTINEL_OWNED_FILES,
    SENTINEL_PERSISTENT_SYSCTL, SENTINEL_UPLINK_ROUTE,
};
use crate::shell_quote;

/// Strictly validate a Linux network interface name to prevent shell and nftables injection.
///
/// Accepts 1..=15 ASCII characters: starts with alphanumeric, followed by alphanumeric,
/// `.`, `_`, or `-`.
pub fn validate_iface_name(name: &str, label: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 15,
        "{label} interface name must be 1..=15 characters, got '{name}'"
    );
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "{label} interface name must start with an ASCII letter or digit, got '{name}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
        "{label} interface name contains forbidden characters (allowed: A-Z, a-z, 0-9, ., _, -): '{name}'"
    );
    Ok(())
}

/// Render the atomic, idempotent nftables ruleset for `table ip outbe_up_nat`.
///
/// Uses the `ip` family because VM bridge networking and `net.ipv4.ip_forward` are
/// IPv4-only; this avoids touching IPv6 hooks and works on all Linux nftables kernels.
pub(crate) fn render_nat_ruleset(bridge: &str, subnet: Ipv4Subnet, uplink: &str) -> Result<String> {
    validate_iface_name(bridge, "bridge")?;
    validate_iface_name(uplink, "uplink")?;
    ensure!(
        bridge != uplink,
        "uplink interface '{uplink}' must differ from bridge interface '{bridge}'"
    );
    let cidr = subnet.cidr_string();
    Ok(format!(
        "{OWNERSHIP_MARKER}\n\
         table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}\n\
         delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME}\n\
         \n\
         table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} {{\n\
         \tchain postrouting {{\n\
         \t\ttype nat hook postrouting priority srcnat; policy accept;\n\
         \t\tiifname \"{bridge}\" oifname \"{uplink}\" ip saddr {cidr} masquerade\n\
         \t}}\n\
         \n\
         \tchain forward {{\n\
         \t\ttype filter hook forward priority filter; policy accept;\n\
         \t\tiifname \"{bridge}\" oifname \"{uplink}\" ip saddr {cidr} accept\n\
         \t\tiifname \"{uplink}\" oifname \"{bridge}\" ip daddr {cidr} ct state established,related accept\n\
         \t}}\n\
         }}\n"
    ))
}

pub(crate) fn render_systemd_unit(rules_path: &Path) -> String {
    let rules = rules_path.display();
    format!(
        "{OWNERSHIP_MARKER}\n\
         [Unit]\n\
         Description=Outbe VM outbound NAT masquerade\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=/usr/sbin/nft -f {rules}\n\
         ExecStop=/bin/sh -c '/usr/sbin/nft delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} 2>/dev/null || true'\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

fn ownership_probe_snippet(paths: &HostNatPaths) -> String {
    let rules_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);
    format!(
        "probe_file() {{\n\
           if [ ! -e \"$1\" ]; then echo absent;\n\
           elif grep -Fq -- {marker_q} \"$1\" 2>/dev/null; then echo owned;\n\
           else echo foreign; fi\n\
         }}\n\
         printf 'rules=%s\\n' \"$(probe_file {rules_q})\"\n\
         printf 'service=%s\\n' \"$(probe_file {service_q})\"\n\
         printf 'sysctl=%s\\n' \"$(probe_file {sysctl_q})\"\n"
    )
}

pub(super) fn build_ownership_probe_command(paths: &HostNatPaths) -> String {
    let script = ownership_probe_snippet(paths);
    format!("sudo -n sh -ec {}", shell_quote(&script))
}

pub(super) fn build_preflight_command(
    bridge: &str,
    uplink_override: Option<&str>,
    paths: &HostNatPaths,
) -> Result<String> {
    validate_iface_name(bridge, "bridge")?;
    let uplink_snippet = match uplink_override {
        Some(uplink) => {
            validate_iface_name(uplink, "uplink")?;
            let q = shell_quote(uplink);
            format!("ip link show dev {q} >/dev/null && printf 'override %s\\n' {q}")
        }
        None => "ip -4 -o route show default".to_owned(),
    };
    let bridge_q = shell_quote(bridge);
    let scan_dirs = paths
        .sysctl_scan_dirs
        .iter()
        .map(|d| shell_quote(&d.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let sysctl_conf_q = shell_quote(&paths.sysctl_conf_path.to_string_lossy());
    let owned_snippet = ownership_probe_snippet(paths);

    let script = format!(
        "printf '%s\\n' '{SENTINEL_BRIDGE_ADDR}'\n\
         ip -4 -o addr show dev {bridge_q}\n\
         printf '%s\\n' '{SENTINEL_UPLINK_ROUTE}'\n\
         {uplink_snippet}\n\
         printf '%s\\n' '{SENTINEL_IP_FORWARD}'\n\
         sysctl -n net.ipv4.ip_forward\n\
         printf '%s\\n' '{SENTINEL_PERSISTENT_SYSCTL}'\n\
         dir_idx=0\n\
         for d in {scan_dirs}; do\n\
           if [ -d \"$d\" ]; then\n\
             for f in \"$d\"/*.conf; do\n\
               [ -e \"$f\" ] || [ -L \"$f\" ] || continue\n\
               printf 'FILE %s %s\\n' \"$dir_idx\" \"$f\"\n\
               if [ -f \"$f\" ]; then\n\
                 grep -H -E '^[[:space:]]*net[./]ipv4[./]ip_forward[[:space:]]*=' \"$f\" 2>/dev/null | sed 's/^/SET /' || true\n\
               fi\n\
             done\n\
           fi\n\
           dir_idx=$((dir_idx + 1))\n\
         done\n\
         if [ -f {sysctl_conf_q} ]; then\n\
           printf 'CONF %s\\n' {sysctl_conf_q}\n\
           grep -H -E '^[[:space:]]*net[./]ipv4[./]ip_forward[[:space:]]*=' {sysctl_conf_q} 2>/dev/null | sed 's/^/SET /' || true\n\
         fi\n\
         printf '%s\\n' '{SENTINEL_OWNED_FILES}'\n\
         {owned_snippet}\
         printf '%s\\n' '{SENTINEL_NFT_JSON}'\n\
         nft -j list ruleset\n"
    );
    Ok(format!("sudo -n sh -ec {}", shell_quote(&script)))
}

pub(crate) fn build_apply_command(
    ruleset: &str,
    unit: &str,
    write_sysctl_dropin: bool,
    enable_runtime_ip_forward: bool,
    paths: &HostNatPaths,
) -> String {
    let rules_dir = paths
        .rules_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/outbe-up"));
    let service_dir = paths
        .service_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/systemd/system"));
    let sysctl_dir = paths
        .sysctl_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/sysctl.d"));

    let rules_dir_q = shell_quote(&rules_dir.to_string_lossy());
    let rules_path_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_dir_q = shell_quote(&service_dir.to_string_lossy());
    let service_path_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_dir_q = shell_quote(&sysctl_dir.to_string_lossy());
    let sysctl_path_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);

    let mut script = format!(
        "check_owner() {{\n\
           if [ -e \"$1\" ] && ! grep -Fq -- {marker_q} \"$1\" 2>/dev/null; then\n\
             echo \"refusing to overwrite unmanaged file: $1\" >&2\n\
             exit 1\n\
           fi\n\
         }}\n\
         check_owner {rules_path_q}\n\
         check_owner {service_path_q}\n"
    );
    if write_sysctl_dropin {
        script.push_str(&format!("check_owner {sysctl_path_q}\n"));
    }

    // Load atomically via `nft -f -` before writing persistent files so a syntax/kernel
    // rejection leaves no partial files behind.
    script.push_str(&format!(
        "nft -f - <<'OUTBE_NAT_RULES_EOF'\n\
         {ruleset}OUTBE_NAT_RULES_EOF\n\
         mkdir -p {rules_dir_q} {service_dir_q}\n\
         tmp_rules=$(mktemp \"{}.tmp.XXXXXX\")\n\
         trap 'rm -f \"$tmp_rules\" \"${{tmp_unit:-}}\" \"${{tmp_sysctl:-}}\"' EXIT\n\
         cat <<'OUTBE_NAT_RULES_EOF' > \"$tmp_rules\"\n\
         {ruleset}OUTBE_NAT_RULES_EOF\n\
         chmod 0644 \"$tmp_rules\"\n\
         mv -f \"$tmp_rules\" {rules_path_q}\n",
        paths.rules_path.display()
    ));

    if write_sysctl_dropin {
        script.push_str(&format!(
            "mkdir -p {sysctl_dir_q}\n\
             tmp_sysctl=$(mktemp \"{}.tmp.XXXXXX\")\n\
             cat <<'OUTBE_NAT_SYSCTL_EOF' > \"$tmp_sysctl\"\n\
             {OWNERSHIP_MARKER}\n\
             net.ipv4.ip_forward = 1\n\
             OUTBE_NAT_SYSCTL_EOF\n\
             chmod 0644 \"$tmp_sysctl\"\n\
             mv -f \"$tmp_sysctl\" {sysctl_path_q}\n",
            paths.sysctl_path.display()
        ));
    }

    if enable_runtime_ip_forward {
        script.push_str("sysctl -w net.ipv4.ip_forward=1\n");
    }

    script.push_str(&format!(
        "tmp_unit=$(mktemp \"{}.tmp.XXXXXX\")\n\
         cat <<'OUTBE_NAT_UNIT_EOF' > \"$tmp_unit\"\n\
         {unit}OUTBE_NAT_UNIT_EOF\n\
         chmod 0644 \"$tmp_unit\"\n\
         mv -f \"$tmp_unit\" {service_path_q}\n\
         systemctl daemon-reload\n\
         systemctl enable {NAT_SERVICE_NAME}\n",
        paths.service_path.display()
    ));

    format!("sudo -n sh -ec {}", shell_quote(&script))
}

/// Remove only managed artifacts carrying `OWNERSHIP_MARKER` and our nftables table.
/// Never writes `net.ipv4.ip_forward=0` and never deletes foreign files.
pub(crate) fn build_remove_command(paths: &HostNatPaths) -> String {
    let rules_dir = paths
        .rules_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/outbe-up"));
    let rules_dir_q = shell_quote(&rules_dir.to_string_lossy());
    let rules_path_q = shell_quote(&paths.rules_path.to_string_lossy());
    let service_path_q = shell_quote(&paths.service_path.to_string_lossy());
    let sysctl_path_q = shell_quote(&paths.sysctl_path.to_string_lossy());
    let marker_q = shell_quote(OWNERSHIP_MARKER);

    let script = format!(
        "nft delete table {NAT_TABLE_FAMILY} {NAT_TABLE_NAME} 2>/dev/null || true\n\
         if [ -f {service_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {service_path_q} 2>/dev/null; then\n\
             systemctl disable --now {NAT_SERVICE_NAME} 2>/dev/null || true\n\
             rm -f {service_path_q}\n\
             systemctl daemon-reload\n\
           else\n\
             echo \"skipping unmanaged service file: {}\" >&2\n\
           fi\n\
         fi\n\
         if [ -f {rules_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {rules_path_q} 2>/dev/null; then\n\
             rm -f {rules_path_q}\n\
             rmdir {rules_dir_q} 2>/dev/null || true\n\
           else\n\
             echo \"skipping unmanaged ruleset file: {}\" >&2\n\
           fi\n\
         fi\n\
         if [ -f {sysctl_path_q} ]; then\n\
           if grep -Fq -- {marker_q} {sysctl_path_q} 2>/dev/null; then\n\
             rm -f {sysctl_path_q}\n\
           else\n\
             echo \"skipping unmanaged sysctl file: {}\" >&2\n\
           fi\n\
         fi\n",
        paths.service_path.display(),
        paths.rules_path.display(),
        paths.sysctl_path.display()
    );
    format!("sudo -n sh -ec {}", shell_quote(&script))
}
