//! Host SGX preflight: CPU topology, EPC regions, and device/AESM mounts.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::FileTypeExt as _;
use std::path::Path;
use std::process::Command;

use eyre::{bail, ensure, eyre, Result, WrapErr as _};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct EpcRegion {
    pub(super) node: String,
    pub(super) total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct HostPreflight {
    pub(super) architecture: String,
    pub(super) cpu_model: String,
    pub(super) epc: Vec<EpcRegion>,
    pub(super) physical_package_count: u64,
    pub(super) sgx_enclave_device: String,
    pub(super) sgx_provision_device: String,
}

pub(super) fn host_preflight() -> Result<HostPreflight> {
    ensure!(std::env::consts::ARCH == "x86_64", "H1 requires x86_64");
    let (sgx_enclave_device, sgx_provision_device) = sgx_devices()?;
    let lscpu: Value = serde_json::from_slice(
        &Command::new("lscpu")
            .arg("--json")
            .output()
            .wrap_err("run lscpu preflight")?
            .stdout,
    )
    .wrap_err("parse lscpu JSON")?;
    let fields = lscpu_fields(&lscpu)?;
    let packages = parse_lscpu_u64(&fields, &["Socket(s):", "Sockets:"])?;
    let cpu_model = field_value(&fields, &["Model name:"])?;
    let cpuinfo = fs::read_to_string("/proc/cpuinfo")?;
    ensure!(
        cpuinfo.lines().any(|line| {
            line.starts_with("flags")
                && line
                    .split_whitespace()
                    .any(|flag| flag == "sgx" || flag == "sgx_lc")
        }),
        "CPU flags do not expose SGX"
    );
    let epc = epc_regions()?;
    ensure!(
        epc.iter().any(|region| region.total_bytes > 0),
        "no non-zero SGX EPC region is exposed"
    );
    Ok(HostPreflight {
        architecture: "x86_64".to_owned(),
        cpu_model,
        epc,
        physical_package_count: packages,
        sgx_enclave_device,
        sgx_provision_device,
    })
}

pub(super) fn lscpu_fields(value: &Value) -> Result<BTreeMap<String, String>> {
    let rows = value
        .get("lscpu")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("lscpu JSON has no lscpu array"))?;
    let mut fields = BTreeMap::new();
    for row in rows {
        let field = row.get("field").and_then(Value::as_str);
        let data = row.get("data").and_then(Value::as_str);
        if let (Some(field), Some(data)) = (field, data) {
            fields.insert(field.to_owned(), data.to_owned());
        }
    }
    Ok(fields)
}

pub(super) fn parse_lscpu_u64(fields: &BTreeMap<String, String>, names: &[&str]) -> Result<u64> {
    field_value(fields, names)?
        .parse()
        .wrap_err_with(|| format!("lscpu {} is not u64", names.join("/")))
}

pub(super) fn field_value(fields: &BTreeMap<String, String>, names: &[&str]) -> Result<String> {
    names
        .iter()
        .find_map(|name| fields.get(*name))
        .cloned()
        .ok_or_else(|| eyre!("lscpu lacks {}", names.join("/")))
}

pub(super) fn epc_regions() -> Result<Vec<EpcRegion>> {
    let mut regions = Vec::new();
    let nodes = Path::new("/sys/devices/system/node");
    if nodes.is_dir() {
        for entry in fs::read_dir(nodes)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("node") || !name[4..].bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let path = entry.path().join("x86/sgx_total_bytes");
            if path.is_file() {
                let total_bytes = fs::read_to_string(&path)?.trim().parse::<u64>()?;
                regions.push(EpcRegion {
                    node: name,
                    total_bytes,
                });
            }
        }
    }
    regions.sort_by(|a, b| a.node.cmp(&b.node));
    Ok(regions)
}

pub(super) fn sgx_devices() -> Result<(String, String)> {
    for (enclave, provision) in [
        ("/dev/sgx_enclave", "/dev/sgx_provision"),
        ("/dev/sgx/enclave", "/dev/sgx/provision"),
    ] {
        if Path::new(enclave).exists() && Path::new(provision).exists() {
            return Ok((enclave.to_owned(), provision.to_owned()));
        }
    }
    bail!("required SGX enclave and provision devices are absent");
}

pub(crate) fn add_sgx_runtime_mounts(command: &mut Command) -> Result<()> {
    let (enclave, provision) = sgx_devices()?;
    command
        .args(["--device", &format!("{enclave}:/dev/sgx_enclave")])
        .args(["--device", &format!("{provision}:/dev/sgx_provision")]);

    const AESM_SOCKET: &str = "/var/run/aesmd/aesm.socket";
    let metadata = fs::symlink_metadata(AESM_SOCKET)
        .wrap_err_with(|| format!("required AESM socket is absent: {AESM_SOCKET}"))?;
    if !metadata.file_type().is_socket() {
        bail!("required AESM path is not a Unix socket: {AESM_SOCKET}");
    }
    command.args(["-v", &format!("{AESM_SOCKET}:{AESM_SOCKET}")]);
    Ok(())
}
