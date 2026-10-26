//! Host-side Intel DCAP collateral capture for one real SGX v3 quote.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use eyre::{bail, ensure, eyre, Result, WrapErr as _};
use serde_json::{json, Value};

use super::{sha256_file, unix_seconds};

const PCK_CRL_DER: &str = "pck.crl.der";
const ROOT_CA_CRL_DER: &str = "root-ca.crl.der";

#[repr(C)]
#[derive(Clone, Copy)]
struct CollateralVersionParts {
    major_version: u16,
    minor_version: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
union CollateralVersion {
    version: u32,
    parts: CollateralVersionParts,
}

#[repr(C)]
struct QuoteCollateral {
    version: CollateralVersion,
    tee_type: u32,
    pck_crl_issuer_chain: *const u8,
    pck_crl_issuer_chain_size: u32,
    root_ca_crl: *const u8,
    root_ca_crl_size: u32,
    pck_crl: *const u8,
    pck_crl_size: u32,
    tcb_info_issuer_chain: *const u8,
    tcb_info_issuer_chain_size: u32,
    tcb_info: *const u8,
    tcb_info_size: u32,
    qe_identity_issuer_chain: *const u8,
    qe_identity_issuer_chain_size: u32,
    qe_identity: *const u8,
    qe_identity_size: u32,
}

type TeeQvGetCollateralFn = unsafe extern "C" fn(
    p_quote: *const u8,
    quote_size: u32,
    pp_quote_collateral: *mut *mut QuoteCollateral,
    p_collateral_size: *mut u32,
) -> u32;

type TeeQvFreeCollateralFn = unsafe extern "C" fn(p_quote_collateral: *mut QuoteCollateral) -> u32;

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Return the exact type-5 certification data from a complete SGX quote v3.
pub(crate) fn extract_type5_pck_chain(quote: &[u8]) -> Result<Vec<u8>> {
    let fixed_quote_size = 436usize;
    let fixed_authentication_size = 64 + 64 + 384 + 64;
    ensure!(
        quote.len() >= fixed_quote_size + fixed_authentication_size + 8,
        "quote is shorter than the SGX v3 authentication structure"
    );
    let version = u16::from_le_bytes([quote[0], quote[1]]);
    ensure!(version == 3, "quote version is not SGX quote v3");
    let tee_type = u32::from_le_bytes([quote[4], quote[5], quote[6], quote[7]]);
    ensure!(tee_type == 0, "quote tee_type is not SGX");

    let authentication_size =
        u32::from_le_bytes([quote[432], quote[433], quote[434], quote[435]]) as usize;
    ensure!(
        fixed_quote_size + authentication_size == quote.len(),
        "quote has trailing or truncated authentication bytes"
    );

    let mut cursor = fixed_quote_size + fixed_authentication_size;
    let qe_authentication_size = u16::from_le_bytes([quote[cursor], quote[cursor + 1]]) as usize;
    cursor += 2 + qe_authentication_size;
    ensure!(
        cursor + 6 <= quote.len(),
        "quote certification-data header is truncated"
    );
    let certification_data_type = u16::from_le_bytes([quote[cursor], quote[cursor + 1]]);
    let certification_data_size = u32::from_le_bytes([
        quote[cursor + 2],
        quote[cursor + 3],
        quote[cursor + 4],
        quote[cursor + 5],
    ]) as usize;
    cursor += 6;
    let end = cursor + certification_data_size;
    ensure!(
        certification_data_type == 5,
        "quote certification data is not type 5"
    );
    ensure!(
        end == quote.len(),
        "quote has trailing or truncated certification data"
    );

    let pck_chain = &quote[cursor..end];
    ensure!(
        pck_chain.starts_with(b"-----BEGIN CERTIFICATE-----\n")
            && pck_chain.ends_with(b"\0")
            && !pck_chain[..pck_chain.len() - 1].contains(&0),
        "type-5 PCK certificate chain is not canonical PEM plus NUL"
    );
    Ok(pck_chain.to_vec())
}

/// Remove the QPL ABI terminator while rejecting ambiguous text bytes.
pub(crate) fn canonical_text_component(value: &[u8], label: &str) -> Result<Vec<u8>> {
    ensure!(
        value.ends_with(b"\0")
            && !value.ends_with(b"\0\0")
            && !value[..value.len() - 1].contains(&0),
        "{label} is not terminated by exactly one NUL"
    );
    let canonical = &value[..value.len() - 1];
    ensure!(!canonical.is_empty(), "{label} is empty");
    std::str::from_utf8(canonical).map_err(|_| eyre!("{label} is not UTF-8"))?;
    Ok(canonical.to_vec())
}

/// Return one complete DER SEQUENCE, dropping only an ABI terminator.
pub(crate) fn canonical_der_component(value: &[u8], label: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len() >= 2 && value[0] == 0x30,
        "{label} is not a DER SEQUENCE"
    );
    let encoded_length = value[1];
    let (header_size, content_size) = if encoded_length < 0x80 {
        (2usize, usize::from(encoded_length))
    } else {
        let length_size = usize::from(encoded_length & 0x7F);
        ensure!(
            (1..=4).contains(&length_size) && value.len() >= 2 + length_size,
            "{label} has an invalid DER length"
        );
        let length_bytes = &value[2..2 + length_size];
        ensure!(
            length_bytes[0] != 0,
            "{label} has a non-canonical DER length"
        );
        let mut content_size = 0usize;
        for &byte in length_bytes {
            content_size = (content_size << 8) | usize::from(byte);
        }
        ensure!(
            content_size >= 0x80,
            "{label} has a non-canonical DER length"
        );
        (2 + length_size, content_size)
    };

    let object_size = header_size + content_size;
    if value.len() == object_size {
        return Ok(value.to_vec());
    }
    if value.len() == object_size + 1 && value[object_size] == 0 {
        return Ok(value[..object_size].to_vec());
    }
    bail!("{label} has trailing or truncated DER bytes")
}

/// Normalize legacy hex and size-delimited raw CRLs to canonical DER.
pub(crate) fn canonical_crl_component(
    value: &[u8],
    minor_version: u16,
    label: &str,
) -> Result<Vec<u8>> {
    ensure!(
        matches!(minor_version, 0 | 1),
        "{label} uses unsupported collateral minor version"
    );
    if value.starts_with(b"\x30") {
        return canonical_der_component(value, label);
    }
    ensure!(minor_version != 1, "{label} v3.1 payload is not raw DER");
    let encoded = value.strip_suffix(b"\0").unwrap_or(value);
    ensure!(
        !encoded.is_empty() && !encoded.contains(&0),
        "{label} contains an invalid NUL"
    );
    ensure!(
        encoded.len() % 2 == 0,
        "{label} contains odd-length hexadecimal DER"
    );
    ensure!(
        encoded.iter().all(u8::is_ascii_hexdigit),
        "{label} is not canonical hexadecimal DER"
    );
    let decoded =
        hex::decode(encoded).map_err(|_| eyre!("{label} is not canonical hexadecimal DER"))?;
    canonical_der_component(&decoded, label)
}

pub(crate) fn load_host_qpl_pin(repo: &Path) -> Result<(String, String)> {
    let pin_path = repo.join("release/project-toolchain-v1.json");
    let pin: Value = serde_json::from_slice(
        &fs::read(&pin_path).wrap_err_with(|| format!("read {}", pin_path.display()))?,
    )?;
    let packages = pin
        .get("host_only_packages")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("single project pin lacks the exact host-only QPL package"))?;
    ensure!(
        packages.len() == 1 && packages.contains_key("libsgx-dcap-default-qpl"),
        "single project pin lacks the exact host-only QPL package"
    );
    let version = packages["libsgx-dcap-default-qpl"]
        .as_str()
        .ok_or_else(|| eyre!("single project pin has an invalid host-only QPL version"))?;
    ensure!(
        !version.is_empty() && !version.chars().any(char::is_whitespace),
        "single project pin has an invalid host-only QPL version"
    );
    Ok(("libsgx-dcap-default-qpl".to_owned(), version.to_owned()))
}

fn is_rfc3339_seconds(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return false;
    }
    let seps_ok = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ]
    .into_iter()
    .all(|(idx, expected)| bytes[idx] == expected);
    let digits_ok = [0..4, 5..7, 8..10, 11..13, 14..16, 17..19]
        .into_iter()
        .all(|range| bytes[range].iter().all(u8::is_ascii_digit));
    seps_ok && digits_ok
}

pub(crate) fn crl_provenance(path: &Path, crl_type: &str) -> Result<Value> {
    ensure!(
        matches!(crl_type, "processor" | "platform" | "root"),
        "unsupported CRL type: {crl_type}"
    );
    let output = Command::new("openssl")
        .args(["crl", "-inform", "DER", "-in"])
        .arg(path)
        .args([
            "-noout",
            "-issuer",
            "-lastupdate",
            "-nextupdate",
            "-dateopt",
            "iso_8601",
        ])
        .output()
        .wrap_err("invoke openssl crl")?;
    ensure!(
        output.status.success(),
        "openssl crl failed with {}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout)?;
    let mut fields = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once('=') {
            fields.insert(key.to_owned(), value.trim().to_owned());
        }
    }
    let issuer = fields.get("issuer").cloned().unwrap_or_default();
    let this_update = fields
        .get("lastUpdate")
        .map(|s| s.replacen(' ', "T", 1))
        .unwrap_or_default();
    let next_update = fields
        .get("nextUpdate")
        .map(|s| s.replacen(' ', "T", 1))
        .unwrap_or_default();
    ensure!(
        !issuer.is_empty() && is_rfc3339_seconds(&this_update) && is_rfc3339_seconds(&next_update),
        "OpenSSL did not return canonical CRL issuer and validity dates"
    );
    Ok(json!({
        "issuer": issuer,
        "next_update": next_update,
        "sha256": sha256_file(path)?,
        "size": fs::metadata(path)?.len(),
        "this_update": this_update,
        "type": crl_type,
    }))
}

fn package_version(name: &str) -> Result<String> {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", name])
        .output()
        .wrap_err_with(|| format!("query dpkg version for {name}"))?;
    ensure!(
        output.status.success(),
        "dpkg-query failed for {name} with {}",
        output.status
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn library_provenance(path: &Path, package: &str, expected_version: Option<&str>) -> Result<Value> {
    let resolved = fs::canonicalize(path).wrap_err_with(|| format!("resolve {}", path.display()))?;
    let pkg_version = package_version(package)?;
    if let Some(expected) = expected_version {
        ensure!(
            pkg_version == expected,
            "host package {package} version {pkg_version:?} does not match the single project pin {expected:?}"
        );
    }
    Ok(json!({
        "package": package,
        "package_version": pkg_version,
        "path": resolved.display().to_string(),
        "size": fs::metadata(&resolved)?.len(),
        "sha256": sha256_file(&resolved)?,
    }))
}

unsafe fn bytes_at(pointer: *const u8, size: u32, label: &str) -> Result<Vec<u8>> {
    ensure!(!pointer.is_null() && size != 0, "QPL returned empty {label}");
    // SAFETY: `pointer` and `size` are returned by Intel QVL `tee_qv_get_collateral`.
    Ok(unsafe { std::slice::from_raw_parts(pointer, size as usize) }.to_vec())
}

fn capture_collateral(repo: &Path, quote: &[u8]) -> Result<(BTreeMap<String, Vec<u8>>, Value)> {
    let manifest_path = repo.join("release/dcap-native-qvl-v1.json");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let qvl_artifact = manifest["artifacts"]
        .as_array()
        .and_then(|arr| arr.iter().find(|item| item["role"] == "qvl"))
        .ok_or_else(|| eyre!("native manifest has no qvl artifact"))?;
    let qvl_path = fs::canonicalize(
        qvl_artifact["path"]
            .as_str()
            .ok_or_else(|| eyre!("missing qvl path"))?,
    )?;
    ensure!(
        sha256_file(&qvl_path)?
            == qvl_artifact["sha256"]
                .as_str()
                .ok_or_else(|| eyre!("missing qvl sha256"))?,
        "installed QVL digest does not match release/dcap-native-qvl-v1.json"
    );

    let c_path = CString::new(qvl_path.as_os_str().as_bytes())?;
    let get_sym = CString::new("tee_qv_get_collateral")?;
    let free_sym = CString::new("tee_qv_free_collateral")?;

    // SAFETY: Dynamically loads the SHA-256-verified Intel QVL shared object and resolves its documented C entrypoints.
    unsafe {
        let handle = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        ensure!(
            !handle.is_null(),
            "dlopen failed for {}",
            qvl_path.display()
        );
        let get_ptr = libc::dlsym(handle, get_sym.as_ptr());
        let free_ptr = libc::dlsym(handle, free_sym.as_ptr());
        if get_ptr.is_null() || free_ptr.is_null() {
            libc::dlclose(handle);
            bail!("missing tee_qv_get_collateral / tee_qv_free_collateral in QVL library");
        }
        let get_collateral: TeeQvGetCollateralFn = std::mem::transmute(get_ptr);
        let free_collateral: TeeQvFreeCollateralFn = std::mem::transmute(free_ptr);

        let mut collateral_ptr: *mut QuoteCollateral = std::ptr::null_mut();
        let mut collateral_size: u32 = 0;
        let status = get_collateral(
            quote.as_ptr(),
            u32::try_from(quote.len())?,
            &mut collateral_ptr,
            &mut collateral_size,
        );
        if status != 0 {
            libc::dlclose(handle);
            bail!("tee_qv_get_collateral failed with quote3_error_t 0x{status:04x}");
        }

        let result = (|| -> Result<(BTreeMap<String, Vec<u8>>, Value)> {
            ensure!(
                (collateral_size as usize) >= std::mem::size_of::<QuoteCollateral>(),
                "QPL collateral allocation is smaller than its ABI header"
            );
            let collateral = &*collateral_ptr;
            let parts = collateral.version.parts;
            let expected = &manifest["intel_dcap"];
            let expected_major = u16::try_from(
                expected["collateral_major_version"]
                    .as_u64()
                    .ok_or_else(|| eyre!("missing collateral_major_version"))?,
            )?;
            ensure!(
                parts.major_version == expected_major
                    && matches!(parts.minor_version, 0 | 1)
                    && collateral.tee_type == 0,
                "QPL collateral ABI/profile does not match the pinned SGX contract"
            );

            let raw_text = [
                (
                    "pck-crl-issuer-chain.pem",
                    bytes_at(
                        collateral.pck_crl_issuer_chain,
                        collateral.pck_crl_issuer_chain_size,
                        "PCK CRL issuer chain",
                    )?,
                ),
                (
                    "tcb-info-issuer-chain.pem",
                    bytes_at(
                        collateral.tcb_info_issuer_chain,
                        collateral.tcb_info_issuer_chain_size,
                        "TCB Info issuer chain",
                    )?,
                ),
                (
                    "tcb-info.json",
                    bytes_at(collateral.tcb_info, collateral.tcb_info_size, "TCB Info")?,
                ),
                (
                    "qe-identity-issuer-chain.pem",
                    bytes_at(
                        collateral.qe_identity_issuer_chain,
                        collateral.qe_identity_issuer_chain_size,
                        "QE Identity issuer chain",
                    )?,
                ),
                (
                    "qe-identity.json",
                    bytes_at(
                        collateral.qe_identity,
                        collateral.qe_identity_size,
                        "QE Identity",
                    )?,
                ),
            ];

            let mut components = BTreeMap::new();
            components.insert(
                "pck-certificate-chain.pem0".to_owned(),
                extract_type5_pck_chain(quote)?,
            );
            let pck_crl_raw = bytes_at(collateral.pck_crl, collateral.pck_crl_size, "PCK CRL")?;
            let root_crl_raw = bytes_at(
                collateral.root_ca_crl,
                collateral.root_ca_crl_size,
                "root CA CRL",
            )?;
            components.insert(
                PCK_CRL_DER.to_owned(),
                canonical_crl_component(&pck_crl_raw, parts.minor_version, "PCK CRL")?,
            );
            components.insert(
                ROOT_CA_CRL_DER.to_owned(),
                canonical_crl_component(&root_crl_raw, parts.minor_version, "root CA CRL")?,
            );
            for (name, raw) in raw_text {
                components.insert(name.to_owned(), canonical_text_component(&raw, name)?);
            }

            let (qpl_pkg, qpl_ver) = load_host_qpl_pin(repo)?;
            let qvl_pkg = qvl_artifact["package"]
                .as_str()
                .ok_or_else(|| eyre!("missing qvl package"))?;
            let provenance = json!({
                "collateral_abi": {
                    "major_version": parts.major_version,
                    "minor_version": parts.minor_version,
                    "tee_type": collateral.tee_type,
                    "normalized_minor_version": expected["collateral_minor_version"],
                    "allocation_size": collateral_size,
                },
                "qvl": library_provenance(&qvl_path, qvl_pkg, None)?,
                "qpl": library_provenance(
                    Path::new("/usr/lib/x86_64-linux-gnu/libdcap_quoteprov.so.1"),
                    &qpl_pkg,
                    Some(&qpl_ver),
                )?,
                "qcnl": library_provenance(
                    Path::new("/usr/lib/x86_64-linux-gnu/libsgx_default_qcnl_wrapper.so.1"),
                    &qpl_pkg,
                    Some(&qpl_ver),
                )?,
            });
            Ok((components, provenance))
        })();

        let free_status = free_collateral(collateral_ptr);
        libc::dlclose(handle);
        ensure!(
            free_status == 0,
            "tee_qv_free_collateral failed with quote3_error_t 0x{free_status:04x}"
        );
        result
    }
}

/// Capture exact Intel DCAP collateral for one real SGX quote and write `output_dir`.
pub(crate) fn write_capture(
    repo: &Path,
    quote_path: &Path,
    output_dir: &Path,
    expected_pck_ca: &str,
) -> Result<()> {
    let capture_started_at = unix_seconds()?;
    let quote = fs::read(quote_path)?;
    let (components, mut provenance) = capture_collateral(repo, &quote)?;
    ensure!(
        !output_dir.exists(),
        "collateral output directory already exists: {}",
        output_dir.display()
    );
    fs::create_dir_all(output_dir)?;
    for (name, value) in &components {
        fs::write(output_dir.join(name), value)?;
    }

    let components_meta: BTreeMap<String, Value> = components
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                json!({"size": value.len(), "sha256": sha256_bytes(value)}),
            )
        })
        .collect();
    let openssl_out = Command::new("openssl").arg("version").output()?;
    ensure!(openssl_out.status.success(), "openssl version failed");
    let openssl_version = String::from_utf8(openssl_out.stdout)?.trim().to_owned();
    let completed_at = unix_seconds()?;

    let obj = provenance
        .as_object_mut()
        .ok_or_else(|| eyre!("provenance must be an object"))?;
    obj.insert(
        "quote".to_owned(),
        json!({
            "path": quote_path.file_name().and_then(|n| n.to_str()).unwrap_or("quote-v3.bin"),
            "size": quote.len(),
            "sha256": sha256_bytes(&quote),
        }),
    );
    obj.insert("components".to_owned(), json!(components_meta));
    obj.insert(
        "crls".to_owned(),
        json!({
            "pck": crl_provenance(&output_dir.join(PCK_CRL_DER), expected_pck_ca)?,
            "root": crl_provenance(&output_dir.join(ROOT_CA_CRL_DER), "root")?,
        }),
    );
    obj.insert(
        "capture".to_owned(),
        json!({
            "completed_at": completed_at,
            "started_at": capture_started_at,
        }),
    );
    obj.insert("openssl_version".to_owned(), Value::String(openssl_version));

    let mut formatted = serde_json::to_string_pretty(&provenance)?;
    formatted.push('\n');
    fs::write(output_dir.join("capture-provenance.json"), formatted)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root")
    }

    #[test]
    fn extracts_the_exact_type_five_pck_chain_and_rejects_trailing_bytes() {
        let quote = fs::read(
            repo_root().join("crates/system/tee/tests/fixtures/intel-dcap-1.26/sgx-processor-quote-v3.bin"),
        )
        .expect("read quote fixture");
        let pck_chain = extract_type5_pck_chain(&quote).expect("extract type-5 PCK chain");
        assert!(pck_chain.starts_with(b"-----BEGIN CERTIFICATE-----\n"));
        assert!(pck_chain.ends_with(b"\0"));
        assert!(!pck_chain[..pck_chain.len() - 1].contains(&0));
        let text = std::str::from_utf8(&pck_chain[..pck_chain.len() - 1]).expect("utf8");
        assert_eq!(text.matches("-----BEGIN CERTIFICATE-----").count(), 3);

        let mut trailing = quote;
        trailing.push(0xa5);
        let err = extract_type5_pck_chain(&trailing).unwrap_err();
        assert!(err.to_string().contains("trailing"));
    }

    #[test]
    fn text_collateral_requires_one_terminal_nul_and_host_qpl_matches_pin() {
        assert_eq!(
            canonical_text_component(b"{\"value\":1}\0", "test").unwrap(),
            b"{\"value\":1}"
        );
        for malformed in [
            b"{\"value\":1}".as_slice(),
            b"{\"value\":\0}\0".as_slice(),
            b"{\"value\":1}\0\0".as_slice(),
        ] {
            assert!(canonical_text_component(malformed, "test").is_err());
        }

        let (pkg, ver) = load_host_qpl_pin(&repo_root()).unwrap();
        assert_eq!(pkg, "libsgx-dcap-default-qpl");
        assert_eq!(ver, "1.26.100.1-noble1");
    }

    #[test]
    fn crl_provenance_records_issuer_type_and_validity() {
        let crl_path = repo_root().join(
            "crates/system/tee/tests/fixtures/intel-dcap-1.26-intent-bound-processor/pck.crl.der",
        );
        let meta = crl_provenance(&crl_path, "processor").expect("crl_provenance");
        assert_eq!(meta["type"], "processor");
        assert!(meta["issuer"]
            .as_str()
            .unwrap()
            .contains("Intel SGX PCK Processor CA"));
        assert_eq!(meta["this_update"], "2026-08-26T23:46:46Z");
        assert_eq!(meta["next_update"], "2026-09-25T23:46:46Z");
        assert!(meta["size"].as_u64().unwrap() > 0);
        assert_eq!(meta["sha256"].as_str().unwrap().len(), 64);
    }

    #[test]
    fn crl_versions_normalize_to_raw_der_and_reject_malformed_encodings() {
        let raw_der = hex::decode("3003020101").unwrap();
        let mut hex_with_nul = hex::encode(&raw_der).into_bytes();
        hex_with_nul.push(0);
        assert_eq!(
            canonical_crl_component(&hex_with_nul, 0, "test CRL").unwrap(),
            raw_der
        );

        let mut raw_with_nul = raw_der.clone();
        raw_with_nul.push(0);
        assert_eq!(
            canonical_crl_component(&raw_with_nul, 0, "test CRL").unwrap(),
            raw_der
        );
        assert_eq!(
            canonical_crl_component(&raw_der, 1, "test CRL").unwrap(),
            raw_der
        );

        let mut double_nul = raw_der.clone();
        double_nul.extend_from_slice(b"\0\0");
        let mut trailing_x = raw_der;
        trailing_x.push(b'x');
        for (value, minor) in [
            (b"300\0".as_slice(), 0),
            (b"zz\0".as_slice(), 0),
            (b"30 03 02 01 01\0".as_slice(), 0),
            (double_nul.as_slice(), 0),
            (trailing_x.as_slice(), 1),
            (b"3003020101\0".as_slice(), 1),
            (b"\x30\x00".as_slice(), 2),
        ] {
            assert!(canonical_crl_component(value, minor, "test CRL").is_err());
        }
    }
}
