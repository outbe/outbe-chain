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

/// Render `run-tee-renew.sh` for one validator.
pub fn tee_renew_script(
    config: &Value,
    index: usize,
    base_dir: &str,
    keys_dir: &str,
    validator_host: &str,
) -> Result<String, GenesisError> {
    let key_dir = format!("{keys_dir}/validator-{index}");
    let tee_port = port_of(config, "tee_enclave_port")?;
    let rpc_port = port_of(config, "rpc_port")?;
    let rpc_bind = rpc_bind_ip(config, validator_host)?;
    Ok(format!(
        "\nKEY=\"$(tr -d '[:space:]' < {q_evm_key})\"\n\
exec {q_cli} tee renew \\\n\
  --enclave-socket 127.0.0.1:{tee_port} \\\n\
  --node-data-dir {q_data_dir} \\\n\
  --reth-p2p-secret-key {q_p2p_secret} \\\n\
  --private-key \"$KEY\" \\\n\
  --rpc-url http://{rpc_bind}:{rpc_port}\n",
        q_evm_key = quote(&format!("{key_dir}/evm-key.hex")),
        q_cli = quote(&format!("{base_dir}/outbe-cli")),
        q_data_dir = quote(&format!("{base_dir}/validator-{index}/data")),
        q_p2p_secret = quote(&format!("{key_dir}/reth-p2p-secret.hex")),
    ))
}

/// Write the services and installer for one founding-validator host.
pub fn write_systemd_units(
    output_dir: &Path,
    base_dir: &str,
    index: usize,
    enable_tee_renewal: bool,
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

    let renewal_install = if enable_tee_renewal {
        "as_root systemctl enable --now outbe-tee-renew.timer\n"
    } else {
        ""
    };
    let base = quote(base_dir);
    let install = format!(
        "\n{root_helper}\n\n\
# Install and start this machine's single founding validator.\n\
UNIT_DIR=\"$(cd -- \"$(dirname -- \"${{BASH_SOURCE[0]}}\")/systemd\" && pwd)\"\n\
\n\
getent group outbe >/dev/null || as_root groupadd --system outbe\n\
id -u outbe >/dev/null 2>&1 || as_root useradd --system --gid outbe \\\n  --home-dir {base} --shell /usr/sbin/nologin outbe\n\
as_root install -d -m 0700 {base}/validator-{index}/tee /var/lib/outbe/tee\n\
as_root chown -R outbe:outbe {base}\n\
as_root install -m 0644 \"$UNIT_DIR\"/outbe-* /etc/systemd/system/\n\
as_root systemctl daemon-reload\n\
\n\
as_root systemctl enable --now outbe-validator.target\n\
{renewal_install}\n\
echo\n\
echo \"founding validator services:\"\n\
systemctl list-units 'outbe-*' --no-pager --no-legend | sed 's/^/  /'\n\
echo\n\
echo \"follow the node with: journalctl -u outbe-validator -f\"\n",
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
    let default_enclave_dir = format!("{base_dir}/sgx");
    let enclave_dir = config
        .get("enclave_dir")
        .and_then(Value::as_str)
        .unwrap_or(&default_enclave_dir);
    let manifest = quote(&format!("{enclave_dir}/outbe-tee-enclave.manifest.sgx"));
    let signature = quote(&format!("{enclave_dir}/outbe-tee-enclave.sig"));
    let enclave_check = if super::uses_production_tee_session(config) {
        format!(
            "manifest={manifest}\n\
signature={signature}\n\
if [ -f \"$manifest\" ] && [ -f \"$signature\" ]; then\n\
  note \"enclave manifest sha256\" \"$(sha256sum \"$manifest\" | cut -c1-16)\"\n\
  note \"enclave signature sha256\" \"$(sha256sum \"$signature\" | cut -c1-16)\"\n\
  if command -v gramine-sgx-sigstruct-view >/dev/null; then\n\
    note \"enclave mr_enclave\" \"$(gramine-sgx-sigstruct-view \"$signature\" 2>/dev/null | grep -oE 'mr_enclave: [0-9a-f]+' | cut -c13-28)\"\n\
    note \"enclave mr_signer\" \"$(gramine-sgx-sigstruct-view \"$signature\" 2>/dev/null | grep -oE 'mr_signer: [0-9a-f]+' | cut -c12-27)\"\n\
  fi\n\
  echo \"  ^ enclave artifacts must match across machines\"\n\
else\n\
  note \"enclave artifacts\" \"MISSING manifest or signature\"; fail=1\n\
fi\n"
        )
    } else {
        let runner = config
            .get("enclave_runner")
            .and_then(Value::as_str)
            .unwrap_or("outbe-tee-enclave-mock");
        format!(
            "runner={}\n\
if [ -x \"$runner\" ] || command -v \"$runner\" >/dev/null 2>&1; then\n\
  note \"development enclave\" \"outbe-tee-enclave-mock\"\n\
else\n\
  note \"development enclave\" \"MISSING $runner\"; fail=1\n\
fi\n",
            quote(runner)
        )
    };
    Ok(format!(
        "\n# Read-only pre-launch check for validator {index}. Exits non-zero on anything\n\
# that would make the network fail after start rather than before.\n\
cd {}\n\
fail=0\n\
note() {{ printf '  %-42s %s\\n' \"$1\" \"$2\"; }}\n\
\n\
note \"genesis sha256\" \"$(sha256sum genesis.json | cut -c1-16)\"\n\
echo \"  ^ genesis must be identical on every machine\"\n\
{enclave_check}\n\
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
