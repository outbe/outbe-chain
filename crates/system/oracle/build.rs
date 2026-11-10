//! Compiles the feeder sources files into the node.
//!
//! It emits one table entry per file `sources/<major>.<minor>.toml`: the protocol version from the file name, the
//! file text, the `currency_pairs` and the `gas_providers` chain ids of the
//! file. The node registers these pairs and chains, so it never parses TOML at
//! runtime. A bad file fails the build.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=sources");
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("sources");
    let mut files = versioned_files(&root);
    files.sort();
    let mut out = String::from("const SOURCES: &[Source] = &[\n");
    for (major, minor, path) in files {
        let pairs = pairs(&path);
        let gas_chains = gas_chains(&path);
        writeln!(
            out,
            "    Source {{ version: encode_protocol_version({major}, {minor}), toml: include_str!({:?}), pairs: &[{pairs}], gas_chains: &[{gas_chains}] }},",
            path.display().to_string()
        )
        .unwrap();
    }
    out.push_str("];\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("sources.rs");
    std::fs::write(dest, out).unwrap();
}

/// The `<major>.<minor>.toml` files, with their versions.
fn versioned_files(dir: &Path) -> Vec<(u8, u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .map(|path| {
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let (major, minor) = stem
                .split_once('.')
                .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
                .unwrap_or_else(|| panic!("{}: name must be <major>.<minor>.toml", path.display()));
            (major, minor, path)
        })
        .collect()
}

/// The `chain_id` of each `gas_providers` entry of a file, as a Rust list body.
fn gas_chains(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let table: toml::Table =
        toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let Some(entries) = table
        .get("gas_providers")
        .and_then(|value| value.as_array())
    else {
        return String::new();
    };
    let mut seen = Vec::new();
    for entry in entries {
        let chain_id = entry
            .get("chain_id")
            .and_then(|value| value.as_integer())
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value != 0)
            .unwrap_or_else(|| {
                panic!(
                    "{}: gas_providers entry without a valid chain_id",
                    path.display()
                )
            });
        if seen.contains(&chain_id) {
            panic!(
                "{}: gas_providers chain_id {chain_id} twice",
                path.display()
            );
        }
        seen.push(chain_id);
    }
    seen.iter()
        .map(|chain_id| format!("{chain_id}, "))
        .collect()
}

/// The `currency_pairs` of a file as `(Asset, Asset)` Rust expressions.
fn pairs(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let table: toml::Table =
        toml::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let Some(pairs) = table
        .get("currency_pairs")
        .and_then(|value| value.as_array())
    else {
        return String::new();
    };
    pairs
        .iter()
        .map(|pair| {
            let side = |key: &str| {
                let value = pair.get(key).and_then(|value| value.as_str());
                asset(value.unwrap_or_else(|| panic!("{}: pair without {key}", path.display())))
                    .unwrap_or_else(|| panic!("{}: invalid {key} {value:?}", path.display()))
            };
            format!("({}, {}), ", side("base"), side("quote"))
        })
        .collect()
}

/// `COEN` or an ISO 4217 numeric code. Token pairs are not supported yet.
fn asset(text: &str) -> Option<String> {
    if text.eq_ignore_ascii_case("COEN") {
        return Some("Asset::Coen".into());
    }
    let code = text.parse::<u16>().ok()?;
    (1..=999)
        .contains(&code)
        .then(|| format!("Asset::Iso({code})"))
}
