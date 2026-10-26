//! Caddy reverse-proxy, systemd unit files, and preflight check script.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::error::GenesisError;

use super::{port_of, quote, write_script};

/// Systemd service roles in dependency order: `(role, description, after_unit)`.
pub const UNIT_ROLES: &[(&str, &str, Option<&str>)] = &[
    ("enclave", "TEE enclave", None),
    (
        "radicle",
        "Radicle sidecar",
        Some("outbe-enclave@%i.service"),
    ),
    ("node", "validator node", Some("outbe-radicle@%i.service")),
    (
        "feeder",
        "price oracle feeder",
        Some("outbe-node@%i.service"),
    ),
];

/// Render `Caddyfile` for one validator host.
pub fn caddyfile(config: &Value, host: &str) -> Result<String, GenesisError> {
    let rpc_port = port_of(config, "rpc_port")?;
    let status_port = port_of(config, "radicle_status_port")?;
    let public_rpc = port_of(config, "public_rpc_port")?;
    let public_radicle = port_of(config, "public_radicle_status_port")?;
    Ok(format!(
        "# Generated for {host}. Plain HTTP on the public address: no TLS,\n\
# because these hosts are addressed by IP and have no certificate names.\n\
{{\n\
\tauto_https off\n\
\tadmin off\n\
}}\n\
\n\
# Ethereum JSON-RPC\n\
:{public_rpc} {{\n\
\t@options method OPTIONS\n\
\thandle @options {{\n\
\t\theader {{\n\
\t\t\tAccess-Control-Allow-Origin *\n\
\t\t\tAccess-Control-Allow-Methods \"GET, POST, OPTIONS\"\n\
\t\t\tAccess-Control-Allow-Headers \"Content-Type\"\n\
\t\t\tAccess-Control-Max-Age 86400\n\
\t\t}}\n\
\t\trespond 204\n\
\t}}\n\
\thandle {{\n\
\t\treverse_proxy 127.0.0.1:{rpc_port} {{\n\
\t\t\theader_up X-Real-IP {{remote_host}}\n\
\t\t\theader_down -Access-Control-Allow-Origin\n\
\t\t}}\n\
\t\theader Access-Control-Allow-Origin *\n\
\t}}\n\
}}\n\
\n\
# Radicle sidecar status (read-only JSON)\n\
:{public_radicle} {{\n\
\thandle {{\n\
\t\treverse_proxy 127.0.0.1:{status_port} {{\n\
\t\t\theader_up X-Real-IP {{remote_host}}\n\
\t\t}}\n\
\t\theader Access-Control-Allow-Origin *\n\
\t}}\n\
}}\n"
    ))
}

/// Render `install-caddy.sh` for one validator host.
pub fn caddy_install_script(
    config: &Value,
    base_dir: &str,
    index: usize,
) -> Result<String, GenesisError> {
    let public_rpc = port_of(config, "public_rpc_port")?;
    let public_radicle = port_of(config, "public_radicle_status_port")?;
    let caddyfile_path = format!("{base_dir}/validator-{index}/Caddyfile");
    Ok(format!(
        "\n# Publish this validator's RPC and Radicle status through caddy.\n\
if ! command -v caddy >/dev/null; then\n\
  echo \"installing caddy...\"\n\
  sudo apt-get update -qq\n\
  sudo apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl >/dev/null\n\
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \\\n\
    | sudo gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg\n\
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \\\n\
    | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null\n\
  sudo apt-get update -qq\n\
  sudo apt-get install -y -qq caddy >/dev/null\n\
fi\n\
\n\
sudo install -m 644 {} /etc/caddy/Caddyfile\n\
sudo systemctl enable --now caddy\n\
sudo systemctl reload caddy 2>/dev/null || sudo systemctl restart caddy\n\
\n\
# Let the world reach the published ports; the node's own ports stay loopback.\n\
sudo ufw allow {public_rpc}/tcp >/dev/null 2>&1 || true\n\
sudo ufw allow {public_radicle}/tcp >/dev/null 2>&1 || true\n\
\n\
echo \"published:\"\n\
echo \"  RPC            http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_rpc}\"\n\
echo \"  Radicle status http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_radicle}\"\n",
        quote(&caddyfile_path)
    ))
}

/// Render `run-tee-renew.sh` for one validator.
pub fn tee_renew_script(
    config: &Value,
    index: usize,
    base_dir: &str,
    keys_dir: &str,
) -> Result<String, GenesisError> {
    let key_dir = format!("{keys_dir}/validator-{index}");
    let tee_port = port_of(config, "tee_enclave_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    Ok(format!(
        "\nKEY=\"$(tr -d '[:space:]' < {q_evm_key})\"\n\
exec {q_cli} tee renew \\\n\
  --enclave-socket 127.0.0.1:{tee_port} \\\n\
  --node-data-dir {q_data_dir} \\\n\
  --reth-p2p-secret-key {q_p2p_secret} \\\n\
  --private-key \"$KEY\" \\\n\
  --rpc-url http://127.0.0.1:{rpc_port}\n",
        q_evm_key = quote(&format!("{key_dir}/evm-key.hex")),
        q_cli = quote(&format!("{base_dir}/outbe-cli")),
        q_data_dir = quote(&format!("{base_dir}/validator-{index}/data")),
        q_p2p_secret = quote(&format!("{key_dir}/reth-p2p-secret.hex")),
    ))
}

/// Render one templated systemd unit file (`%i` is the validator index).
pub fn systemd_unit(role: &str, description: &str, after: Option<&str>, base_dir: &str) -> String {
    let dependency_suffix = after.map(|a| format!(" {a}")).unwrap_or_default();
    let user = if role == "enclave" { "root" } else { "ubuntu" };
    format!(
        "[Unit]\n\
Description=Outbe {description} (validator %i)\n\
After=network-online.target{dependency_suffix}\n\
Wants=network-online.target{dependency_suffix}\n\
StartLimitIntervalSec=0\n\
\n\
[Service]\n\
Type=simple\n\
User={user}\n\
WorkingDirectory={base_dir}/validator-%i\n\
ExecStart={base_dir}/validator-%i/run-{role}.sh\n\
Restart=always\n\
RestartSec=5\n\
StandardOutput=journal\n\
StandardError=journal\n\
\n\
[Install]\n\
WantedBy=multi-user.target\n"
    )
}

/// Write all systemd service/timer templates and `install-systemd.sh`.
pub fn write_systemd_units(
    output_dir: &Path,
    base_dir: &str,
    enable_tee_renewal: bool,
) -> Result<(), GenesisError> {
    let unit_dir = output_dir.join("systemd");
    fs::create_dir_all(&unit_dir).map_err(|err| GenesisError::io(&unit_dir, err))?;
    for &(role, description, after) in UNIT_ROLES {
        let unit = systemd_unit(role, description, after, base_dir);
        let unit_path = unit_dir.join(format!("outbe-{role}@.service"));
        fs::write(&unit_path, unit).map_err(|err| GenesisError::io(&unit_path, err))?;
    }

    let renew_service = unit_dir.join("outbe-tee-renew@.service");
    fs::write(
        &renew_service,
        format!(
            "[Unit]\n\
Description=Outbe founder TEE lease renewal (validator %i)\n\
After=network-online.target outbe-enclave@%i.service outbe-node@%i.service\n\
Requisite=outbe-enclave@%i.service outbe-node@%i.service\n\
\n\
[Service]\n\
Type=oneshot\n\
User=ubuntu\n\
WorkingDirectory={base_dir}/validator-%i\n\
ExecStart={base_dir}/validator-%i/run-tee-renew.sh\n\
TimeoutStartSec=10min\n\
UMask=0077\n\
LimitCORE=0\n"
        ),
    )
    .map_err(|err| GenesisError::io(&renew_service, err))?;

    let renew_timer = unit_dir.join("outbe-tee-renew@.timer");
    fs::write(
        &renew_timer,
        "[Unit]\n\
Description=Daily Outbe founder TEE lease renewal check (validator %i)\n\
\n\
[Timer]\n\
OnCalendar=daily\n\
Persistent=true\n\
\n\
[Install]\n\
WantedBy=timers.target\n",
    )
    .map_err(|err| GenesisError::io(&renew_timer, err))?;

    let renewal_install = if enable_tee_renewal {
        "\nsudo systemctl enable --now \"outbe-tee-renew@$INDEX.timer\"\n"
    } else {
        ""
    };
    let q_base = quote(base_dir);
    let install = format!(
        "\n# Install and start every Outbe service for validator $1 on this machine.\n\
INDEX=\"${{1:?usage: install-systemd.sh <validator-index>}}\"\n\
\n\
sudo install -m 644 {q_base}/systemd/outbe-*@.service /etc/systemd/system/\n\
sudo install -m 644 {q_base}/systemd/outbe-*@.timer /etc/systemd/system/\n\
sudo systemctl daemon-reload\n\
\n\
\n\
for role in enclave radicle node feeder; do\n\
  sudo systemctl enable --now \"outbe-$role@$INDEX.service\"\n\
done\n\
{renewal_install}\n\
echo\n\
echo \"services for validator $INDEX:\"\n\
systemctl list-units 'outbe-*' --no-pager --no-legend | sed 's/^/  /'\n\
echo\n\
echo \"follow one with: journalctl -u outbe-node@$INDEX -f\"\n"
    );
    write_script(&output_dir.join("install-systemd.sh"), &install)
}

/// Render `preflight.sh` for one validator.
pub fn preflight_script(
    config: &Value,
    base_dir: &str,
    index: usize,
) -> Result<String, GenesisError> {
    let tee_port = port_of(config, "tee_enclave_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    Ok(format!(
        "\n# Read-only pre-launch check for validator {index}. Exits non-zero on anything\n\
# that would make the network fail after start rather than before.\n\
cd {}\n\
fail=0\n\
note() {{ printf '  %-42s %s\\n' \"$1\" \"$2\"; }}\n\
\n\
note \"genesis sha256\" \"$(sha256sum genesis.json | cut -c1-16)\"\n\
echo \"  ^ genesis must be identical on every machine\"\n\
\n\
if [ -f outbe-tee-enclave.sig ]; then\n\
  note \"enclave mr_enclave\" \"$(gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null | grep -oE 'mr_enclave: [0-9a-f]+' | cut -c13-28)\"\n\
  note \"enclave mr_signer\" \"$(gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null | grep -oE 'mr_signer: [0-9a-f]+' | cut -c12-27)\"\n\
  echo \"  ^ mr_signer must match across machines; a per-host key breaks dcap-required\"\n\
else\n\
  note \"enclave signature\" \"MISSING\"; fail=1\n\
fi\n\
\n\
# State from an earlier genesis belongs to a different chain; the node refuses to start on it.\n\
for stale in validator-{index}/data validator-{index}/consensus tee; do\n\
  [ -e \"$stale\" ] && {{ note \"leftover state\" \"$stale - remove before a new genesis\"; fail=1; }}\n\
done\n\
\n\
for port in {tee_port} {rpc_port}; do\n\
  ss -ltn 2>/dev/null | grep -q \":$port \" && {{ note \"port $port\" \"already in use\"; fail=1; }}\n\
done\n\
\n\
if [ \"$fail\" = 0 ]; then echo; echo \"  preflight OK\"; else echo; echo \"  preflight FAILED\"; fi\n\
exit \"$fail\"\n",
        quote(base_dir)
    ))
}
