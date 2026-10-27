//! Project toolchain version pin validation and APT closure ledger.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use eyre::{ensure, eyre, Result, WrapErr as _};
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};

use crate::release::elf::manifest::sha256_file;

pub const CONTRACT_PATH: &str = "release/project-toolchain-v1.json";
pub const APT_SOURCES_PATH: &str = "release/apt/ubuntu.sources";
pub const APT_SNAPSHOT_HOST: &str = "https://snapshot.ubuntu.com/ubuntu/";
pub const MUTABLE_APT_HOSTS: &[&str] = &[
    "archive.ubuntu.com",
    "security.ubuntu.com",
    "ports.ubuntu.com",
];

const EXPECTED_TOP_LEVEL_KEYS: &[&str] = &[
    "activation",
    "apt_provenance",
    "gramine",
    "host_only_packages",
    "platform",
    "rust",
    "schema_version",
    "system_packages",
    "target",
    "zk_tools",
];

const EXPECTED_APT_SOURCE_PATHS: &[&str] = &[
    "/etc/apt/sources.list.d/gramine.list",
    "/etc/apt/sources.list.d/intel-sgx.list",
    "/etc/apt/sources.list.d/ubuntu.sources",
    "/usr/share/keyrings/gramine-keyring.gpg",
    "/usr/share/keyrings/intel-sgx-deb.key",
    "/usr/share/keyrings/ubuntu-archive-keyring.gpg",
];

const EXPECTED_SYSTEM_PACKAGES: &[&str] = &[
    "build-essential",
    "clang",
    "cmake",
    "gcc-14-base",
    "git",
    "libc++-dev",
    "libc++abi-dev",
    "libgcc-s1",
    "libsgx-dcap-quote-verify",
    "libsgx-dcap-quote-verify-dev",
    "libsgx-headers",
    "libssl-dev",
    "libstdc++6",
    "pkg-config",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AptLedger {
    pub ledger: String,
    pub sha256: String,
    pub count: usize,
}

fn is_lower_hex_n(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn valid_pkg_version(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
}

pub(crate) fn read_json_obj(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    let val: Value =
        serde_json::from_str(&raw).wrap_err_with(|| format!("parse {}", path.display()))?;
    ensure!(
        val.is_object(),
        "{} must contain a JSON object",
        path.display()
    );
    Ok(val)
}

fn validate_apt_pin(apt: &Value) -> Result<()> {
    let obj = apt
        .as_object()
        .ok_or_else(|| eyre!("project APT provenance has an unsupported shape"))?;
    ensure!(
        obj.len() == 2 && obj.contains_key("deb_closure") && obj.contains_key("source_files"),
        "project APT provenance has an unsupported shape"
    );
    let closure = obj["deb_closure"]
        .as_object()
        .ok_or_else(|| eyre!("project APT package closure must have an exact digest and count"))?;
    let count_ok = closure
        .get("count")
        .and_then(Value::as_i64)
        .is_some_and(|c| c > 0);
    let fmt_ok = closure.get("format").and_then(Value::as_str)
        == Some("sha256sum-lines-sorted-by-filename-v1");
    let sha_ok = closure
        .get("sha256")
        .and_then(Value::as_str)
        .is_some_and(|s| is_lower_hex_n(s, 64));
    ensure!(
        closure.len() == 3 && count_ok && fmt_ok && sha_ok,
        "project APT package closure must have an exact digest and count"
    );

    let sources = obj["source_files"]
        .as_array()
        .ok_or_else(|| eyre!("project APT source and key files must be hash-pinned"))?;
    let paths: Vec<&str> = sources
        .iter()
        .filter_map(|s| s.get("path").and_then(Value::as_str))
        .collect();
    let hashes_ok = sources.iter().all(|s| {
        s.as_object().is_some_and(|m| m.len() == 2)
            && s.get("sha256")
                .and_then(Value::as_str)
                .is_some_and(|h| is_lower_hex_n(h, 64))
    });
    ensure!(
        paths == EXPECTED_APT_SOURCE_PATHS && hashes_ok,
        "project APT source and key files must be hash-pinned"
    );
    Ok(())
}

fn validate_rust_and_gramine_pin(rust: &Value, gramine: &Value) -> Result<()> {
    let r = rust
        .as_object()
        .ok_or_else(|| eyre!("project Rust pin has an unsupported shape"))?;
    ensure!(
        r.len() == 2 && r.contains_key("components") && r.contains_key("version"),
        "project Rust pin has an unsupported shape"
    );
    let r_ver = r["version"]
        .as_str()
        .ok_or_else(|| eyre!("project Rust version must be exact"))?;
    ensure!(
        r_ver.split('.').count() == 3
            && r_ver
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "project Rust version must be exact"
    );
    let comps: Vec<&str> = r["components"]
        .as_array()
        .ok_or_else(|| eyre!("project Rust components must be unique, sorted names"))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    let unique: BTreeSet<&str> = comps.iter().copied().collect();
    let sorted: Vec<&str> = unique.into_iter().collect();
    ensure!(
        !comps.is_empty() && comps == sorted,
        "project Rust components must be unique, sorted names"
    );

    let g = gramine
        .as_object()
        .ok_or_else(|| eyre!("project Gramine pin has an unsupported shape"))?;
    ensure!(
        g.len() == 2 && g.contains_key("source_commit") && g.contains_key("version"),
        "project Gramine pin has an unsupported shape"
    );
    let g_ver = g["version"]
        .as_str()
        .ok_or_else(|| eyre!("project Gramine version must be exact"))?;
    ensure!(
        g_ver.split('.').count() == 2
            && g_ver
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        "project Gramine version must be exact"
    );
    ensure!(
        g["source_commit"]
            .as_str()
            .is_some_and(|s| is_lower_hex_n(s, 40)),
        "project Gramine source commit must be an exact lowercase Git SHA"
    );
    Ok(())
}

fn valid_zk_tool_pin(tool: &Value) -> bool {
    let Some(m) = tool.as_object() else {
        return false;
    };
    if m.len() != 3 || !valid_pkg_version(m.get("version")) {
        return false;
    }
    let ver = m.get("version").and_then(Value::as_str).unwrap_or("");
    let url = m.get("archive_url").and_then(Value::as_str).unwrap_or("");
    let sha = m
        .get("archive_sha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    is_lower_hex_n(sha, 64)
        && url.starts_with("https://github.com/")
        && url.contains(&format!("/v{ver}/"))
}

fn validate_packages_and_zk(pin: &Map<String, Value>) -> Result<()> {
    let pkgs = pin["system_packages"]
        .as_object()
        .ok_or_else(|| eyre!("project system packages must be sorted exact version pins"))?;
    ensure!(
        !pkgs.is_empty() && pkgs.values().all(|v| valid_pkg_version(Some(v))),
        "project system packages must be sorted exact version pins"
    );
    let pkg_names: Vec<&str> = pkgs.keys().map(String::as_str).collect();
    ensure!(
        pkg_names == EXPECTED_SYSTEM_PACKAGES,
        "project toolchain has an unsupported system package set"
    );

    let host_only = pin["host_only_packages"]
        .as_object()
        .ok_or_else(|| eyre!("project toolchain has an unsupported host-only package set"))?;
    let host_names: Vec<&str> = host_only.keys().map(String::as_str).collect();
    ensure!(
        host_names == ["libsgx-dcap-default-qpl"]
            && host_only.values().all(|v| valid_pkg_version(Some(v))),
        "project toolchain has an unsupported host-only package set"
    );

    let zk = pin["zk_tools"]
        .as_object()
        .ok_or_else(|| eyre!("project toolchain has an unsupported ZK tool set"))?;
    let zk_names: Vec<&str> = zk.keys().map(String::as_str).collect();
    ensure!(
        zk_names == ["barretenberg", "nargo"],
        "project toolchain has an unsupported ZK tool set"
    );
    for (name, tool) in zk {
        ensure!(
            valid_zk_tool_pin(tool),
            "project {name} tool must use an exact GitHub archive pin"
        );
    }
    Ok(())
}

pub fn load_and_validate_pin(path: &Path) -> Result<Value> {
    let pin = read_json_obj(path)?;
    let obj = pin.as_object().expect("checked object");
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    ensure!(
        keys == EXPECTED_TOP_LEVEL_KEYS,
        "project toolchain pin has an unsupported shape"
    );
    ensure!(
        obj["schema_version"].as_i64() == Some(1),
        "unsupported project toolchain pin version"
    );
    ensure!(
        obj["activation"].as_str() == Some("pending-container-delivery"),
        "unsupported project toolchain activation state"
    );
    ensure!(
        obj["target"].as_str() == Some("x86_64-unknown-linux-gnu"),
        "project toolchain supports only x86_64 Linux"
    );
    ensure!(
        obj["platform"].as_str() == Some("linux/amd64"),
        "project toolchain supports only linux/amd64"
    );
    validate_apt_pin(&obj["apt_provenance"])?;
    validate_rust_and_gramine_pin(&obj["rust"], &obj["gramine"])?;
    validate_packages_and_zk(obj)?;
    Ok(pin)
}

pub fn apt_ledger(deb_dir: &Path) -> Result<AptLedger> {
    let dir = fs::canonicalize(deb_dir)?;
    let mut debs = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("deb") {
            let meta = fs::symlink_metadata(&path)?;
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "APT package closure contains a non-regular artifact"
            );
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| eyre!("non-UTF-8 deb filename"))?
                .to_owned();
            debs.push((name, path));
        }
    }
    debs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut ledger = String::new();
    for (name, path) in &debs {
        ledger.push_str(&format!("{}  {name}\n", sha256_file(path)?));
    }
    let sha256 = hex::encode(Sha256::digest(ledger.as_bytes()));
    Ok(AptLedger {
        ledger,
        sha256,
        count: debs.len(),
    })
}

pub fn verify_apt_inputs(pin_path: &Path, root: &Path, deb_dir: &Path) -> Result<()> {
    let pin = load_and_validate_pin(pin_path)?;
    let root = fs::canonicalize(root)?;
    let sources = pin["apt_provenance"]["source_files"]
        .as_array()
        .expect("validated");
    for source in sources {
        let rel = source["path"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches('/');
        let candidate = root.join(rel);
        let meta = fs::symlink_metadata(&candidate).map_err(|_| {
            eyre!(
                "missing or non-regular APT provenance input: {}",
                source["path"]
            )
        })?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "missing or non-regular APT provenance input: {}",
            source["path"]
        );
        ensure!(
            sha256_file(&candidate)? == source["sha256"].as_str().unwrap_or(""),
            "APT provenance digest mismatch: {}",
            source["path"]
        );
    }
    let actual = apt_ledger(deb_dir)?;
    let closure = &pin["apt_provenance"]["deb_closure"];
    let expected_count = closure["count"].as_u64().unwrap_or(0) as usize;
    let expected_sha = closure["sha256"].as_str().unwrap_or("");
    ensure!(
        actual.count == expected_count,
        "APT package closure count mismatch: expected {expected_count}, found {}\nresolved closure:\n{}",
        actual.count,
        actual.ledger
    );
    ensure!(
        actual.sha256 == expected_sha,
        "APT package closure digest mismatch: expected {expected_sha}, found {}\nresolved closure:\n{}",
        actual.sha256,
        actual.ledger
    );
    Ok(())
}

pub fn regenerate_apt_closure(repo_root: &Path, write: bool) -> Result<()> {
    let pin_path = repo_root.join(CONTRACT_PATH);
    let mut pin = load_and_validate_pin(&pin_path)?;
    let elf = read_json_obj(&repo_root.join("release/reproducible-elf-build-v1.json"))?;
    let base_image = elf["builder"]["base_images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|img| img.contains("gramineproject/gramine:"))
        .ok_or_else(|| eyre!("ELF build contract must name a Gramine base image"))?;
    let pkg_args = pin["system_packages"]
        .as_object()
        .expect("validated")
        .iter()
        .map(|(k, v)| format!("      {k}={}", v.as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(" \\\n");

    let recipe = format!(
        "FROM {base_image} AS probe\n\
         ARG DEBIAN_FRONTEND=noninteractive\n\
         COPY ubuntu.sources /etc/apt/sources.list.d/ubuntu.sources\n\
         RUN apt-get update && \\\n\
             apt-get install --download-only --reinstall -y --no-install-recommends \\\n\
         {pkg_args} && \\\n\
             mkdir -p /out && \\\n\
             (cd /var/cache/apt/archives && sha256sum *.deb | LC_ALL=C sort -k2) > /out/apt-closure.txt\n\
         FROM scratch AS ledger\n\
         COPY --from=probe /out/apt-closure.txt /\n"
    );

    let tmp = tempfile::tempdir()?;
    fs::write(tmp.path().join("Dockerfile"), recipe)?;
    fs::copy(
        repo_root.join(APT_SOURCES_PATH),
        tmp.path().join("ubuntu.sources"),
    )?;
    let out_dir = tmp.path().join("out");
    let status = Command::new("docker")
        .args([
            "build",
            "--platform",
            "linux/amd64",
            "--no-cache",
            "--target",
            "ledger",
            "--output",
            &format!("type=local,dest={}", out_dir.display()),
        ])
        .arg(tmp.path())
        .status()?;
    ensure!(status.success(), "docker build probe failed");

    let ledger = fs::read_to_string(out_dir.join("apt-closure.txt"))?;
    let count = ledger.lines().count();
    let sha256 = hex::encode(Sha256::digest(ledger.as_bytes()));
    print!("{ledger}");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"count": count, "sha256": sha256}))?
    );

    let closure = &mut pin["apt_provenance"]["deb_closure"];
    if closure["count"].as_u64() == Some(count as u64)
        && closure["sha256"].as_str() == Some(sha256.as_str())
    {
        println!("project APT package closure: already current");
        return Ok(());
    }
    ensure!(
        write,
        "project APT package closure drifted from the pin; rerun with --write to update"
    );
    closure["count"] = json!(count);
    closure["sha256"] = json!(sha256);
    fs::write(
        &pin_path,
        format!("{}\n", serde_json::to_string_pretty(&pin)?),
    )?;
    println!("updated {CONTRACT_PATH}");
    Ok(())
}
