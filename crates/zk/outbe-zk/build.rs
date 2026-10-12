#![allow(dead_code)]
//! Read-only codegen over frozen L2 tribute artifacts in `circuits/`.
fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let circuits_dir = crate_dir.join("../../../circuits");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_path = circuits_dir.join("manifest.toml");
    println!("cargo:rerun-if-changed={}", manifest_path.display());

    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", manifest_path.display()));

    let proof_system = manifest["proof_system"]
        .as_str()
        .expect("manifest: proof_system must be a string")
        .to_string();
    let loaded = load_circuits(&manifest, &circuits_dir);
    let l2 = load_l2_circuits(&manifest, &circuits_dir, &loaded);

    let mut generated = String::new();
    generated.push_str(&generate_header());
    generated.push_str(&generate_default_types());
    generated.push_str(&generate_circuit_modules(&loaded, &proof_system));
    for package in &l2.packages {
        generated.push_str(&generate_circuit_module(&package.circuit, &proof_system));
    }
    generated.push_str(&generate_l2_registry(&l2, &proof_system));

    fs::write(out_dir.join("noir_generated.rs"), generated)
        .expect("failed to write noir_generated.rs");
}

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;
use tiny_keccak::{Hasher, Keccak};

/// Shared proving artifacts and computed identity for an L1 release or L2 package.
struct Loaded {
    module: String,
    label: String,
    // L2 uses the latest registered chain version descriptively, not as identity.
    version: String,
    artifacts: String,
    circuit_hash: [u8; 32],
    vk_hash: [u8; 32],
    vk: Vec<u8>,
    abi: Option<Value>,
    has_bytecode: bool,
}

struct Release {
    status: String,
    circuit: Loaded,
}

struct L2Package {
    path: String,
    circuit: Loaded,
}

struct L2Registry {
    packages: Vec<L2Package>,
    versions: BTreeMap<(u64, String), Vec<usize>>,
}

/// Read frozen artifacts and derive the identities needed by code generation.
fn load_circuits(manifest: &toml::Value, manifest_dir: &Path) -> Vec<Release> {
    let Some(circuits) = manifest.get("circuit").and_then(|value| value.as_array()) else {
        return Vec::new();
    };

    let mut loaded = Vec::new();
    for circuit in circuits {
        let module = circuit["module"].as_str().expect("circuit.module");
        let label = circuit["label"].as_str().expect("circuit.label");
        let versions = circuit["versions"]
            .as_array()
            .expect("circuit.versions must be an array");
        for release in versions {
            if let Some(entry) = load_release(module, label, release, manifest_dir) {
                loaded.push(entry);
            }
        }
    }

    loaded
}

fn load_release(
    module: &str,
    label: &str,
    release: &toml::Value,
    manifest_dir: &Path,
) -> Option<Release> {
    let version = release["version"]
        .as_str()
        .expect("circuit.versions.version")
        .to_string();
    let status = release["status"]
        .as_str()
        .expect("circuit.versions.status")
        .to_string();
    let manifest_hash = release.get("circuit_hash").and_then(|v| v.as_str());

    // Revoked releases retain their catalog record but no verification key.
    if status == "revoked" {
        return None;
    }

    let artifacts = format!("resources/circuits/{module}/{version}");
    Some(Release {
        status,
        circuit: load_artifacts(
            module,
            label,
            &version,
            &artifacts,
            manifest_hash,
            manifest_dir,
        ),
    })
}

fn load_artifacts(
    module: &str,
    label: &str,
    version: &str,
    artifacts: &str,
    manifest_hash: Option<&str>,
    manifest_dir: &Path,
) -> Loaded {
    let dir = manifest_dir.join(artifacts);
    let vk_path = dir.join("circuit.vk");
    let b64_path = dir.join("bytecode.b64");
    let abi_path = dir.join("abi.json");
    println!("cargo:rerun-if-changed={}", vk_path.display());
    println!("cargo:rerun-if-changed={}", b64_path.display());
    println!("cargo:rerun-if-changed={}", abi_path.display());

    // VK is always present — verification needs only it.
    let vk = fs::read(&vk_path).unwrap_or_else(|e| panic!("read {}: {e}", vk_path.display()));
    let vk_hash = keccak256(&vk);

    // Bytecode is kept only for the active version. If absent, the identity
    // is preserved in the manifest's `circuit_hash`.
    let has_bytecode = b64_path.exists();
    let circuit_hash = if has_bytecode {
        let bytecode = fs::read_to_string(&b64_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", b64_path.display()));
        let acir = base64::engine::general_purpose::STANDARD
            .decode(bytecode.trim())
            .unwrap_or_else(|e| panic!("{module}@{version}: bad base64: {e}"));
        keccak256(&acir)
    } else {
        hex_to_32(manifest_hash.unwrap_or_else(|| {
            panic!("{module}@{version}: bytecode dropped but manifest has no circuit_hash")
        }))
    };

    let abi = if abi_path.exists() {
        Some(
            serde_json::from_str(
                &fs::read_to_string(&abi_path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", abi_path.display())),
            )
            .unwrap_or_else(|e| panic!("parse {}: {e}", abi_path.display())),
        )
    } else {
        None
    };

    Loaded {
        module: module.to_string(),
        label: label.to_string(),
        version: version.to_string(),
        artifacts: artifacts.to_string(),
        circuit_hash,
        vk_hash,
        vk,
        abi,
        has_bytecode,
    }
}

/// Generated-file provenance.
fn generate_header() -> String {
    let mut generated = String::new();
    generated.push_str("// @generated by build.rs from the frozen circuit registry.\n");
    generated
        .push_str("// Source of truth: circuits/manifest.toml and committed frozen artifacts.\n");
    generated
}

/// Shared circuit types and their Alloy representation.
fn generate_default_types() -> String {
    let mut generated = String::new();
    generated.push_str("use crate::Fr;\n\n");
    generated.push_str("#[derive(Clone, Copy, Debug)]\n");
    generated.push_str("pub struct EmbeddedCurvePoint {\n    pub x: Fr,\n    pub y: Fr,\n}\n\n");
    generated.push_str(
        "#[cfg(feature = \"alloy\")]\n\
         pub mod alloy {\n\
         \x20   #[derive(Clone, Copy, Debug, PartialEq, Eq)]\n\
         \x20   pub struct EmbeddedCurvePoint {\n\
         \x20       pub x: alloy_primitives::B256,\n\
         \x20       pub y: alloy_primitives::B256,\n\
         \x20   }\n\
         }\n\n",
    );

    generated
}

/// Modules for the latest provable active version of each circuit.
fn generate_circuit_modules(loaded: &[Release], proof_system: &str) -> String {
    let mut generated = String::new();
    // Per module, the latest **active** version that is provable (has bytecode +
    // ABI) is the prover-facing "head".
    let mut head: BTreeMap<&str, &Loaded> = BTreeMap::new();
    for release in loaded {
        let l = &release.circuit;
        if release.status != "active" || l.abi.is_none() || !l.has_bytecode {
            continue;
        }
        head.entry(&l.module)
            .and_modify(|cur| {
                if version_key(&l.version) > version_key(&cur.version) {
                    *cur = l;
                }
            })
            .or_insert(l);
    }
    for l in head.values() {
        generated.push_str(&generate_circuit_module(l, proof_system));
    }

    generated
}

/// Verification registry over every non-revoked frozen version.
fn generate_circuit_registry(loaded: &[Release], proof_system: &str) -> String {
    // The in-code registry over every entry (all versions). VK-only view.
    let mut registry = String::from(
        "/// Every non-revoked frozen L1 circuit version (see `circuits/manifest.toml`).\n\
         pub static CIRCUIT_REGISTRY: &[crate::RegistryEntry] = &[\n",
    );
    for l in loaded {
        registry.push_str(&generate_registry_entry(l, proof_system));
    }
    registry.push_str("];\n");
    registry
}

/// Load each L2 package once, checking every chain-version pin against its VK.
fn load_l2_circuits(manifest: &toml::Value, manifest_dir: &Path, l1: &[Release]) -> L2Registry {
    let chains = manifest.get("l2_chain").map_or(&[][..], |value| {
        value
            .as_array()
            .expect("manifest: l2_chain must be an array of tables")
            .as_slice()
    });
    let mut registry = L2Registry {
        packages: Vec::new(),
        versions: BTreeMap::new(),
    };
    let mut chain_ids = BTreeSet::new();
    let mut package_indices = BTreeMap::<String, usize>::new();
    let mut modules: BTreeMap<String, String> = l1
        .iter()
        .map(|release| {
            (
                release.circuit.module.clone(),
                release.circuit.artifacts.clone(),
            )
        })
        .collect();
    for chain in chains {
        let chain_id = chain
            .get("chain_id")
            .and_then(|value| value.as_integer())
            .and_then(|id| u64::try_from(id).ok())
            .expect("l2_chain.chain_id must be a non-negative integer");
        assert!(chain_ids.insert(chain_id), "duplicate L2 chain {chain_id}");
        let versions = chain
            .get("versions")
            .and_then(|value| value.as_array())
            .expect("l2_chain.versions must be an array");
        for version in versions {
            let name = version
                .get("selector")
                .and_then(|value| value.as_str())
                .filter(|name| !name.trim().is_empty())
                .expect("l2_chain.versions.selector must be a non-empty string");
            let key = (chain_id, name.to_string());
            assert!(
                !registry.versions.contains_key(&key),
                "L2 chain {chain_id}: duplicate version {name:?}"
            );
            version
                .get("stable")
                .and_then(|value| value.as_bool())
                .expect("l2_chain.versions.stable must be a boolean");
            let circuits = version
                .get("circuits")
                .and_then(|value| value.as_array())
                .expect("l2_chain.versions.circuits must be an array");
            let mut paths = BTreeSet::new();
            let mut indices = Vec::with_capacity(circuits.len());
            for circuit in circuits {
                let path = circuit
                    .get("path")
                    .and_then(|value| value.as_str())
                    .expect("L2 circuit.path must be a string");
                assert!(
                    !path.contains(['\\', ':'])
                        && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".."),
                    "L2 chain {chain_id}: package path {path:?} must be relative and cannot escape its chain"
                );
                assert!(
                    paths.insert(path),
                    "L2 chain {chain_id} version {name:?}: duplicate package path {path:?}"
                );
                let pin = circuit
                    .get("vk_hash")
                    .and_then(|value| value.as_str())
                    .expect("L2 circuit.vk_hash must be a string");
                assert!(
                    pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "L2 chain {chain_id} package {path:?}: vk_hash must be 64 lowercase hex characters"
                );
                let vk_hash = hex_to_32(pin);
                let artifacts = format!("l2/{chain_id}/{path}");
                let index = if let Some(&index) = package_indices.get(&artifacts) {
                    let package = &mut registry.packages[index];
                    assert_eq!(
                        package.circuit.vk_hash, vk_hash,
                        "L2 package {artifacts}: conflicting vk_hash pins"
                    );
                    if version_key(name) > version_key(&package.circuit.version) {
                        package.circuit.version = name.to_string();
                    }
                    index
                } else {
                    let chain_root = manifest_dir.join(format!("l2/{chain_id}"));
                    let chain_root = fs::canonicalize(&chain_root)
                        .unwrap_or_else(|e| panic!("resolve {}: {e}", chain_root.display()));
                    let root = manifest_dir.join(&artifacts);
                    let resolved = fs::canonicalize(&root)
                        .unwrap_or_else(|e| panic!("resolve {}: {e}", root.display()));
                    assert!(
                        resolved.starts_with(&chain_root) && resolved != chain_root,
                        "L2 package {artifacts}: path escapes its chain"
                    );
                    let nargo_path = root.join("Nargo.toml");
                    println!("cargo:rerun-if-changed={}", nargo_path.display());
                    let nargo: toml::Value = toml::from_str(
                        &fs::read_to_string(&nargo_path)
                            .unwrap_or_else(|e| panic!("read {}: {e}", nargo_path.display())),
                    )
                    .unwrap_or_else(|e| panic!("parse {}: {e}", nargo_path.display()));
                    let module = nargo["package"]["name"]
                        .as_str()
                        .expect("Nargo package.name");
                    if let Some(previous) = modules.insert(module.to_string(), artifacts.clone()) {
                        panic!("duplicate circuit module {module:?}: {previous} and {artifacts}");
                    }
                    let label = module.replace('_', ".");
                    let loaded =
                        load_artifacts(module, &label, name, &artifacts, None, manifest_dir);
                    assert!(
                        loaded.has_bytecode && loaded.abi.is_some(),
                        "L2 package {artifacts}: bytecode.b64 and abi.json are required"
                    );
                    assert_eq!(
                        loaded.vk_hash, vk_hash,
                        "L2 package {artifacts}: stored circuit.vk does not match pinned vk_hash"
                    );
                    let index = registry.packages.len();
                    registry.packages.push(L2Package {
                        path: path.to_string(),
                        circuit: loaded,
                    });
                    package_indices.insert(artifacts, index);
                    index
                };
                indices.push(index);
            }
            registry.versions.insert(key, indices);
        }
    }
    registry
}

/// Chain selections the node looks up by offerTribute version.
fn generate_l2_registry(registry: &L2Registry, proof_system: &str) -> String {
    let mut chains: Vec<(u64, Vec<(&str, usize)>)> = Vec::new();
    for ((chain_id, selector), indices) in &registry.versions {
        if chains.last().is_none_or(|(id, _)| id != chain_id) {
            chains.push((*chain_id, Vec::new()));
        }
        for index in indices {
            chains.last_mut().unwrap().1.push((selector, *index));
        }
    }
    let mut generated =
        String::from("pub static L2_CIRCUITS_REGISTRY: &[crate::L2ChainEntry] = &[\n");
    for (chain_id, circuits) in chains {
        generated.push_str(&format!(
            "    crate::L2ChainEntry {{ chain_id: {chain_id}, circuits: &[\n"
        ));
        for (selector, index) in circuits {
            let package = &registry.packages[index];
            let vk_path = format!(
                "/../../../circuits/{}/circuit.vk",
                package.circuit.artifacts
            );
            generated.push_str(&format!(
                "        crate::L2CircuitVersion {{\n\
                 \x20           version: {selector:?},\n\
                 \x20           circuit_hash: {ch},\n\
                 \x20           vk_hash: {vh},\n\
                 \x20           vk_bytes: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {vk_path:?})),\n\
                 \x20           label: {label:?},\n\
                 \x20           proof_system: {proof_system:?},\n\
                 \x20       }},\n",
                ch = hex_lit(&package.circuit.circuit_hash),
                vh = hex_lit(&package.circuit.vk_hash),
                label = package.circuit.label,
            ));
        }
        generated.push_str("    ] },\n");
    }
    generated.push_str("];\n");
    generated
}

/// keccak256 (Ethereum variant), used for both `circuit_hash` and `vk_hash`.
fn keccak256(input: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    hasher.update(input);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    out
}

/// Parse a `"a.b.c"` semver into a comparable tuple (missing parts = 0).
fn version_key(v: &str) -> (u64, u64, u64) {
    let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// 64-char (optionally `0x`-prefixed) hex string -> `[u8; 32]`.
fn hex_to_32(s: &str) -> [u8; 32] {
    let s = s.strip_prefix("0x").unwrap_or(s);
    assert_eq!(
        s.len(),
        64,
        "circuit_hash must be 64 hex chars, got {}",
        s.len()
    );
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("valid hex");
    }
    out
}

/// Lower-case hex of a digest (no `0x`).
fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A `hex!("..")` literal (`hex-literal` crate) for a 32-byte digest — readable,
/// compile-time, `[u8; 32]`.
fn hex_lit(bytes: &[u8; 32]) -> String {
    format!("::hex_literal::hex!({:?})", hex_str(bytes))
}

/// Marker (unit) type name: `demo_tribute` -> `DemoTribute`.
fn marker_ident(module: &str) -> String {
    module
        .split('_')
        .map(|seg| {
            let mut c = seg.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// One `crate::RegistryEntry { … }` literal (verification view: VK only).
fn generate_registry_entry(release: &Release, proof_system: &str) -> String {
    let l = &release.circuit;
    let status = match release.status.as_str() {
        "active" => "Active",
        "deprecated" => "Deprecated",
        "revoked" => "Revoked",
        other => panic!("{}: unknown status {other:?}", l.module),
    };
    let v = &l.version;
    let vk_path = format!("/{}/circuit.vk", l.artifacts);
    format!(
        "    crate::RegistryEntry {{\n\
         \x20       label: {label:?},\n\
         \x20       version: {v:?},\n\
         \x20       status: crate::CircuitStatus::{status},\n\
         \x20       proof_system: {proof_system:?},\n\
         \x20       circuit_hash: {ch},\n\
         \x20       vk_hash: {vh},\n\
         \x20       vk_bytes: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {vk_path:?})),\n\
         \x20   }},\n",
        label = l.label,
        ch = hex_lit(&l.circuit_hash),
        vh = hex_lit(&l.vk_hash),
    )
}

/// One circuit module, ordered as declarations, implementations, then functions.
fn generate_circuit_module(l: &Loaded, proof_system: &str) -> String {
    let module = &l.module;
    let params = l.abi.as_ref().expect("head module has ABI")["parameters"]
        .as_array()
        .unwrap_or_else(|| panic!("{module}@{}: abi.parameters is not an array", l.version));
    let mut generated = format!("pub mod {module} {{\n");
    generated.push_str(&generate_circuit_imports(params, module));
    generated.push_str(&generate_circuit_constants(l, params, proof_system));
    generated.push_str(&generate_circuit_struct(
        params, module, "Witness", "private",
    ));
    generated.push_str(&generate_circuit_struct(
        params,
        module,
        "PublicInputs",
        "public",
    ));
    generated.push_str(&generate_circuit_marker(l));
    generated.push_str(&generate_circuit_impl(params, module));
    generated.push_str(&generate_circuit_id_impl(module));
    generated.push_str(&generate_public_decoder(params, module));
    generated.push_str(&generate_combined_proof_encoder(module));
    generated.push_str(&generate_alloy_module(params, module));
    generated.push_str("}\n\n");
    generated
}

/// Imports required by the circuit's ABI types.
fn generate_circuit_imports(params: &[Value], module: &str) -> String {
    let mut generated = String::new();
    if params
        .iter()
        .any(|p| rust_type(&p["type"], module).contains("EmbeddedCurvePoint"))
    {
        generated.push_str("    use super::EmbeddedCurvePoint;\n");
    }
    generated.push_str("    use crate::Fr;\n\n");
    generated
}

/// Proof layout and frozen circuit identity constants.
fn generate_circuit_constants(l: &Loaded, params: &[Value], proof_system: &str) -> String {
    let b64_path = format!("/../../../circuits/{}/bytecode.b64", l.artifacts);
    let vk_path = format!("/../../../circuits/{}/circuit.vk", l.artifacts);
    let version = &l.version;
    let mut generated = String::new();
    let public_input_count: usize = params
        .iter()
        .filter(|p| p["visibility"] == "public")
        .map(|p| abi_field_count(&p["type"]))
        .sum();
    let proof_words = proof_words(&l.vk, public_input_count, proof_system);
    generated.push_str(&format!(
        "    /// Number of public field words in the frozen ABI.\n\
         \x20   pub const PUBLIC_INPUT_COUNT: usize = {public_input_count};\n\
         \x20   /// ZK proof words for the pinned proof system and frozen circuit size.\n\
         \x20   pub const PROOF_WORDS: usize = {proof_words};\n\
         \x20   /// Combined proof: four-byte count, public inputs, then proof words.\n\
         \x20   pub const COMBINED_LEN: usize = 4 + (PUBLIC_INPUT_COUNT + PROOF_WORDS) * 32;\n\n"
    ));

    generated.push_str("    /// Canonical dotted label (matches `outbe-zk-canonical`).\n");
    generated.push_str(&format!("    pub const LABEL: &str = {:?};\n", l.label));
    generated.push_str("    /// Semver-style version string. Not authoritative.\n");
    generated.push_str(&format!("    pub const VERSION: &str = {version:?};\n"));
    generated
        .push_str("    /// `keccak256(base64_decode(bytecode))` — proving bytecode identity.\n");
    generated.push_str(&format!(
        "    pub const CIRCUIT_HASH: [u8; 32] = {};\n",
        hex_lit(&l.circuit_hash)
    ));
    generated.push_str("    /// Base64-encoded ACIR bytecode (frozen; the proving artifact).\n");
    generated.push_str(&format!(
        "    pub const BYTECODE_B64: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {b64_path:?}));\n"
    ));
    generated.push_str("    /// UltraHonkKeccak verification key (frozen).\n");
    generated.push_str(&format!(
        "    pub const VK_BYTES: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {vk_path:?}));\n"
    ));
    generated.push_str("    /// `keccak256(VK_BYTES)`.\n");
    generated.push_str(&format!(
        "    pub const VK_HASH: [u8; 32] = {};\n\n",
        hex_lit(&l.vk_hash)
    ));

    generated
}

/// One circuit input struct, with fields in ABI order.
fn generate_circuit_struct(params: &[Value], module: &str, name: &str, visibility: &str) -> String {
    let (doc, derives) = match visibility {
        "private" => ("Private (witness) inputs, in ABI order.", "Clone, Debug"),
        "public" => (
            "Public inputs, in ABI order.",
            "Clone, Debug, PartialEq, Eq",
        ),
        other => panic!("unexpected visibility {other:?}"),
    };
    let mut generated =
        format!("    /// {doc}\n    #[derive({derives})]\n    pub struct {name} {{\n");
    for p in params.iter().filter(|p| p["visibility"] == visibility) {
        let field = p["name"].as_str().expect("ABI parameter name");
        let ty = rust_type(&p["type"], module);
        generated.push_str(&format!("        pub {field}: {ty},\n"));
    }
    generated.push_str("    }\n\n");
    generated
}

/// The circuit's unit marker type.
fn generate_circuit_marker(l: &Loaded) -> String {
    let marker = marker_ident(&l.module);
    let mut generated = String::new();
    generated.push_str(&format!(
        "    /// Canonical marker type for the `{}` circuit.\n",
        l.label
    ));
    generated.push_str(&format!("    pub struct {marker};\n\n"));

    generated
}

/// Circuit implementation linking the generated structs to ABI field order.
fn generate_circuit_impl(params: &[Value], module: &str) -> String {
    let marker = marker_ident(module);
    let flatten = generate_public_input_fields(params);
    let abi_flat = generate_witness_input_fields(params, module);
    let mut generated = String::new();
    generated.push_str("    impl crate::protocol::zk::Circuit for ");
    generated.push_str(&format!("{marker} {{\n"));
    generated.push_str("        type Witness = Witness;\n");
    generated.push_str("        type PublicInputs = PublicInputs;\n");
    generated.push_str("        fn public_inputs(p: &PublicInputs) -> Vec<Fr> {\n");
    generated.push_str(&flatten);
    generated.push_str("        }\n");
    generated.push_str("        fn witness_inputs(w: &Witness, p: &PublicInputs) -> Vec<Fr> {\n");
    generated.push_str(&abi_flat);
    generated.push_str("        }\n");
    generated.push_str("    }\n\n");

    generated
}

/// Public-input field flattening body for Circuit::public_inputs.
fn generate_public_input_fields(params: &[Value]) -> String {
    let mut flatten = String::from("            let mut v: Vec<Fr> = Vec::new();\n");
    for p in params {
        if p["visibility"].as_str() != Some("public") {
            continue;
        }
        let name = p["name"].as_str().unwrap();
        flatten.push_str(&generate_abi_leaf(
            &p["type"],
            &format!("p.{name}"),
            "            ",
            0,
        ));
    }
    flatten.push_str("            v\n");

    flatten
}

/// Full ABI field flattening body for Circuit::witness_inputs.
fn generate_witness_input_fields(params: &[Value], module: &str) -> String {
    let mut abi_flat = String::from("            let mut v: Vec<Fr> = Vec::new();\n");
    for p in params {
        let name = p["name"].as_str().unwrap();
        let src = match p["visibility"].as_str() {
            Some("private") => "w",
            Some("public") => "p",
            other => panic!("{module}: param {name} has unexpected visibility {other:?}"),
        };
        abi_flat.push_str(&generate_abi_leaf(
            &p["type"],
            &format!("{src}.{name}"),
            "            ",
            0,
        ));
    }
    abi_flat.push_str("            v\n");

    abi_flat
}

/// CircuitId implementation referencing the frozen identity constants.
fn generate_circuit_id_impl(module: &str) -> String {
    let marker = marker_ident(module);
    let mut generated = String::new();
    generated.push_str(&format!("    impl crate::CircuitId for {marker} {{\n"));
    generated.push_str("        const LABEL: &'static str = LABEL;\n");
    generated.push_str("        const VERSION: &'static str = VERSION;\n");
    generated.push_str("        const CIRCUIT_HASH: [u8; 32] = CIRCUIT_HASH;\n");
    generated.push_str("        const BYTECODE_B64: &'static str = BYTECODE_B64;\n");
    generated.push_str("        const VK_BYTES: &'static [u8] = VK_BYTES;\n");
    generated.push_str("        const VK_HASH: [u8; 32] = VK_HASH;\n");
    generated.push_str("    }\n");

    generated
}

/// Count ABI leaves, including array elements and transparent struct fields.
fn abi_field_count(ty: &Value) -> usize {
    match ty["kind"].as_str() {
        Some("field" | "integer" | "boolean") => 1,
        Some("array") => {
            ty["length"].as_u64().expect("array length") as usize * abi_field_count(&ty["type"])
        }
        Some("struct") => ty["fields"]
            .as_array()
            .expect("struct fields")
            .iter()
            .map(|f| abi_field_count(&f["type"]))
            .sum(),
        _ => panic!("unsupported ABI type: {ty}"),
    }
}

/// UltraKeccakZK, bb 5.0.0-nightly.20260522 (U256Codec, no round padding).
/// Upstream at 87aa5283d5b0cab6632ed49fdd25f6548d2ba91f:
/// barretenberg/cpp/src/barretenberg/{honk/proof_length.hpp,flavor/flavor.hpp}.
fn proof_words(vk: &[u8], public_input_count: usize, proof_system: &str) -> usize {
    assert_eq!(proof_system, "bb-keccak-v1", "unsupported proof layout");
    // Three metadata words followed by 28 two-word commitments.
    assert_eq!(vk.len(), (3 + 28 * 2) * 32, "unexpected Keccak VK size");
    let header = |index: usize| {
        let word = &vk[index * 32..(index + 1) * 32];
        assert!(word[..28].iter().all(|&b| b == 0), "invalid VK header");
        u32::from_be_bytes(word[28..].try_into().unwrap()) as usize
    };
    let log_n = header(0);
    assert!((1..=28).contains(&log_n), "invalid VK log circuit size");
    // The API proof retains the eight DefaultIO pairing-accumulator words.
    assert_eq!(header(1), public_input_count + 8, "VK/ABI input mismatch");
    // DefaultIO (8) + Oink (18) + Sumcheck (9*log_n + 50)
    // + Shplemini (3*log_n + 6), including ZK masking data.
    82 + 12 * log_n
}

/// The feature-independent public-input decoder.
fn generate_public_decoder(params: &[Value], module: &str) -> String {
    let mut decoded = String::new();
    let mut index = 0;
    for p in params.iter().filter(|p| p["visibility"] == "public") {
        let name = p["name"].as_str().expect("ABI parameter name");
        let ty = &p["type"];
        let value = match ty["kind"].as_str() {
            Some("field") => format!("fields[{index}]"),
            Some("array") if ty["type"]["kind"] == "field" => {
                format!("std::array::from_fn(|i| fields[{index} + i])")
            }
            Some("integer") if ty["sign"] == "unsigned" => {
                format!("{}::from_field(&fields[{index}])?", rust_type(ty, module))
            }
            Some("struct") => match ty["path"].as_str() {
                Some("outbe_circuit_core::types::EthAddress") => format!(
                    r#"{{
                if words[{index}][..12].iter().any(|&byte| byte != 0) {{
                    return Err(crate::Error::NonCanonical("address"));
                }}
                fields[{index}]
            }}"#
                ),
                Some("bignum::fields::U256::U256") => {
                    let limbs = (index..index + 3)
                        .map(|i| format!("u128::from_field(&fields[{i}])?"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        r#"{{
                let limbs = [{limbs}];
                crate::codec::u256_from_limbs_be(limbs)
                    .ok_or(crate::Error::NonCanonical("uint256 limbs"))?;
                limbs
            }}"#
                    )
                }
                _ => panic!("{module}: unsupported public ABI type: {ty}"),
            },
            _ => panic!("{module}: unsupported public ABI type: {ty}"),
        };
        decoded.push_str(&format!("            {name}: {value},\n"));
        index += abi_field_count(ty);
    }
    let field_element = if decoded.contains("::from_field") {
        "        use crate::FieldElement;\n"
    } else {
        ""
    };
    format!(
        r#"
    /// Decode public inputs from the canonical combined-proof encoding.
    pub fn decode_public_inputs(combined_proof: &[u8]) -> Result<PublicInputs, crate::Error> {{
        use crate::codec;
{field_element}
        let words = crate::wire::decode_public_words::<PUBLIC_INPUT_COUNT>(combined_proof, COMBINED_LEN)?;
        let fields = words.map(|word| codec::field_from_be32(&word));
        Ok(PublicInputs {{
{decoded}        }})
    }}
"#
    )
}

/// The feature-independent combined-proof encoder.
fn generate_combined_proof_encoder(module: &str) -> String {
    let marker = marker_ident(module);
    format!(
        r#"
    /// Encode the public-input count (big-endian u32), ABI-ordered public
    /// words, and proof words. Each proof word must contain exactly 32 bytes.
    pub fn encode_combined_proof(public_inputs: PublicInputs, proof: Vec<Vec<u8>>) -> Result<Vec<u8>, crate::Error> {{
        use crate::{{protocol::zk::Circuit, codec}};
        if proof.len() != PROOF_WORDS {{
            return Err(crate::Error::Proof(format!(
                "proof has {{}} words, expected {{}}", proof.len(), PROOF_WORDS
            )));
        }}
        let mut combined = Vec::with_capacity(COMBINED_LEN);
        combined.extend_from_slice(&(PUBLIC_INPUT_COUNT as u32).to_be_bytes());
        for field in <{marker} as Circuit>::public_inputs(&public_inputs) {{
            combined.extend_from_slice(&codec::field_to_be_bytes(&field));
        }}
        for (index, word) in proof.into_iter().enumerate() {{
            if word.len() != 32 {{
                return Err(crate::Error::Proof(format!(
                    "proof word {{index}} has {{}} bytes, expected 32", word.len()
                )));
            }}
            combined.extend_from_slice(&word);
        }}
        // Apply the same semantic address and limb bounds as decoding.
        decode_public_inputs(&combined)?;
        Ok(combined)
    }}
"#
    )
}

/// Alloy module: declarations first, then checked conversions in both directions.
fn generate_alloy_module(params: &[Value], module: &str) -> String {
    let mut types = String::new();
    let mut implementations = String::new();
    for (name, visibility) in [("PublicInputs", "public"), ("Witness", "private")] {
        types.push_str(&generate_alloy_struct(params, module, name, visibility));
        implementations.push_str(&generate_alloy_to_circuit_impl(
            params, module, name, visibility,
        ));
        implementations.push_str(&generate_circuit_to_alloy_impl(
            params, module, name, visibility,
        ));
    }
    let imports = generate_alloy_imports(&types);
    format!("\n    #[cfg(feature = \"alloy\")]\n    pub mod alloy {{\n{imports}\n{types}{implementations}    }}\n")
}

/// Imports used by the generated Alloy structs.
fn generate_alloy_imports(types: &str) -> String {
    let mut generated = String::new();
    for ty in ["Address", "B256", "U256"] {
        if types.contains(&format!(": {ty}")) || types.contains(&format!("[{ty};")) {
            generated.push_str(&format!("        use alloy_primitives::{ty};\n"));
        }
    }
    generated
}

/// One Alloy input struct, preserving the ABI's semantic types.
fn generate_alloy_struct(params: &[Value], module: &str, name: &str, visibility: &str) -> String {
    let derives = if visibility == "public" {
        "Clone, Copy, Debug, PartialEq, Eq"
    } else {
        "Clone, Debug"
    };
    let mut generated = format!("        #[derive({derives})]\n        pub struct {name} {{\n");
    for p in params.iter().filter(|p| p["visibility"] == visibility) {
        let field = p["name"].as_str().expect("ABI parameter name");
        let ty = alloy_type(&p["type"], module);
        generated.push_str(&format!("            pub {field}: {ty},\n"));
    }
    generated.push_str("        }\n\n");
    generated
}

/// Checked conversion from a circuit struct to its Alloy representation.
fn generate_circuit_to_alloy_impl(
    params: &[Value],
    module: &str,
    name: &str,
    visibility: &str,
) -> String {
    let mut fields = String::new();
    for p in params.iter().filter(|p| p["visibility"] == visibility) {
        let field = p["name"].as_str().expect("ABI parameter name");
        let value = circuit_to_alloy(&p["type"], &format!("value.{field}"), module);
        fields.push_str(&format!("                    {field}: {value},\n"));
    }
    format!(
        r#"
        impl TryFrom<super::{name}> for {name} {{
            type Error = crate::Error;

            /// Convert circuit inputs to Alloy, checking address and U256 bounds.
            fn try_from(value: super::{name}) -> Result<Self, Self::Error> {{
                Ok(Self {{
{fields}                }})
            }}
        }}
"#
    )
}

/// Reverse the ABI mapping without reducing addresses or truncating limbs.
fn circuit_to_alloy(ty: &Value, expr: &str, module: &str) -> String {
    match ty["kind"].as_str() {
        Some("field") => format!("crate::codec::field_to_b256(&{expr})?"),
        Some("array") if alloy_type(ty, module) != rust_type(ty, module) => {
            let inner = circuit_to_alloy(&ty["type"], "item", module);
            format!(
                "{expr}.map(|item| -> Result<_, crate::Error> {{ Ok({inner}) }})\n\
                 .into_iter().collect::<Result<Vec<_>, _>>()?.try_into().expect(\"ABI array length unchanged\")"
            )
        }
        Some("struct") => match ty["path"].as_str() {
            Some("std::embedded_curve_ops::EmbeddedCurvePoint") => {
                let fields = ty["fields"].as_array().expect("point fields");
                let members: Vec<_> = fields.iter().map(|f| {
                    let name = f["name"].as_str().expect("point field name");
                    let value = circuit_to_alloy(&f["type"], &format!("{expr}.{name}"), module);
                    format!("{name}: {value}")
                }).collect();
                format!("crate::noir::alloy::EmbeddedCurvePoint {{ {} }}", members.join(", "))
            }
            Some("outbe_circuit_core::types::EthAddress") => format!(
                "<Address as crate::FieldElement>::from_field(&{expr})?"
            ),
            Some("bignum::fields::U256::U256") => format!(
                "U256::from_be_bytes(crate::codec::u256_from_limbs_be({expr}).ok_or(crate::Error::NonCanonical(\"uint256 limbs\"))?)"
            ),
            _ => {
                let fields = ty["fields"].as_array().expect("struct fields");
                match fields.as_slice() {
                    [only] => circuit_to_alloy(&only["type"], expr, module),
                    _ => panic!("{module}: unsupported Alloy ABI type: {ty}"),
                }
            }
        },
        _ => expr.to_string(),
    }
}

/// TryFrom supplies TryInto automatically and rejects noncanonical field words.
fn generate_alloy_to_circuit_impl(
    params: &[Value],
    module: &str,
    name: &str,
    visibility: &str,
) -> String {
    let mut fields = String::new();
    for p in params.iter().filter(|p| p["visibility"] == visibility) {
        let target = p["name"].as_str().expect("ABI parameter name");
        let value = alloy_to_circuit(&p["type"], &format!("value.{target}"), module);
        fields.push_str(&format!("                    {target}: {value},\n"));
    }
    format!(
        r#"
        impl TryFrom<{name}> for super::{name} {{
            type Error = crate::error::Error;

            /// Convert to circuit inputs, rejecting field words at or above the modulus.
            fn try_from(value: {name}) -> Result<Self, Self::Error> {{
                Ok(Self {{
{fields}                }})
            }}
        }}
"#
    )
}

/// Convert recursively using the protocol's canonical field and U256 codecs.
fn alloy_to_circuit(ty: &Value, expr: &str, module: &str) -> String {
    match ty["kind"].as_str() {
        Some("field") => {
            format!("crate::codec::field_from_b256(&{expr})?")
        }
        Some("array") if alloy_type(ty, module) != rust_type(ty, module) => {
            let inner = alloy_to_circuit(&ty["type"], "item", module);
            format!(
                "{expr}.map(|item| -> Result<_, crate::error::Error> {{ Ok({inner}) }})\n\
                 .into_iter().collect::<Result<Vec<_>, _>>()?.try_into().expect(\"ABI array length unchanged\")"
            )
        }
        Some("struct") => match ty["path"].as_str() {
            Some("std::embedded_curve_ops::EmbeddedCurvePoint") => {
                let fields = ty["fields"].as_array().expect("point fields");
                let members: Vec<_> = fields
                    .iter()
                    .map(|f| {
                        let name = f["name"].as_str().expect("point field name");
                        let value = alloy_to_circuit(&f["type"], &format!("{expr}.{name}"), module);
                        format!("{name}: {value}")
                    })
                    .collect();
                format!(
                    "crate::noir::EmbeddedCurvePoint {{ {} }}",
                    members.join(", ")
                )
            }
            Some("outbe_circuit_core::types::EthAddress") => {
                format!("crate::FieldElement::to_field(&{expr})?")
            }
            Some("bignum::fields::U256::U256") => {
                format!("crate::codec::u256_limbs_be(&{expr}.to_be_bytes::<32>())")
            }
            _ => {
                let fields = ty["fields"].as_array().expect("struct fields");
                match fields.as_slice() {
                    [only] => alloy_to_circuit(&only["type"], expr, module),
                    _ => panic!("{module}: unsupported Alloy ABI type: {ty}"),
                }
            }
        },
        _ => expr.to_string(),
    }
}

/// Preserve the same ABI shape, replacing field words and semantic Noir types.
fn alloy_type(ty: &Value, module: &str) -> String {
    match ty["kind"].as_str() {
        Some("field") => "B256".to_string(),
        Some("array") => format!(
            "[{}; {}]",
            alloy_type(&ty["type"], module),
            ty["length"].as_u64().expect("array length")
        ),
        Some("struct") => match ty["path"].as_str() {
            Some("std::embedded_curve_ops::EmbeddedCurvePoint") => {
                "crate::noir::alloy::EmbeddedCurvePoint".to_string()
            }
            Some("outbe_circuit_core::types::EthAddress") => "Address".to_string(),
            Some("bignum::fields::U256::U256") => "U256".to_string(),
            _ => {
                let fields = ty["fields"].as_array().expect("struct fields");
                match fields.as_slice() {
                    [only] => alloy_type(&only["type"], module),
                    _ => panic!("{module}: unsupported Alloy ABI type: {ty}"),
                }
            }
        },
        _ => rust_type(ty, module),
    }
}

/// Statements appending one ABI parameter to the running field vector.
fn generate_abi_leaf(ty: &Value, expr: &str, indent: &str, depth: usize) -> String {
    match ty["kind"].as_str() {
        Some("field") => format!("{indent}v.push({expr});\n"),
        Some("integer") => {
            let repr = match ty["sign"].as_str().unwrap_or("unsigned") {
                "unsigned" => format!("{expr} as u128"),
                "signed" => format!("{expr} as u64"),
                other => panic!("unexpected integer sign {other}"),
            };
            format!("{indent}v.push(Fr::from({repr}));\n")
        }
        Some("boolean") => format!("{indent}v.push(Fr::from({expr} as u64));\n"),
        Some("array") => {
            let var = format!("__e{depth}");
            let body = generate_abi_leaf(&ty["type"], &var, &format!("{indent}    "), depth + 1);
            format!("{indent}for {var} in {expr}.iter().copied() {{\n{body}{indent}}}\n")
        }
        // Walk declared fields in ABI order; single-field newtypes are transparent.
        Some("struct") => {
            let fields = ty["fields"].as_array().expect("struct ABI fields");
            match fields.as_slice() {
                [only] => generate_abi_leaf(&only["type"], expr, indent, depth + 1),
                _ => fields
                    .iter()
                    .map(|f| {
                        let name = f["name"].as_str().expect("struct field name");
                        generate_abi_leaf(&f["type"], &format!("{expr}.{name}"), indent, depth + 1)
                    })
                    .collect(),
            }
        }
        other => panic!("witness_inputs: unsupported ABI kind {other:?}"),
    }
}

/// Map a noir ABI type to a Rust type string.
fn rust_type(ty: &Value, module: &str) -> String {
    match ty["kind"].as_str() {
        Some("field") => "Fr".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("integer") => {
            let sign = ty["sign"].as_str().unwrap_or("unsigned");
            let width = ty["width"].as_u64().expect("integer width");
            match sign {
                "unsigned" => format!("u{width}"),
                "signed" => format!("i{width}"),
                other => panic!("{module}: unexpected integer sign {other}"),
            }
        }
        Some("array") => {
            let len = ty["length"].as_u64().expect("array length");
            let inner = rust_type(&ty["type"], module);
            format!("[{inner}; {len}]")
        }
        Some("struct") => {
            let path = ty["path"].as_str().unwrap_or("");
            if path == "std::embedded_curve_ops::EmbeddedCurvePoint" {
                return "EmbeddedCurvePoint".to_string();
            }
            // A single-field Noir struct is a newtype (`EthAddress { inner:
            // Field }`); it carries one ABI leaf, so it is transparent at this
            // boundary and maps to its inner Rust type. The type only exists to
            // constrain the *circuit* -- `EthAddress::validate` enforces the
            // 160-bit bound in-circuit, which no Rust-side wrapper could.
            let fields = ty["fields"].as_array().expect("struct ABI fields");
            match fields.as_slice() {
                [only] => rust_type(&only["type"], module),
                _ => panic!("{module}: unsupported struct ABI type {path}"),
            }
        }
        other => panic!("{module}: unsupported ABI kind {other:?}"),
    }
}
