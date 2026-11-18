//! Caddy reverse-proxy, systemd unit files, and preflight check script.

use std::fs;
use std::path::Path;

use outbe_operator_systemd::{founding_units, FoundingConfig};
use serde_json::Value;

use crate::error::GenesisError;
use crate::network::rpc_bind_ip;

use super::{port_of, quote, write_script, ROOT_HELPER};

/// Render `Caddyfile` for one validator host.
pub fn caddyfile(config: &Value, host: &str) -> Result<String, GenesisError> {
    let rpc_port = port_of(config, "rpc_port")?;
    let status_port = port_of(config, "radicle_status_port")?;
    let public_rpc = port_of(config, "public_rpc_port")?;
    let public_radicle = port_of(config, "public_radicle_status_port")?;
    let rpc_bind = rpc_bind_ip(config, host)?;
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
\t\treverse_proxy {rpc_bind}:{rpc_port} {{\n\
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
        "\n{root_helper}\n\n\
# Publish this validator's RPC and Radicle status through caddy.\n\
if ! command -v caddy >/dev/null; then\n\
  echo \"installing caddy...\"\n\
  as_root apt-get update -qq\n\
  as_root apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl >/dev/null\n\
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \\\n\
    | as_root gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg\n\
  curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \\\n\
    | as_root tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null\n\
  as_root apt-get update -qq\n\
  as_root apt-get install -y -qq caddy >/dev/null\n\
fi\n\
\n\
as_root install -m 644 {} /etc/caddy/Caddyfile\n\
as_root systemctl enable --now caddy\n\
as_root systemctl reload caddy 2>/dev/null || as_root systemctl restart caddy\n\
\n\
# Let the world reach the published ports; the node's own ports stay loopback.\n\
as_root ufw allow {public_rpc}/tcp >/dev/null 2>&1 || true\n\
as_root ufw allow {public_radicle}/tcp >/dev/null 2>&1 || true\n\
\n\
echo \"published:\"\n\
echo \"  RPC            http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_rpc}\"\n\
echo \"  Radicle status http://$(curl -s -m 5 ifconfig.me || echo '<this-host>'):{public_radicle}\"\n",
        quote(&caddyfile_path),
        root_helper = ROOT_HELPER,
    ))
}

/// Write the services and installer for one founding-validator host.
pub fn write_systemd_units(
    output_dir: &Path,
    base_dir: &str,
    index: usize,
) -> Result<(), GenesisError> {
    let unit_dir = output_dir.join("systemd");
    fs::create_dir_all(&unit_dir).map_err(|err| GenesisError::io(&unit_dir, err))?;
    for unit in founding_units(FoundingConfig {
        base_dir,
        validator_index: index,
    }) {
        let unit_path = unit_dir.join(unit.name);
        fs::write(&unit_path, unit.content).map_err(|err| GenesisError::io(&unit_path, err))?;
    }

    let base = quote(base_dir);
    let install = format!(
        "\n{root_helper}\n\n\
# Install this machine's single founding validator. `--prepare-only` installs\n\
# the units without enabling or starting the network.\n\
START=true\n\
case \"${{1:-}}\" in\n\
  \"\") ;;\n\
  --prepare-only) START=false ;;\n\
  *) echo \"usage: $0 [--prepare-only]\" >&2; exit 2 ;;\n\
esac\n\
UNIT_DIR=\"$(cd -- \"$(dirname -- \"${{BASH_SOURCE[0]}}\")/systemd\" && pwd)\"\n\
\n\
getent group outbe >/dev/null || as_root groupadd --system outbe\n\
id -u outbe >/dev/null 2>&1 || as_root useradd --system --gid outbe \\\n  --home-dir {base} --shell /usr/sbin/nologin outbe\n\
as_root install -d -m 0700 {base}/validator-{index}/tee /var/lib/outbe/tee\n\
as_root chown -R outbe:outbe {base}\n\
as_root systemctl disable --now outbe-tee-renew.timer >/dev/null 2>&1 || true\n\
as_root rm -f /etc/systemd/system/outbe-tee-renew.timer /etc/systemd/system/outbe-tee-renew.service\n\
as_root install -m 0644 \"$UNIT_DIR\"/outbe-* /etc/systemd/system/\n\
as_root systemctl daemon-reload\n\
\n\
if \"$START\"; then\n\
  as_root systemctl enable --now outbe-validator.target >/dev/null\n\
else\n\
  as_root systemctl disable --now outbe-validator.target >/dev/null 2>&1 || true\n\
fi\n\
",
        root_helper = ROOT_HELPER,
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
    let hyperlane_binary = config
        .get("hyperlane_binary")
        .and_then(Value::as_str)
        .unwrap_or("outbe-hyperlane-validator");
    let runner = config
        .get("enclave_runner")
        .and_then(Value::as_str)
        .unwrap_or("outbe-tee-enclave-confidential-vm");
    let enclave_check = format!(
        "runner={}\n\
if [ -x \"$runner\" ] || command -v \"$runner\" >/dev/null 2>&1; then\n\
  note \"confidential VM signer\" \"outbe-tee-enclave-confidential-vm\"\n\
else\n\
  note \"confidential VM signer\" \"MISSING $runner\"; fail=1\n\
fi\n",
        quote(runner)
    );
    Ok(format!(
        "\n# Read-only pre-launch check for validator {index}. Exits non-zero on anything\n\
# that would make the network fail after start rather than before.\n\
cd {}\n\
fail=0\n\
note() {{ printf '  %-42s %s\\n' \"$1\" \"$2\"; }}\n\
\n\
note \"genesis sha256\" \"$(sha256sum genesis.json | cut -c1-16)\"\n\
echo \"  ^ genesis must be identical on every machine\"\n\
if [ -r .bb-crs/bn254_g1.dat ]; then\n\
  note \"CRS sha256\" \"$(sha256sum .bb-crs/bn254_g1.dat | cut -c1-16)\"\n\
else\n\
  note \"CRS\" \"MISSING .bb-crs/bn254_g1.dat\"; fail=1\n\
fi\n\
{enclave_check}\n\
if [ ! -x {hyperlane_binary} ]; then\n\
  note \"Hyperlane validator\" \"MISSING {hyperlane_binary}\"; fail=1\n\
fi\n\
for required in keys/validator-{index}/hyperlane-key.hex validator-{index}/hyperlane-public.toml; do\n\
  [ -r \"$required\" ] || {{ note \"Hyperlane artifact\" \"MISSING $required\"; fail=1; }}\n\
done\n\
\n\
# State from an earlier genesis belongs to a different chain; the node refuses to start on it.\n\
for stale in validator-{index}/data validator-{index}/consensus tee; do\n\
  [ -e \"$stale\" ] && {{ note \"leftover state\" \"$stale - remove before a new genesis\"; fail=1; }}\n\
done\n\
\n\
for port in {tee_port} {rpc_port} 9000 9003; do\n\
  ss -ltn 2>/dev/null | grep -q \":$port \" && {{ note \"port $port\" \"already in use\"; fail=1; }}\n\
done\n\
\n\
if command -v ufw >/dev/null 2>&1 \
  && ufw status 2>/dev/null | grep -Eq '(^|[[:space:]]){rpc_port}(/tcp)?[[:space:]]+ALLOW'; then\n\
  note \"firewall warning\" \"RPC port {rpc_port} has an allow rule; verify its source scope\"\n\
fi\n\
\n\
if [ \"$fail\" = 0 ]; then echo; echo \"  preflight OK\"; else echo; echo \"  preflight FAILED\"; fi\n\
exit \"$fail\"\n",
        quote(base_dir),
        hyperlane_binary = quote(hyperlane_binary),
    ))
}
